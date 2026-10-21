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

pub fn path() -> PathBuf {
    config_dir().join("config.toml")
}

/// A Spotify Client ID: 32 hexadecimal characters.
pub fn valid_client_id(id: &str) -> bool {
    id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit())
}

/// Saves `client_id` for `pikap auth`'s setup: a new config from the template, or
/// the line put in front of an existing file that lacks it.
pub fn save_client_id(client_id: &str) -> Result<PathBuf> {
    let path = path();
    write_client_id(&path, client_id)?;
    Ok(path)
}

fn write_client_id(path: &std::path::Path, client_id: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let line = format!("client_id = \"{client_id}\"\n");
    let text = match std::fs::read_to_string(path) {
        Ok(existing) => format!("{line}{existing}"),
        Err(_) => format!(
            "{line}redirect_uri = \"{DEFAULT_REDIRECT_URI}\"\n\n\
             # Optional: Made For You lists appear by themselves once played; share links\n\
             # here show ones you have not played yet.\n\
             made_for_you = []\n"
        ),
    };
    std::fs::write(path, text)?;
    Ok(())
}

pub fn load() -> Result<Config> {
    let path = path();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_writes_or_completes_the_config() {
        let dir = std::env::temp_dir().join(format!("pikap-config-{}", std::process::id()));
        let path = dir.join("config.toml");
        let id = "0123456789abcdef0123456789abcdef";
        assert!(valid_client_id(id));
        assert!(!valid_client_id("not-an-id"));

        write_client_id(&path, id).unwrap();
        let file: File = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(file.client_id.as_deref(), Some(id));
        assert_eq!(file.redirect_uri.as_deref(), Some(DEFAULT_REDIRECT_URI));

        std::fs::write(&path, "made_for_you = [\"x\"]\n").unwrap();
        write_client_id(&path, id).unwrap();
        let file: File = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            file.client_id.as_deref(),
            Some(id),
            "added in front of what was there"
        );
        assert_eq!(file.made_for_you, ["x"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
