//! mpv as the player, driven over its JSON IPC socket.
//!
//! pikap makes no sound itself, on YouTube Music as on Spotify: mpv plays, pikap is the
//! remote. Unlike Spotify.app, mpv has no window to stop it from, so it lives with
//! pikap: quitting pikap quits mpv, and mpv shares pikap's process group, so closing
//! the terminal pane ends both. Should pikap die without that, the next pikap finds
//! the same mpv at the same socket and takes over.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;

const START_WAIT: Duration = Duration::from_secs(5);

/// What mpv reports on its own.
#[derive(Debug, PartialEq)]
pub enum Event {
    /// A file stopped: `eof` when it played to the end, `stop` when replaced, `error`…
    EndFile(String),
    /// The socket closed: mpv quit.
    Closed,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;

pub struct Mpv {
    writer: tokio::sync::Mutex<OwnedWriteHalf>,
    pending: Pending,
    next: AtomicU64,
}

/// macOS caps socket paths at 104 bytes, so the socket lives in /tmp, one per user.
pub fn socket_path() -> PathBuf {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/tmp/pikap-mpv-{uid}.sock"))
}

impl Mpv {
    /// Connects to the running player, starting one when there is none. The flag says
    /// whether it was already running.
    pub async fn connect(socket: &Path) -> Result<(Arc<Self>, UnboundedReceiver<Event>, bool)> {
        if let Ok(stream) = UnixStream::connect(socket).await {
            let (mpv, events) = Self::attach(stream);
            return Ok((mpv, events, true));
        }
        let _ = std::fs::remove_file(socket);
        let mut command = tokio::process::Command::new("mpv");
        command
            .args([
                "--idle=yes",
                "--no-video",
                "--no-terminal",
                "--force-window=no",
                "--audio-display=no",
                "--ytdl-format=bestaudio/best",
            ])
            .arg(format!("--input-ipc-server={}", socket.display()))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command
            .spawn()
            .context("mpv is not installed (brew install mpv)")?;
        let start = Instant::now();
        loop {
            if let Ok(stream) = UnixStream::connect(socket).await {
                let (mpv, events) = Self::attach(stream);
                return Ok((mpv, events, false));
            }
            if start.elapsed() > START_WAIT {
                bail!("mpv started but opened no socket at {}", socket.display());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn attach(stream: UnixStream) -> (Arc<Self>, UnboundedReceiver<Event>) {
        let (read, write) = stream.into_split();
        let pending: Pending = Arc::default();
        let (events_tx, events) = unbounded_channel();
        tokio::spawn(read_loop(BufReader::new(read), pending.clone(), events_tx));
        let mpv = Arc::new(Self {
            writer: tokio::sync::Mutex::new(write),
            pending,
            next: AtomicU64::new(1),
        });
        (mpv, events)
    }

    pub async fn command(&self, args: Value) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| anyhow!("mpv state poisoned"))?
            .insert(id, tx);
        let line = json!({ "command": args, "request_id": id }).to_string() + "\n";
        self.writer.lock().await.write_all(line.as_bytes()).await?;
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .context("mpv did not answer")?
            .context("mpv closed")?
    }

    pub async fn get(&self, property: &str) -> Result<Value> {
        self.command(json!(["get_property", property])).await
    }

    pub async fn set(&self, property: &str, value: Value) -> Result<()> {
        self.command(json!(["set_property", property, value]))
            .await
            .map(drop)
    }
}

async fn read_loop(
    mut reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    pending: Pending,
    events: UnboundedSender<Event>,
) {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(id) = message["request_id"].as_u64() {
            let waiter = pending.lock().ok().and_then(|mut p| p.remove(&id));
            if let Some(waiter) = waiter {
                let answer = match message["error"].as_str() {
                    Some("success") => Ok(message["data"].clone()),
                    Some(error) => Err(anyhow!("mpv: {error}")),
                    None => Err(anyhow!("mpv: malformed answer")),
                };
                let _ = waiter.send(answer);
            }
        } else if message["event"] == "end-file" {
            let reason = message["reason"].as_str().unwrap_or("").to_owned();
            let _ = events.send(Event::EndFile(reason));
        }
    }
    let _ = events.send(Event::Closed);
}
