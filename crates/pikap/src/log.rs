//! Plain append-only log at `~/.cache/pikap/pikap.log`, timestamped from startup.
//! The TUI owns the terminal, so this is where requests and multi-step flows
//! (resuming, signing in) leave a trace.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use crate::spotify::config;

const MAX_BYTES: u64 = 1 << 20;

static LOG: OnceLock<(Instant, Mutex<File>)> = OnceLock::new();

pub fn init() {
    let path = config::cache_dir().join("pikap.log");
    let _ = std::fs::create_dir_all(config::cache_dir());
    let fresh = std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES);
    let file = OpenOptions::new()
        .create(true)
        .append(!fresh)
        .write(true)
        .truncate(fresh)
        .open(&path);
    if let Ok(mut file) = file {
        let _ = writeln!(file, "---- pikap {} ----", env!("CARGO_PKG_VERSION"));
        let _ = LOG.set((Instant::now(), Mutex::new(file)));
    }
}

pub fn write(message: std::fmt::Arguments<'_>) {
    if let Some((start, file)) = LOG.get()
        && let Ok(mut file) = file.lock()
    {
        let _ = writeln!(file, "{:>9.3}s {message}", start.elapsed().as_secs_f64());
    }
}

macro_rules! plog {
    ($($arg:tt)*) => {
        $crate::log::write(format_args!($($arg)*))
    };
}

pub(crate) use plog;
