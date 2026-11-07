//! The YouTube Music backend: answers the UI's [`Command`]s with [`Update`]s, like the
//! Spotify worker, but plays through mpv and keeps the queue itself.
//!
//! mpv plays one file at a time. pikap holds the queue, resolves each track's stream
//! address with yt-dlp (several seconds, so the next tracks are resolved ahead) and
//! loads the next one when mpv reports the end of a file. The queue is saved, so a new
//! pikap picks up where the last one left off, on the same mpv.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::client::{self, Client};
use super::mpv::{self, Event, Mpv};
use super::parse::{self, Song};
use super::{LIKED, id, playlist_uri};
use crate::backend::{
    AlbumPage, ArtistPage, Command, Device, ListPage, PlayContext, Playback, Update,
};
use crate::data::{Kind, LibraryIndex, NowPlaying, SearchResults, Track};
use crate::log::plog;
use crate::spotify::config;

/// How often the playback state goes to the screen; mpv is local, so this is free.
const TICK: Duration = Duration::from_secs(1);
/// Stream addresses stay valid for about six hours.
const URL_LIFE: Duration = Duration::from_secs(4 * 3600);
/// Tracks resolved ahead of the playing one.
const PREFETCH: usize = 2;
const COVER_PX: u32 = 640;
const MAX_LIST: usize = 1000;
const QUEUE_SENT: usize = 50;
/// "Previous" restarts the track once it has played this long.
const RESTART_AFTER: f64 = 3.0;
const MADE_FOR_YOU: usize = 16;

// YouTube Music's own search filter chips.
const SONGS: &str = "EgWKAQIIAWoMEAMQBBAJEA4QChAF";
const ALBUMS: &str = "EgWKAQIYAWoMEAMQBBAJEA4QChAF";
const ARTISTS: &str = "EgWKAQIgAWoMEAMQBBAJEA4QChAF";
const PLAYLISTS: &str = "EgeKAQQoAEABagwQAxAEEAkQDhAKEAU";

#[derive(Clone, Default, Serialize, Deserialize)]
struct Queue {
    songs: Vec<Song>,
    index: usize,
    /// (uri, kind) of the list the queue came from.
    context: Option<(String, String)>,
    shuffle: bool,
    /// 0 off, 1 the whole queue, 2 the track.
    repeat: u8,
    /// The order before shuffling, to go back to.
    #[serde(skip)]
    unshuffled: Option<Vec<Song>>,
    /// Tracks added with "add to queue" since the track changed; they line up in order.
    #[serde(skip)]
    added: usize,
    /// Where the current track stood when pikap closed, in seconds.
    #[serde(default)]
    position: f64,
}

impl Queue {
    fn current(&self) -> Option<&Song> {
        self.songs.get(self.index)
    }

    fn upcoming(&self) -> Vec<Track> {
        self.songs
            .iter()
            .skip(self.index + 1)
            .take(QUEUE_SENT)
            .map(|s| Track::from(&s.info))
            .collect()
    }
}

/// A list's page with its tracks whole; an artist or album page with the extra id
/// or image the page needs.
type Lists = HashMap<String, (Arc<ListPage>, Vec<Song>)>;
type Artists = HashMap<String, (Arc<ArtistPage>, Option<String>)>;
type Albums = HashMap<String, (Arc<AlbumPage>, Option<String>)>;

struct State {
    ui: Sender<Update>,
    cookie: Option<String>,
    /// The cookie as a Netscape file, for yt-dlp.
    cookie_file: Option<PathBuf>,
    http: reqwest::Client,
    client: tokio::sync::Mutex<Option<Arc<Client>>>,
    mpv: tokio::sync::Mutex<Option<Arc<Mpv>>>,
    events: UnboundedSender<Event>,
    queue: Mutex<Queue>,
    /// The screen shows the other service: no playback reports.
    resting: std::sync::atomic::AtomicBool,
    /// pikap is closing: nothing may start mpv again.
    quitting: std::sync::atomic::AtomicBool,
    /// A saved track and where to start it, used by the first play after startup.
    resume_at: Mutex<Option<(String, f64)>>,
    /// The track asked for last; its stream may still be resolving.
    loaded: Mutex<Option<String>>,
    /// The track whose file mpv actually has open.
    in_mpv: Mutex<Option<String>>,
    urls: Mutex<HashMap<String, (String, Instant)>>,
    /// One resolution per track at a time: a second, simultaneous one makes YouTube
    /// refuse the address the first one returned.
    resolving: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// The track already given a fresh address after mpv could not open it.
    retried: Mutex<Option<String>>,
    /// Every track seen, by URI, for commands that only name one.
    songs: Mutex<HashMap<String, Song>>,
    lists: Mutex<Lists>,
    artists: Mutex<Artists>,
    albums: Mutex<Albums>,
    liked: Mutex<HashSet<String>>,
}

pub async fn run(mut commands: UnboundedReceiver<crate::backend::Command>, ui: Sender<Update>) {
    let cookie = client::read_cookie();
    let cookie_file = cookie.as_deref().and_then(|c| match write_cookie_file(c) {
        Ok(path) => Some(path),
        Err(err) => {
            plog!("ytmusic cookie file: {err:#}");
            None
        }
    });
    let (events, mut event_rx) = unbounded_channel();
    let state = Arc::new(State {
        ui: ui.clone(),
        cookie,
        cookie_file,
        http: reqwest::Client::builder()
            .user_agent(concat!("pikap/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_default(),
        client: tokio::sync::Mutex::new(None),
        mpv: tokio::sync::Mutex::new(None),
        events,
        queue: Mutex::new(Queue::default()),
        quitting: std::sync::atomic::AtomicBool::new(false),
        resting: std::sync::atomic::AtomicBool::new(false),
        resume_at: Mutex::new(None),
        loaded: Mutex::new(None),
        in_mpv: Mutex::new(None),
        urls: Mutex::default(),
        resolving: Mutex::default(),
        retried: Mutex::new(None),
        songs: Mutex::default(),
        lists: Mutex::default(),
        artists: Mutex::default(),
        albums: Mutex::default(),
        liked: Mutex::default(),
    });
    let _ = ui.send(Update::Meta(true));
    state.restore().await;
    {
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(err) = state.load_library().await {
                state.report(&err);
            }
        });
    }

    let mut tick = tokio::time::interval(TICK);
    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break };
                if let Command::Active(on) = command {
                    state.resting.store(!on, std::sync::atomic::Ordering::Relaxed);
                    if on {
                        state.send_queue();
                        state.send_playback().await;
                    }
                    continue;
                }
                state.handle(command).await;
                if state.quitting.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
            }
            Some(event) = event_rx.recv() => state.on_event(event).await,
            _ = tick.tick() => {
                if !state.resting.load(std::sync::atomic::Ordering::Relaxed) {
                    state.send_playback().await;
                }
            }
        }
    }
}

