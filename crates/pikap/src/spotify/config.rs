//! Where pikap keeps things and which Spotify Developer app it talks to.
//!
//! `~/.config/pikap/config.toml` holds `client_id`, `redirect_uri` and the
//! `made_for_you` playlist URLs. The redirect URI must be registered on the app.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

pub const DEFAULT_REDIRECT_URI: &str = "http://127.0.0.1:8989/login";

pub struct Config {
    pub client_id: String,
    pub redirect_uri: String,
    /// Spotify's personalised playlists, which the API cannot list.
    pub made_for_you: Vec<String>,
    /// The file the values came from, for messages.
    pub source: PathBuf,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct File {
    client_id: Option<String>,
    redirect_uri: Option<String>,
    made_for_you: Vec<String>,
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

pub fn config_dir() -> PathBuf {
    home().join(".config/pikap")
}

pub fn cache_dir() -> PathBuf {
    home().join(".cache/pikap")
}

pub fn load() -> Result<Config> {
    let path = config_dir().join("config.toml");
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "{} is missing; create it with `client_id = \"...\"` from your Spotify Developer app",
            path.display()
        )
    })?;
    let file: File =
        toml::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
    let Some(client_id) = file.client_id.filter(|id| !id.trim().is_empty()) else {
        bail!("{}: `client_id` is not set", path.display());
    };
    Ok(Config {
        client_id: client_id.trim().to_owned(),
        redirect_uri: file
            .redirect_uri
            .unwrap_or_else(|| DEFAULT_REDIRECT_URI.to_owned()),
        made_for_you: file.made_for_you,
        source: path,
    })
}