impl State {
    fn report(&self, err: &anyhow::Error) {
        plog!("ytmusic: {err:#}");
        let _ = self.ui.send(Update::Error(format!("{err:#}")));
    }

    async fn client(&self) -> Result<Arc<Client>> {
        let mut client = self.client.lock().await;
        if let Some(c) = client.as_ref() {
            return Ok(c.clone());
        }
        let fresh = Arc::new(Client::connect(self.cookie.as_deref()).await?);
        plog!("ytmusic: connected, signed in: {}", fresh.signed_in());
        *client = Some(fresh.clone());
        Ok(fresh)
    }

    fn signed_in(&self) -> bool {
        self.cookie.is_some()
    }

    /// The player, started or found again; `true` when it was already running.
    async fn mpv(&self) -> Result<(Arc<Mpv>, bool)> {
        if self.quitting.load(std::sync::atomic::Ordering::Relaxed) {
            bail!("pikap is closing");
        }
        let mut mpv = self.mpv.lock().await;
        if let Some(m) = mpv.as_ref() {
            return Ok((m.clone(), true));
        }
        let (fresh, mut events, running) = Mpv::connect(&mpv::socket_path()).await?;
        let forward = self.events.clone();
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if forward.send(event).is_err() {
                    break;
                }
            }
        });
        *mpv = Some(fresh.clone());
        Ok((fresh, running))
    }

    fn remember(&self, songs: &[Song]) {
        if let Ok(mut known) = self.songs.lock() {
            for s in songs {
                known.insert(s.info.uri.clone(), s.clone());
            }
        }
    }

    fn known(&self, uri: &str) -> Option<Song> {
        self.songs.lock().ok()?.get(uri).cloned()
    }

    // ---- commands ------------------------------------------------------------------

    async fn handle(self: &Arc<Self>, command: Command) {
        let result = match command {
            Command::SetPlaying(true) => self.resume(None).await,
            Command::SetPlaying(false) => self.set("pause", json!(true)).await,
            Command::Next => self.step(1).await,
            Command::Previous => self.previous().await,
            Command::SkipAhead(n) => self.step(n as isize).await,
            Command::SeekTo(ms) => self.set("time-pos", json!(ms as f64 / 1000.0)).await,
            Command::Volume(v) => self.set("volume", json!(v)).await,
            Command::SetShuffle(on) => {
                self.shuffle(on);
                self.send_queue();
                Ok(())
            }
            Command::SetRepeat(state) => {
                if let Ok(mut q) = self.queue.lock() {
                    q.repeat = match state {
                        "context" => 1,
                        "track" => 2,
                        _ => 0,
                    };
                }
                self.save();
                Ok(())
            }
            Command::LoadQueue => {
                self.send_queue();
                Ok(())
            }
            Command::LoadDevices => {
                let volume = self.get_f64("volume").await.map(|v| v.round() as u8);
                let _ = self.ui.send(Update::Devices(vec![Device {
                    id: Some("mpv".into()),
                    name: "mpv".into(),
                    kind: "Computer".into(),
                    volume_percent: volume,
                    is_active: true,
                    is_restricted: false,
                }]));
                Ok(())
            }
            Command::Transfer { .. } => Ok(()),
            Command::Quit(done) => {
                self.quit().await;
                let _ = done.send(());
                Ok(())
            }
            Command::AddToQueue(track) => {
                self.add_to_queue(&track);
                Ok(())
            }
            other => {
                // Anything that reads the network runs on its own.
                let state = self.clone();
                tokio::spawn(async move {
                    if let Err(err) = state.handle_slow(other).await {
                        state.report(&err);
                    }
                    state.send_playback().await;
                });
                Ok(())
            }
        };
        if let Err(err) = result {
            self.report(&err);
        }
        self.send_playback().await;
    }

    async fn handle_slow(self: &Arc<Self>, command: Command) -> Result<()> {
        match command {
            Command::PlayTrack(uri) | Command::PlayRecent(uri) => self.play_track(&uri).await,
            Command::PlayInContext { context, track } => self.play_in(&context, Some(&track)).await,
            Command::PlayContext(context) => self.play_in(&context, None).await,
            Command::PlayFrom { context, uris } => match (context, uris.first()) {
                (Some(context), Some(first)) => self.play_in(&context, Some(first)).await,
                _ => {
                    let songs: Vec<Song> = uris.iter().filter_map(|u| self.known(u)).collect();
                    self.play_queue(songs, 0, None);
                    Ok(())
                }
            },
            Command::Resume { fallback } => self.resume(fallback).await,
            Command::LoadRadio(seed) => {
                let (page, _) = self.radio(&seed).await?;
                let _ = self.ui.send(Update::Radio(seed, page));
                Ok(())
            }
            Command::StartRadio(seed) => {
                let (page, songs) = self.radio(&seed).await?;
                let context = (page.uri.clone(), "playlist".to_owned());
                let _ = self.ui.send(Update::Radio(seed, page.clone()));
                let _ = self.ui.send(Update::Context(page));
                self.play_queue(songs, 0, Some(context));
                Ok(())
            }
            Command::OpenList(Kind::Artist, uri) => {
                self.artist_page(&uri).await;
                Ok(())
            }
            Command::OpenList(kind, uri) => {
                let (page, _) = self.list(kind, &uri).await?;
                let _ = self.ui.send(Update::List(page));
                Ok(())
            }
            Command::Search(query) => {
                let found = self.search(&query).await?;
                let _ = self.ui.send(Update::Search(Arc::new(found)));
                Ok(())
            }
            Command::SetLiked(uri, liked) => {
                let result = self.set_liked(&uri, liked).await;
                if result.is_err() {
                    // The heart was set ahead of the answer; put it back.
                    let _ = self.ui.send(Update::Liked(uri, !liked));
                }
                result
            }
            Command::AddToPlaylist { playlist, track } => {
                self.add_to_playlist(&playlist, &track).await
            }
            Command::RemoveFromPlaylist { .. } => {
                bail!("removing from a playlist is not there yet on YouTube Music")
            }
            _ => Ok(()),
        }
    }

    async fn set(&self, property: &str, value: Value) -> Result<()> {
        self.mpv().await?.0.set(property, value).await
    }

    async fn get_f64(&self, property: &str) -> Option<f64> {
        let (mpv, _) = self.mpv().await.ok()?;
        mpv.get(property).await.ok()?.as_f64()
    }

    async fn get_bool(&self, property: &str) -> Option<bool> {
        let (mpv, _) = self.mpv().await.ok()?;
        mpv.get(property).await.ok()?.as_bool()
    }

    /// Plays on: unpauses what is loaded, reloads the saved queue into an idle player,
    /// or starts the fallback (context, track).
    async fn resume(self: &Arc<Self>, fallback: Option<(Option<String>, String)>) -> Result<()> {
        let idle = self.get_bool("idle-active").await.unwrap_or(true);
        let has_queue = self.queue.lock().is_ok_and(|q| q.current().is_some());
        if !idle {
            return self.set("pause", json!(false)).await;
        }
        if has_queue {
            self.play_current();
            return Ok(());
        }
        match fallback {
            Some((Some(context), track)) => self.play_in(&context, Some(&track)).await,
            Some((None, track)) => self.play_track(&track).await,
            None => Ok(()),
        }
    }

    async fn previous(self: &Arc<Self>) -> Result<()> {
        if self.get_f64("time-pos").await.unwrap_or(0.0) > RESTART_AFTER {
            return self.set("time-pos", json!(0.0)).await;
        }
        self.step(-1).await
    }

    async fn step(self: &Arc<Self>, by: isize) -> Result<()> {
        let moved = match self.queue.lock() {
            Ok(mut q) if !q.songs.is_empty() => {
                let last = q.songs.len() as isize - 1;
                let next = q.index as isize + by;
                let next = if next > last && q.repeat == 1 {
                    0
                } else {
                    next.clamp(0, last)
                };
                let moved = next as usize != q.index;
                q.index = next as usize;
                q.added = 0;
                moved
            }
            _ => false,
        };
        if moved {
            self.play_current();
        } else if by > 0 {
            self.extend_with_radio().await?;
        }
        Ok(())
    }

    /// A track played to its end.
    async fn on_event(self: &Arc<Self>, event: Event) {
        match event {
            Event::EndFile(reason) if reason == "eof" => {
                let repeat_track = self.queue.lock().is_ok_and(|q| q.repeat == 2);
                let result = if repeat_track {
                    self.play_current();
                    Ok(())
                } else {
                    self.step(1).await
                };
                if let Err(err) = result {
                    self.report(&err);
                }
            }
            Event::EndFile(reason) if reason == "error" => {
                // Addresses expire or get refused (403): try a fresh one once, then skip.
                let Some(uri) = self.in_mpv.lock().ok().and_then(|l| l.clone()) else {
                    return;
                };
                let first_time = self
                    .retried
                    .lock()
                    .map(|mut r| r.replace(uri.clone()).as_deref() != Some(uri.as_str()))
                    .unwrap_or(false);
                if let Ok(mut urls) = self.urls.lock() {
                    urls.remove(&uri);
                }
                if first_time {
                    plog!("ytmusic: mpv could not open {uri}, resolving it again");
                    self.play_current();
                } else {
                    plog!("ytmusic: mpv could not play {uri}, skipping");
                    let _ = self.step(1).await;
                }
            }
            Event::Closed => {
                plog!("ytmusic: mpv quit");
                *self.mpv.lock().await = None;
                *self.loaded.lock().unwrap_or_else(|e| e.into_inner()) = None;
                *self.in_mpv.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
            Event::EndFile(_) => {}
        }
        self.send_playback().await;
    }

    // ---- the queue -----------------------------------------------------------------

    fn play_queue(
        self: &Arc<Self>,
        songs: Vec<Song>,
        index: usize,
        context: Option<(String, String)>,
    ) {
        if songs.is_empty() {
            return;
        }
        self.remember(&songs);
        if let Ok(mut q) = self.queue.lock() {
            let shuffle = q.shuffle;
            *q = Queue {
                index: index.min(songs.len() - 1),
                songs,
                context,
                shuffle: false,
                repeat: q.repeat,
                unshuffled: None,
                added: 0,
                position: 0.0,
            };
            if shuffle {
                drop(q);
                self.shuffle(true);
            }
        }
        self.play_current();
    }

    /// Shows the current track at once, then resolves and loads it in the background;
    /// a later call wins if the user moves on meanwhile.
    fn play_current(self: &Arc<Self>) {
        let Some(song) = self.queue.lock().ok().and_then(|q| q.current().cloned()) else {
            return;
        };
        let uri = song.info.uri.clone();
        *self.loaded.lock().unwrap_or_else(|e| e.into_inner()) = Some(uri.clone());
        self.save();
        self.send_queue();
        self.follow(&song);
        let state = self.clone();
        tokio::spawn(async move {
            // The old track must not play on (or speak for the new one) meanwhile.
            let stale = state
                .in_mpv
                .lock()
                .ok()
                .and_then(|mut m| m.take_if(|m| *m != uri));
            if stale.is_some()
                && let Ok((mpv, _)) = state.mpv().await
            {
                let _ = mpv.command(json!(["stop"])).await;
            }
            state.send_playback().await;
            let url = match state.stream_url(&uri).await {
                Ok(url) => url,
                Err(err) => {
                    // Not loading any more; the track shows as stopped.
                    if let Ok(mut loaded) = state.loaded.lock() {
                        loaded.take_if(|l| *l == uri);
                    }
                    state.send_playback().await;
                    return state.report(&err);
                }
            };
            let still = state
                .loaded
                .lock()
                .is_ok_and(|l| l.as_deref() == Some(uri.as_str()));
            if !still {
                return;
            }
            let start = state
                .resume_at
                .lock()
                .ok()
                .and_then(|mut r| r.take())
                .filter(|(saved, at)| *saved == uri && *at > 1.0)
                .map(|(_, at)| at);
            let loaded = async {
                let (mpv, _) = state.mpv().await?;
                let load = match start {
                    Some(at) => json!(["loadfile", url, "replace", -1, format!("start={at:.1}")]),
                    None => json!(["loadfile", url, "replace"]),
                };
                if mpv.command(load).await.is_err() {
                    // mpv before 0.38 takes no index argument; start from the top.
                    mpv.command(json!(["loadfile", url, "replace"])).await?;
                }
                mpv.set("pause", json!(false)).await
            };
            if let Err(err) = loaded.await {
                return state.report(&err);
            }
            *state.in_mpv.lock().unwrap_or_else(|e| e.into_inner()) = Some(uri.clone());
            state.send_playback().await;
            state.prefetch();
        });
    }

    fn prefetch(self: &Arc<Self>) {
        let next: Vec<String> = self
            .queue
            .lock()
            .map(|q| {
                q.songs
                    .iter()
                    .skip(q.index + 1)
                    .take(PREFETCH)
                    .map(|s| s.info.uri.clone())
                    .collect()
            })
            .unwrap_or_default();
        for uri in next {
            let state = self.clone();
            tokio::spawn(async move {
                if let Err(err) = state.stream_url(&uri).await {
                    plog!("ytmusic prefetch {uri}: {err:#}");
                }
            });
        }
    }

    /// The end of the queue: YouTube Music plays on with the last track's radio.
    async fn extend_with_radio(self: &Arc<Self>) -> Result<()> {
        let Some(seed) = self
            .queue
            .lock()
            .ok()
            .and_then(|q| q.current().map(|s| s.info.uri.clone()))
        else {
            return Ok(());
        };
        let (_, songs) = self.radio(&seed).await?;
        self.remember(&songs);
        let grew = match self.queue.lock() {
            Ok(mut q) => {
                let have: HashSet<String> = q.songs.iter().map(|s| s.info.uri.clone()).collect();
                let before = q.songs.len();
                q.songs
                    .extend(songs.into_iter().filter(|s| !have.contains(&s.info.uri)));
                let grew = q.songs.len() > before;
                if grew {
                    q.index += 1;
                    q.added = 0;
                }
                grew
            }
            Err(_) => false,
        };
        if grew {
            self.play_current();
        }
        Ok(())
    }

    fn shuffle(&self, on: bool) {
        let Ok(mut q) = self.queue.lock() else { return };
        if on == q.shuffle {
            return;
        }
        q.shuffle = on;
        if on {
            q.unshuffled = Some(q.songs.clone());
            let start = (q.index + 1).min(q.songs.len());
            let mut seed = Instant::now().elapsed().as_nanos() as u64
                ^ std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0x9E37_79B9);
            let rest = &mut q.songs[start..];
            for i in (1..rest.len()).rev() {
                // xorshift: good enough to deal a queue.
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                rest.swap(i, (seed % (i as u64 + 1)) as usize);
            }
        } else if let Some(original) = q.unshuffled.take() {
            let current = q.current().map(|s| s.info.uri.clone());
            q.index = current
                .and_then(|c| original.iter().position(|s| s.info.uri == c))
                .unwrap_or(0);
            q.songs = original;
        }
        drop(q);
        self.save();
    }

    fn add_to_queue(&self, uri: &str) {
        let Some(song) = self.known(uri) else {
            return self.report(&anyhow!("that track is not loaded yet"));
        };
        if let Ok(mut q) = self.queue.lock() {
            let at = (q.index + 1 + q.added).min(q.songs.len());
            q.songs.insert(at, song);
            q.added += 1;
        }
        self.save();
        self.send_queue();
    }

    fn send_queue(&self) {
        let upcoming = self.queue.lock().map(|q| q.upcoming()).unwrap_or_default();
        let _ = self.ui.send(Update::Queue(upcoming));
    }

    async fn send_playback(&self) {
        let (song, context, shuffle, repeat) = match self.queue.lock() {
            Ok(q) => (q.current().cloned(), q.context.clone(), q.shuffle, q.repeat),
            Err(_) => return,
        };
        let Some(song) = song else {
            let _ = self.ui.send(Update::Playback(None));
            return;
        };
        let uri = song.info.uri.as_str();
        let asked = self.loaded.lock().is_ok_and(|l| l.as_deref() == Some(uri));
        // Only mpv's own file speaks for the track: while the next one resolves, mpv may
        // still hold the last one.
        let open = self.in_mpv.lock().is_ok_and(|m| m.as_deref() == Some(uri));
        let idle = self.get_bool("idle-active").await.unwrap_or(true);
        let paused = self.get_bool("pause").await.unwrap_or(true);
        // Right after a load mpv has no position yet: still loading, not playing at 0.
        let position = if open && !idle {
            self.get_f64("time-pos").await
        } else {
            None
        };
        let live = position.is_some();
        let position = position.unwrap_or(0.0);
        let duration = if live {
            self.get_f64("duration").await
        } else {
            None
        };
        let volume = self.get_f64("volume").await.map(|v| v.round() as u8);
        let playback = Playback {
            device: "mpv".into(),
            volume,
            playing: live && !paused,
            loading: asked && !live,
            position: Duration::from_secs_f64(position.max(0.0)),
            shuffle,
            repeat,
            context: context.map(|(uri, kind)| PlayContext { uri, kind }),
            now: Some(now_playing(&song, duration)),
        };
        let _ = self.ui.send(Update::Playback(Some(Box::new(playback))));
    }

    // ---- what plays ----------------------------------------------------------------

    /// A single track: it plays at once and its radio lines up behind it.
    async fn play_track(self: &Arc<Self>, uri: &str) -> Result<()> {
        let song = match self.known(uri) {
            Some(song) => song,
            None => {
                let (_, songs) = self.radio(uri).await?;
                songs.into_iter().next().context("track not found")?
            }
        };
        self.play_queue(vec![song], 0, None);
        let (page, songs) = self.radio(uri).await?;
        self.remember(&songs);
        // Still just that track (nothing else started meanwhile): its radio follows.
        if let Ok(mut q) = self.queue.lock()
            && q.songs.len() == 1
            && q.songs[0].info.uri == uri
        {
            q.songs
                .extend(songs.into_iter().filter(|s| s.info.uri != uri));
            q.context = Some((page.uri.clone(), "playlist".into()));
        }
        let _ = self.ui.send(Update::Context(page));
        self.save();
        self.send_queue();
        self.prefetch();
        Ok(())
    }

    async fn play_in(self: &Arc<Self>, context: &str, track: Option<&str>) -> Result<()> {
        let kind = kind_of(context);
        let (page, songs) = self.list(kind, context).await?;
        let index = track
            .and_then(|t| songs.iter().position(|s| s.info.uri == t))
            .unwrap_or(0);
        let _ = self.ui.send(Update::Context(page));
        self.play_queue(
            songs,
            index,
            Some((context.to_owned(), context_kind(kind).into())),
        );
        Ok(())
    }

    /// The tracks behind a list, cached; recently played is asked again every time.
    async fn list(&self, kind: Kind, uri: &str) -> Result<(Arc<ListPage>, Vec<Song>)> {
        if kind != Kind::Recent
            && let Some(hit) = self.lists.lock().ok().and_then(|l| l.get(uri).cloned())
        {
            return Ok(hit);
        }
        let client = self.client().await?;
        let (name, songs) = match kind {
            Kind::Album => {
                let (album, thumb) = self.album(uri).await?;
                let songs = album
                    .tracks
                    .iter()
                    .map(|t| Song {
                        info: t.clone(),
                        thumb: thumb.clone(),
                        year: album.year,
                    })
                    .collect();
                (album.name.clone(), songs)
            }
            Kind::Artist => {
                let (artist, _) = self.artist(uri).await?;
                let songs = artist
                    .top
                    .iter()
                    .map(|t| {
                        self.known(&t.uri).unwrap_or(Song {
                            info: t.clone(),
                            thumb: None,
                            year: None,
                        })
                    })
                    .collect();
                (artist.name.clone(), songs)
            }
            Kind::Liked | Kind::Playlist => {
                let list_id = id(uri, "playlist").context("not a playlist")?;
                if list_id.starts_with("RD") {
                    let answer = client
                        .call(
                            "next",
                            json!({ "playlistId": list_id, "isAudioOnly": true }),
                        )
                        .await?;
                    let songs = parse::songs(&answer);
                    // An empty name would read as the liked songs on screen.
                    let name = songs
                        .first()
                        .map(|s| format!("{} Radio", s.info.name))
                        .unwrap_or_else(|| "Radio".into());
                    (name, songs)
                } else {
                    let first = client.browse(&format!("VL{list_id}")).await?;
                    let (name, mut songs) = parse::playlist(&first);
                    let mut token = parse::continuation(&first);
                    while let Some(t) = token.take().filter(|_| songs.len() < MAX_LIST) {
                        let more = client.call("browse", json!({ "continuation": t })).await?;
                        let page = parse::songs(&more);
                        if page.is_empty() {
                            break;
                        }
                        songs.extend(page);
                        token = parse::continuation(&more);
                    }
                    let name = if kind == Kind::Liked {
                        String::new()
                    } else {
                        name
                    };
                    (name, songs)
                }
            }
            Kind::Recent if client.signed_in() => {
                let answer = client.browse("FEmusic_history").await?;
                (String::new(), parse::songs(&answer))
            }
            Kind::Recent => (String::new(), Vec::new()),
            Kind::Heading => bail!("nothing to open"),
        };
        plog!("ytmusic list {kind:?} {uri}: {} tracks", songs.len());
        self.remember(&songs);
        let page = Arc::new(ListPage {
            uri: uri.to_owned(),
            name,
            tracks: songs.iter().map(|s| s.info.clone()).collect(),
        });
        if kind != Kind::Recent
            && let Ok(mut lists) = self.lists.lock()
        {
            lists.insert(uri.to_owned(), (page.clone(), songs.clone()));
        }
        Ok((page, songs))
    }

    async fn artist(&self, uri: &str) -> Result<(Arc<ArtistPage>, Option<String>)> {
        if let Some(hit) = self.artists.lock().ok().and_then(|a| a.get(uri).cloned()) {
            return Ok(hit);
        }
        let channel = id(uri, "artist").context("not an artist")?;
        let answer = self.client().await?.browse(channel).await?;
        let (page, radio, top) = parse::artist(&answer, uri);
        self.remember(&top);
        let hit = (Arc::new(page), radio);
        if let Ok(mut artists) = self.artists.lock() {
            artists.insert(uri.to_owned(), hit.clone());
        }
        Ok(hit)
    }

    async fn album(&self, uri: &str) -> Result<(Arc<AlbumPage>, Option<String>)> {
        if let Some(hit) = self.albums.lock().ok().and_then(|a| a.get(uri).cloned()) {
            return Ok(hit);
        }
        let browse = id(uri, "album").context("not an album")?;
        let answer = self.client().await?.browse(browse).await?;
        let (page, thumb) = parse::album(&answer, uri);
        let hit = (Arc::new(page), thumb);
        if let Ok(mut albums) = self.albums.lock() {
            albums.insert(uri.to_owned(), hit.clone());
        }
        Ok(hit)
    }

    /// A track's or an artist's radio.
    async fn radio(&self, seed: &str) -> Result<(Arc<ListPage>, Vec<Song>)> {
        let (body, list_id, name) = if let Some(video) = id(seed, "track") {
            let list = format!("RDAMVM{video}");
            let body = json!({ "videoId": video, "playlistId": list, "isAudioOnly": true });
            (body, list, None)
        } else if id(seed, "artist").is_some() {
            let (artist, radio) = self.artist(seed).await?;
            let list = radio.context("this artist has no radio")?;
            let body = json!({ "playlistId": list, "isAudioOnly": true });
            (body, list, Some(artist.name.clone()))
        } else {
            bail!("no radio for {seed}");
        };
        let uri = playlist_uri(&list_id);
        if let Some(hit) = self.lists.lock().ok().and_then(|l| l.get(&uri).cloned()) {
            return Ok(hit);
        }
        let answer = self.client().await?.call("next", body).await?;
        let songs = parse::songs(&answer);
        if songs.is_empty() {
            bail!("YouTube Music sent an empty radio");
        }
        self.remember(&songs);
        let name = name.unwrap_or_else(|| songs[0].info.name.clone());
        let page = Arc::new(ListPage {
            uri: uri.clone(),
            name: format!("{name} Radio"),
            tracks: songs.iter().map(|s| s.info.clone()).collect(),
        });
        if let Ok(mut lists) = self.lists.lock() {
            lists.insert(uri, (page.clone(), songs.clone()));
        }
        Ok((page, songs))
    }

    async fn search(&self, query: &str) -> Result<SearchResults> {
        let client = self.client().await?;
        let ask = |params: &'static str| {
            let client = client.clone();
            let query = query.to_owned();
            async move {
                client
                    .call("search", json!({ "query": query, "params": params }))
                    .await
            }
        };
        let (songs, albums, artists, playlists) =
            tokio::join!(ask(SONGS), ask(ALBUMS), ask(ARTISTS), ask(PLAYLISTS));
        let songs = parse::songs(&songs?);
        self.remember(&songs);
        let entries = |answer: Result<Value>, kind| {
            answer
                .map(|a| parse::search_entries(&a, kind))
                .unwrap_or_default()
        };
        Ok(SearchResults {
            query: query.to_owned(),
            tracks: songs.into_iter().take(10).map(|s| s.info).collect(),
            albums: entries(albums, Kind::Album).into_iter().take(10).collect(),
            artists: entries(artists, Kind::Artist)
                .into_iter()
                .take(10)
                .collect(),
            playlists: entries(playlists, Kind::Playlist)
                .into_iter()
                .take(10)
                .collect(),
        })
    }

    // ---- around the playing track --------------------------------------------------

    /// Cover, liked state, artist page, album page and lyrics for a new track.
    fn follow(self: &Arc<Self>, song: &Song) {
        let uri = song.info.uri.clone();
        let liked = self.liked.lock().is_ok_and(|l| l.contains(&uri));
        let _ = self.ui.send(Update::Liked(uri.clone(), liked));

        let (state, s) = (self.clone(), song.clone());
        tokio::spawn(async move {
            let Some(url) = s.thumb.as_deref().map(|u| parse::sized(u, COVER_PX)) else {
                return;
            };
            match state.cover_file(&cover_id(&s), &url).await {
                Ok(path) => drop(state.ui.send(Update::Cover(s.info.uri.clone(), path))),
                Err(err) => plog!("ytmusic cover: {err:#}"),
            }
        });
        if let Some((artist, _)) = song.info.artists.first() {
            let (state, artist) = (self.clone(), artist.clone());
            tokio::spawn(async move { state.artist_page(&artist).await });
        }
        if let Some((album, _)) = song.info.album.clone() {
            let state = self.clone();
            tokio::spawn(async move {
                match state.album(&album).await {
                    Ok((page, _)) => drop(state.ui.send(Update::Album(page))),
                    Err(err) => plog!("ytmusic album: {err:#}"),
                }
            });
        }
        let (state, s) = (self.clone(), song.clone());
        tokio::spawn(async move {
            let artist = s
                .info
                .artists
                .first()
                .map(|(_, n)| n.as_str())
                .unwrap_or("");
            let album = s.info.album.as_ref().map(|(_, n)| n.as_str()).unwrap_or("");
            let page =
                super::lyrics::lrclib(&state.http, artist, &s.info.name, album, s.info.duration)
                    .await
                    .unwrap_or_else(|err| {
                        plog!("lrclib: {err:#}");
                        None
                    });
            let _ = state
                .ui
                .send(Update::Lyrics(s.info.uri.clone(), page.map(Arc::new)));
        });
    }

    /// An artist's page, then the covers of its releases.
    async fn artist_page(&self, uri: &str) {
        let page = match self.artist(uri).await {
            Ok((page, _)) => page,
            Err(err) => return self.report(&err),
        };
        let _ = self.ui.send(Update::Artist(page.clone()));
        for card in &page.discography {
            let Some(url) = &card.cover else { continue };
            let file = format!("card-{}", card.uri.replace(':', "-"));
            match self.cover_file(&file, url).await {
                Ok(path) => drop(self.ui.send(Update::CardCover(card.uri.clone(), path))),
                Err(err) => plog!("cover {}: {err:#}", card.uri),
            }
        }
    }

    async fn cover_file(&self, id: &str, url: &str) -> Result<PathBuf> {
        let path = cover_path(id);
        if path.exists() {
            return Ok(path);
        }
        let dir = path.parent().context("cover dir")?;
        std::fs::create_dir_all(dir)?;
        let bytes = self
            .http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let tmp = dir.join(format!(".{id}.tmp"));
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }

    /// The stream address for a track: cached, or asked from yt-dlp.
    async fn stream_url(&self, uri: &str) -> Result<String> {
        let gate = self
            .resolving
            .lock()
            .map_err(|_| anyhow!("resolver state poisoned"))?
            .entry(uri.to_owned())
            .or_default()
            .clone();
        let _one_at_a_time = gate.lock().await;
        if let Some((url, at)) = self.urls.lock().ok().and_then(|u| u.get(uri).cloned())
            && at.elapsed() < URL_LIFE
        {
            return Ok(url);
        }
        let video = id(uri, "track").context("not a track")?;
        let started = Instant::now();
        let mut command = tokio::process::Command::new("yt-dlp");
        command.args([
            "-f",
            "bestaudio/best",
            "-g",
            "--no-playlist",
            "--no-warnings",
            "-q",
        ]);
        if let Some(file) = &self.cookie_file {
            command.arg("--cookies").arg(file);
        }
        let out = command
            .arg(format!("https://music.youtube.com/watch?v={video}"))
            .output()
            .await
            .context("yt-dlp is not installed (brew install yt-dlp)")?;
        let text = String::from_utf8_lossy(&out.stdout);
        let Some(url) = text.lines().find(|l| l.starts_with("http")) else {
            let err = String::from_utf8_lossy(&out.stderr);
            bail!(
                "yt-dlp: {}",
                err.lines().last().unwrap_or("no stream address")
            );
        };
        plog!(
            "ytmusic: resolved {video} in {:.1} s",
            started.elapsed().as_secs_f64()
        );
        if let Ok(mut urls) = self.urls.lock() {
            urls.insert(uri.to_owned(), (url.to_owned(), Instant::now()));
        }
        Ok(url.to_owned())
    }

    // ---- the signed-in library -----------------------------------------------------

    async fn load_library(self: &Arc<Self>) -> Result<()> {
        let client = self.client().await?;
        let signed_in = client.signed_in();
        let mut made_for_you = Vec::new();
        match client.browse("FEmusic_home").await {
            Ok(home) => {
                for entry in parse::card_entries(&home) {
                    if entry.kind == Kind::Playlist
                        && !made_for_you
                            .iter()
                            .any(|e: &crate::data::Entry| e.uri == entry.uri)
                    {
                        made_for_you.push(entry);
                    }
                }
                made_for_you.truncate(MADE_FOR_YOU);
            }
            Err(err) => plog!("ytmusic home: {err:#}"),
        }
        let (mut playlists, mut albums) = (Vec::new(), Vec::new());
        if signed_in {
            let grid = client.browse("FEmusic_liked_playlists").await?;
            playlists = parse::card_entries(&grid)
                .into_iter()
                .filter(|e| {
                    e.kind == Kind::Playlist && e.uri != LIKED && e.uri != "ytm:playlist:SE"
                })
                .map(|mut e| {
                    e.editable = true;
                    e
                })
                .collect();
            match client.browse("FEmusic_liked_albums").await {
                Ok(grid) => {
                    albums = parse::card_entries(&grid)
                        .into_iter()
                        .filter(|e| e.kind == Kind::Album)
                        .collect();
                }
                Err(err) => plog!("ytmusic saved albums: {err:#}"),
            }
        }
        let index = LibraryIndex {
            // The UI plays liked songs as a context only when it knows the user.
            user: if signed_in {
                "ytmusic".into()
            } else {
                String::new()
            },
            liked: Some(LIKED.into()),
            made_for_you,
            playlists,
            albums,
            held: None,
        };
        let _ = self.ui.send(Update::Library(Box::new(index)));
        if signed_in {
            let (page, _) = self.list(Kind::Liked, LIKED).await?;
            if let Ok(mut liked) = self.liked.lock() {
                liked.extend(page.tracks.iter().map(|t| t.uri.clone()));
            }
            let _ = self
                .ui
                .send(Update::LikedLibrary(Arc::new(page.tracks.clone())));
            // The playing track may have been followed before the list arrived.
            if let Some(uri) = self.loaded.lock().ok().and_then(|l| l.clone()) {
                let liked = self.liked.lock().is_ok_and(|l| l.contains(&uri));
                let _ = self.ui.send(Update::Liked(uri, liked));
            }
        }
        Ok(())
    }

    async fn set_liked(&self, uri: &str, liked: bool) -> Result<()> {
        if !self.signed_in() {
            bail!("sign in to YouTube Music to like songs (pikap auth)");
        }
        let video = id(uri, "track").context("not a track")?;
        let path = if liked {
            "like/like"
        } else {
            "like/removelike"
        };
        self.client()
            .await?
            .call(path, json!({ "target": { "videoId": video } }))
            .await?;
        if let Ok(mut set) = self.liked.lock() {
            if liked {
                set.insert(uri.to_owned());
            } else {
                set.remove(uri);
            }
        }
        // The liked list changed; read it again next time.
        if let Ok(mut lists) = self.lists.lock() {
            lists.remove(LIKED);
        }
        let _ = self.ui.send(Update::Liked(uri.to_owned(), liked));
        Ok(())
    }

    async fn add_to_playlist(&self, playlist: &str, track: &str) -> Result<()> {
        if !self.signed_in() {
            bail!("sign in to YouTube Music to edit playlists (pikap auth)");
        }
        let list = id(playlist, "playlist").context("not a playlist")?;
        let video = id(track, "track").context("not a track")?;
        let body = json!({ "playlistId": list, "actions": [{
            "action": "ACTION_ADD_VIDEO", "addedVideoId": video,
            "dedupeOption": "DEDUPE_OPTION_SKIP" }] });
        self.client()
            .await?
            .call("browse/edit_playlist", body)
            .await?;
        if let Ok(mut lists) = self.lists.lock() {
            lists.remove(playlist);
        }
        let _ = self.ui.send(Update::Edited {
            playlist: playlist.to_owned(),
            track: track.to_owned(),
            added: true,
        });
        if let Ok((page, _)) = self.list(Kind::Playlist, playlist).await {
            let _ = self.ui.send(Update::List(page));
        }
        Ok(())
    }

    // ---- remembering the queue -----------------------------------------------------

    fn save(&self) {
        let Ok(q) = self.queue.lock().map(|q| q.clone()) else {
            return;
        };
        let path = session_path();
        let write = || -> Result<()> {
            std::fs::create_dir_all(path.parent().context("session dir")?)?;
            std::fs::write(&path, serde_json::to_vec(&q)?)?;
            Ok(())
        };
        if let Err(err) = write() {
            plog!("ytmusic session: {err:#}");
        }
    }

    /// pikap is closing: remember where the track stands, then stop mpv.
    async fn quit(&self) {
        self.quitting
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let Some(mpv) = self.mpv.lock().await.clone() else {
            return;
        };
        let live = self.loaded.lock().is_ok_and(|l| l.is_some());
        let at = if live {
            mpv.get("time-pos").await.ok().and_then(|v| v.as_f64())
        } else {
            None
        };
        if let (Some(at), Ok(mut q)) = (at, self.queue.lock()) {
            q.position = at;
        }
        self.save();
        let _ = mpv.command(json!(["quit"])).await;
        plog!("ytmusic: mpv stopped with pikap");
    }

    /// Picks up the saved queue. When mpv still plays, it plays that queue's track.
    async fn restore(self: &Arc<Self>) {
        let Some(saved) = std::fs::read(session_path())
            .ok()
            .and_then(|b| serde_json::from_slice::<Queue>(&b).ok())
        else {
            return;
        };
        let Some(current) = saved.current().cloned() else {
            return;
        };
        self.remember(&saved.songs);
        *self.resume_at.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((current.info.uri.clone(), saved.position));
        if let Ok(mut q) = self.queue.lock() {
            *q = saved;
        }
        let running = match self.mpv().await {
            Ok((_, running)) => running,
            Err(err) => {
                self.report(&err);
                false
            }
        };
        let idle = self.get_bool("idle-active").await.unwrap_or(true);
        if running && !idle {
            *self.loaded.lock().unwrap_or_else(|e| e.into_inner()) = Some(current.info.uri.clone());
            *self.in_mpv.lock().unwrap_or_else(|e| e.into_inner()) = Some(current.info.uri.clone());
        } else {
            // Resolving takes seconds; start now so pressing play is instant.
            let (state, uri) = (self.clone(), current.info.uri.clone());
            tokio::spawn(async move {
                if let Err(err) = state.stream_url(&uri).await {
                    plog!("ytmusic: resolving the saved track: {err:#}");
                }
            });
        }
        self.prefetch();
        self.send_queue();
        self.follow(&current);
    }
}

fn now_playing(song: &Song, duration: Option<f64>) -> NowPlaying {
    let info = &song.info;
    let cover = cover_path(&cover_id(song));
    NowPlaying {
        id: info.uri.clone(),
        title: info.name.clone(),
        artist: info.artist_line(),
        album: info
            .album
            .as_ref()
            .map(|(_, n)| n.clone())
            .unwrap_or_default(),
        year: song.year,
        liked: false,
        duration: match duration {
            Some(d) if d > 0.0 => Duration::from_secs_f64(d),
            _ => info.duration,
        },
        cover: cover.exists().then_some(cover),
        artist_uri: info.artists.first().map(|(u, _)| u.clone()),
        album_uri: info.album.as_ref().map(|(u, _)| u.clone()),
    }
}

/// Covers are cached per album where there is one, otherwise per track.
fn cover_id(song: &Song) -> String {
    let key = song
        .info
        .album
        .as_ref()
        .map(|(u, _)| u.as_str())
        .unwrap_or(&song.info.uri);
    key.replace(':', "-")
}

fn cover_path(id: &str) -> PathBuf {
    config::cache_dir().join("covers").join(format!("{id}.jpg"))
}

fn session_path() -> PathBuf {
    config::cache_dir().join("ytmusic").join("session.json")
}

fn kind_of(uri: &str) -> Kind {
    if uri == LIKED {
        Kind::Liked
    } else if id(uri, "album").is_some() {
        Kind::Album
    } else if id(uri, "artist").is_some() {
        Kind::Artist
    } else {
        Kind::Playlist
    }
}

fn context_kind(kind: Kind) -> &'static str {
    match kind {
        Kind::Album => "album",
        Kind::Artist => "artist",
        Kind::Liked => "collection",
        _ => "playlist",
    }
}

/// yt-dlp reads cookies from a Netscape file; this one is private to the user.
fn write_cookie_file(header: &str) -> Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = config::cache_dir().join("ytmusic");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("cookies.txt");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    writeln!(file, "# Netscape HTTP Cookie File")?;
    for pair in header.split(';') {
        if let Some((name, value)) = pair.trim().split_once('=') {
            writeln!(
                file,
                ".youtube.com\tTRUE\t/\tTRUE\t2147483647\t{name}\t{value}"
            )?;
        }
    }
    Ok(path)
}

/// Plays real music through mpv and talks to YouTube Music; run it by hand:
/// `cargo test live::plays -- --ignored --nocapture`. It quits mpv at the end.
#[cfg(test)]
mod live {
    use super::*;

    fn describe(u: &Update) -> String {
        match u {
            Update::Playback(Some(p)) => format!(
                "playback playing={} pos={:.1}s vol={:?} now={:?} cover={}",
                p.playing,
                p.position.as_secs_f64(),
                p.volume,
                p.now
                    .as_ref()
                    .map(|n| (&n.title, &n.artist, &n.album, n.duration.as_secs())),
                p.now.as_ref().is_some_and(|n| n.cover.is_some())
            ),
            Update::Playback(None) => "playback none".into(),
            Update::Queue(q) => format!("queue {} next={:?}", q.len(), q.first().map(|t| &t.title)),
            Update::Search(s) => format!(
                "search tracks={} albums={} artists={} playlists={}",
                s.tracks.len(),
                s.albums.len(),
                s.artists.len(),
                s.playlists.len()
            ),
            Update::Artist(a) => format!(
                "artist {} top={} releases={} related={}",
                a.name,
                a.top.len(),
                a.discography.len(),
                a.related.len()
            ),
            Update::Album(a) => format!("album {} {:?} tracks={}", a.name, a.year, a.tracks.len()),
            Update::Lyrics(_, l) => {
                format!("lyrics {:?}", l.as_ref().map(|l| (l.synced, l.lines.len())))
            }
            Update::Cover(_, p) => format!("cover {}", p.display()),
            Update::Context(c) => format!("context {} {} tracks={}", c.uri, c.name, c.tracks.len()),
            Update::Library(l) => format!(
                "library mfy={} playlists={} user={:?}",
                l.made_for_you.len(),
                l.playlists.len(),
                l.user
            ),
            Update::Liked(_, l) => format!("liked {l}"),
            Update::Error(e) => format!("ERROR {e}"),
            Update::Meta(m) => format!("meta {m}"),
            Update::CardCover(..) => "cardcover".into(),
            _ => "other".into(),
        }
    }

    #[test]
    #[ignore]
    fn plays() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (cmd, rx) = unbounded_channel();
            let (tx, updates) = std::sync::mpsc::channel();
            tokio::spawn(run(rx, tx));
            let t0 = Instant::now();
            let drain = |label: &str, secs: u64, show_playback: bool| {
                let end = Instant::now() + Duration::from_secs(secs);
                let mut cards = 0;
                let mut last_playback = None;
                while Instant::now() < end {
                    while let Ok(u) = updates.try_recv() {
                        match &u {
                            Update::CardCover(..) => cards += 1,
                            Update::Playback(_) if !show_playback => {
                                last_playback = Some(describe(&u))
                            }
                            _ => println!(
                                "[{:5.1}s {label}] {}",
                                t0.elapsed().as_secs_f64(),
                                describe(&u)
                            ),
                        }
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                if let Some(p) = last_playback {
                    println!("[{:5.1}s {label}] last {p}", t0.elapsed().as_secs_f64());
                }
                if cards > 0 {
                    println!("[{label}] {cards} card covers");
                }
            };
            cmd.send(Command::Volume(20)).unwrap();
            cmd.send(Command::Search("gorillaz rhinestone eyes".into()))
                .unwrap();
            drain("start", 6, false);
            cmd.send(Command::PlayTrack("ytm:track:q1bTAcMAQDc".into()))
                .unwrap();
            drain("play", 16, false);
            // A track nobody resolved yet: it loads for seconds, and meanwhile neither it
            // nor the old track may show as playing.
            cmd.send(Command::PlayTrack("ytm:track:6KjVYeQ9SRw".into()))
                .unwrap();
            let until = Instant::now() + Duration::from_secs(3);
            let mut seen = 0;
            while Instant::now() < until {
                while let Ok(u) = updates.try_recv() {
                    if let Update::Playback(Some(p)) = u {
                        seen += 1;
                        // The old file's state must never speak for the new track:
                        // playing means freshly started, otherwise it is loading.
                        if p.now
                            .as_ref()
                            .is_some_and(|n| n.id == "ytm:track:6KjVYeQ9SRw")
                        {
                            if p.playing {
                                assert!(p.position < Duration::from_millis(3500), "fresh start");
                            } else {
                                assert!(p.loading, "the new track shows as loading");
                            }
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            println!("loading checked over {seen} updates");
            drain("dracula", 12, false);
            cmd.send(Command::SeekTo(60_000)).unwrap();
            drain("seek", 2, true);
            cmd.send(Command::Next).unwrap();
            drain("next", 6, false);
            cmd.send(Command::SetPlaying(false)).unwrap();
            drain("pause", 2, true);
            cmd.send(Command::OpenList(
                Kind::Album,
                "ytm:album:MPREb_RZdSdgE12kU".into(),
            ))
            .unwrap();
            drain("list", 4, false);
            cmd.send(Command::SetPlaying(true)).unwrap();
            drain("playing again", 3, true);

            // Closing pikap stops mpv and remembers where the track stood.
            let (done, wait) = std::sync::mpsc::channel();
            cmd.send(Command::Quit(done)).unwrap();
            assert!(
                wait.recv_timeout(Duration::from_secs(5)).is_ok(),
                "quit answered"
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
            assert!(
                tokio::net::UnixStream::connect(mpv::socket_path())
                    .await
                    .is_err(),
                "mpv is gone"
            );
            let saved: Queue =
                serde_json::from_slice(&std::fs::read(session_path()).unwrap()).unwrap();
            println!(
                "saved at {:.1}s of {}",
                saved.position, saved.songs[saved.index].info.name
            );
            drop(cmd);

            // A new pikap: play picks up at that second.
            let (cmd, rx) = unbounded_channel();
            let (tx, updates) = std::sync::mpsc::channel();
            tokio::spawn(run(rx, tx));
            tokio::time::sleep(Duration::from_secs(2)).await;
            cmd.send(Command::SetPlaying(true)).unwrap();
            let end = Instant::now() + Duration::from_secs(20);
            let mut resumed = None;
            while Instant::now() < end && resumed.is_none() {
                while let Ok(u) = updates.try_recv() {
                    if let Update::Playback(Some(p)) = u
                        && p.playing
                    {
                        resumed = Some(p.position.as_secs_f64());
                    }
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            println!("resumed at {resumed:?}");
            assert!(
                resumed.is_some_and(|r| r >= saved.position - 1.0),
                "picks up where it stopped"
            );
            let (done, wait) = std::sync::mpsc::channel();
            cmd.send(Command::Quit(done)).unwrap();
            let _ = wait.recv_timeout(Duration::from_secs(5));
        });
    }
}
