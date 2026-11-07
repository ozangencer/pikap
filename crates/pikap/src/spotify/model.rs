//! Lenient Web API models. Development Mode strips fields (`popularity`, `genres`,
//! `followers`) and episodes carry `show` instead of `album`, so everything that is not
//! essential defaults instead of failing the whole response.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::backend;
use crate::data::{NowPlaying, Track};

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct Image {
    pub url: String,
    pub width: Option<u32>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct Artist {
    pub uri: String,
    pub name: String,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct Album {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub release_date: String,
    pub images: Vec<Image>,
    pub artists: Vec<Artist>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct Show {
    pub id: String,
    pub name: String,
    pub images: Vec<Image>,
}

/// A track or an episode.
#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct Item {
    pub uri: String,
    pub name: String,
    pub duration_ms: u64,
    pub artists: Vec<Artist>,
    pub album: Option<Album>,
    pub show: Option<Show>,
}

impl Item {
    pub fn info(&self) -> backend::TrackInfo {
        backend::TrackInfo {
            uri: self.uri.clone(),
            name: self.name.clone(),
            artists: self
                .artists
                .iter()
                .map(|a| (a.uri.clone(), a.name.clone()))
                .collect(),
            duration: std::time::Duration::from_millis(self.duration_ms),
            album: self
                .album
                .as_ref()
                .filter(|a| !a.uri.is_empty())
                .map(|a| (a.uri.clone(), a.name.clone())),
        }
    }

    pub fn artist_line(&self) -> String {
        if self.artists.is_empty() {
            return self
                .show
                .as_ref()
                .map(|s| s.name.clone())
                .unwrap_or_default();
        }
        self.artists
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// A row of the queue.
    pub fn track(&self) -> Track {
        Track {
            uri: self.uri.clone(),
            title: self.name.clone(),
            artist: self.artist_line(),
            duration: Duration::from_millis(self.duration_ms),
        }
    }

    /// The now-playing card, with the cover only when it is already cached.
    pub fn now_playing(&self) -> NowPlaying {
        let album = self.album.clone().unwrap_or_default();
        let cover = self
            .cover(640)
            .map(|(id, _)| cover_path(&id))
            .filter(|p| p.exists());
        NowPlaying {
            id: self.uri.clone(),
            title: self.name.clone(),
            artist: self.artist_line(),
            album: if album.name.is_empty() {
                self.show
                    .as_ref()
                    .map(|s| s.name.clone())
                    .unwrap_or_default()
            } else {
                album.name
            },
            year: album.release_date.get(..4).and_then(|y| y.parse().ok()),
            liked: false,
            duration: Duration::from_millis(self.duration_ms),
            cover,
            artist_uri: self
                .artists
                .first()
                .map(|a| a.uri.clone())
                .filter(|u| !u.is_empty()),
            album_uri: Some(album.uri).filter(|u| !u.is_empty()),
        }
    }

    /// Album (or show) id and the image closest to `target` px wide, for the cover cache.
    pub fn cover(&self, target: u32) -> Option<(String, String)> {
        let (id, images) = match (&self.album, &self.show) {
            (Some(a), _) => (&a.id, &a.images),
            (None, Some(s)) => (&s.id, &s.images),
            _ => return None,
        };
        let best = images
            .iter()
            .min_by_key(|i| i.width.unwrap_or(target).abs_diff(target))?;
        Some((id.clone(), best.url.clone()))
    }
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct Device {
    /// Absent for devices the Web API cannot control.
    pub id: Option<String>,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub volume_percent: Option<u8>,
    pub is_active: bool,
    pub is_restricted: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct SavedTrack {
    pub track: Item,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct SavedAlbum {
    pub album: Album,
}

/// One page of a paged list. Spotify leaves holes (`null`) in some search results.
#[derive(Deserialize)]
#[serde(bound = "T: Deserialize<'de>")]
pub struct Page<T> {
    #[serde(default)]
    pub items: Vec<Option<T>>,
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default)]
    pub total: Option<usize>,
}

impl<T> Default for Page<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            next: None,
            total: None,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Owner {
    pub id: String,
    pub display_name: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Total {
    pub total: usize,
}

/// A playlist as `me/playlists` and search list it; Development Mode renamed
/// `tracks` to `items`.
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Playlist {
    pub uri: String,
    pub name: String,
    pub owner: Owner,
    pub collaborative: bool,
    #[serde(alias = "tracks")]
    pub items: Total,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct SearchResponse {
    pub tracks: Page<Item>,
    pub albums: Page<Album>,
    pub artists: Page<Artist>,
    pub playlists: Page<Playlist>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Devices {
    pub devices: Vec<Device>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct PlayHistory {
    pub track: Item,
    pub context: Option<PlayContext>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Recent {
    pub items: Vec<PlayHistory>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct Playback {
    pub device: Device,
    pub context: Option<PlayContext>,
    pub is_playing: bool,
    pub progress_ms: Option<u64>,
    pub shuffle_state: bool,
    pub repeat_state: String,
    pub item: Option<Item>,
}

/// What the playback was started from (playlist, album, artist, liked songs…).
#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct PlayContext {
    pub uri: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Queue {
    pub queue: Vec<Item>,
}

/// Where a cover with this album or show id is cached.
pub fn cover_path(id: &str) -> PathBuf {
    super::config::cache_dir()
        .join("covers")
        .join(format!("{id}.jpg"))
}

impl From<Device> for backend::Device {
    fn from(d: Device) -> Self {
        Self {
            id: d.id,
            name: d.name,
            kind: d.kind,
            volume_percent: d.volume_percent,
            is_active: d.is_active,
            is_restricted: d.is_restricted,
        }
    }
}

impl From<PlayContext> for backend::PlayContext {
    fn from(c: PlayContext) -> Self {
        Self {
            uri: c.uri,
            kind: c.kind,
        }
    }
}

impl From<Playback> for backend::Playback {
    fn from(p: Playback) -> Self {
        Self {
            device: p.device.name,
            volume: p.device.volume_percent,
            playing: p.is_playing,
            loading: false,
            position: Duration::from_millis(p.progress_ms.unwrap_or(0)),
            shuffle: p.shuffle_state,
            repeat: match p.repeat_state.as_str() {
                "context" => 1,
                "track" => 2,
                _ => 0,
            },
            context: p.context.filter(|c| !c.uri.is_empty()).map(Into::into),
            now: p.item.as_ref().map(Item::now_playing),
        }
    }
}

impl From<PlayHistory> for backend::LastPlayed {
    fn from(h: PlayHistory) -> Self {
        Self {
            title: h.track.name.clone(),
            artist: h.track.artist_line(),
            uri: h.track.uri,
            context: h.context.map(Into::into),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_mode_track_without_popularity() {
        let json = r#"{"device":{"name":"MacBook Pro","type":"Computer","volume_percent":64},
            "is_playing":true,"progress_ms":88000,
            "item":{"uri":"spotify:track:x","name":"Rhinestone Eyes","duration_ms":200000,
              "artists":[{"name":"Gorillaz"}],
              "album":{"id":"a","name":"Plastic Beach","release_date":"2010-03-03",
                "images":[{"url":"big","width":640},{"url":"small","width":64}]}}}"#;
        let p: Playback = serde_json::from_str(json).unwrap();
        let item = p.item.unwrap();
        assert_eq!(item.artist_line(), "Gorillaz");
        assert_eq!(item.cover(600).unwrap().1, "big");
    }

    #[test]
    fn playback_crosses_into_plain_data() {
        let json = r#"{"device":{"name":"MacBook Pro","type":"Computer","volume_percent":64},
            "is_playing":true,"progress_ms":88000,"shuffle_state":true,"repeat_state":"track",
            "context":{"type":"playlist","uri":"spotify:playlist:p"},
            "item":{"uri":"spotify:track:x","name":"Rhinestone Eyes","duration_ms":200000,
              "artists":[{"uri":"spotify:artist:g","name":"Gorillaz"}],
              "album":{"id":"a","uri":"spotify:album:a","name":"Plastic Beach","release_date":"2010-03-03"}}}"#;
        let p: backend::Playback = serde_json::from_str::<Playback>(json).unwrap().into();
        assert_eq!(p.device, "MacBook Pro");
        assert_eq!(p.volume, Some(64));
        assert!(p.playing && p.shuffle);
        assert_eq!(p.repeat, 2);
        assert_eq!(p.position, Duration::from_secs(88));
        assert_eq!(p.context.unwrap().kind, "playlist");
        let now = p.now.unwrap();
        assert_eq!(
            (now.title.as_str(), now.artist.as_str(), now.album.as_str()),
            ("Rhinestone Eyes", "Gorillaz", "Plastic Beach")
        );
        assert_eq!(now.year, Some(2010));
        assert_eq!(now.artist_uri.as_deref(), Some("spotify:artist:g"));
        assert_eq!(now.album_uri.as_deref(), Some("spotify:album:a"));
        assert!(!now.liked);

        // An episode has a show instead of an album and no artists.
        let episode = r#"{"is_playing":false,"repeat_state":"off","context":{"type":"show","uri":""},
            "item":{"uri":"spotify:episode:e","name":"Ep","duration_ms":1000,
              "show":{"id":"s","name":"The Show"}}}"#;
        let p: backend::Playback = serde_json::from_str::<Playback>(episode).unwrap().into();
        assert!(p.context.is_none(), "an empty context uri is no context");
        let now = p.now.unwrap();
        assert_eq!(
            (now.artist.as_str(), now.album.as_str()),
            ("The Show", "The Show")
        );
        assert_eq!((now.artist_uri, now.album_uri), (None, None));
    }

    #[test]
    fn recently_played_and_devices() {
        let recent: Recent = serde_json::from_str(
            r#"{"items":[{"track":{"uri":"spotify:track:t","name":"Aaaaah","duration_ms":1,
                "artists":[{"uri":"spotify:artist:a","name":"Agar Agar"}]},
                "played_at":"2026-10-21T16:00:00Z","context":{"type":"artist","uri":"spotify:artist:a"}}]}"#,
        )
        .unwrap();
        assert_eq!(recent.items[0].track.artist_line(), "Agar Agar");
        let devices: Devices = serde_json::from_str(
            r#"{"devices":[{"id":"d1","name":"Studio MacBook","type":"Computer","is_active":true,"volume_percent":100}]}"#,
        )
        .unwrap();
        assert!(devices.devices[0].is_active);
    }

    #[test]
    fn search_skips_holes() {
        let json = r#"{"tracks":{"items":[{"uri":"spotify:track:t","name":"Stylo"}]},
            "playlists":{"items":[null,{"uri":"spotify:playlist:p","name":"Mix",
              "owner":{"id":"u","display_name":"Ada"},"items":{"total":52}}]}}"#;
        let r: SearchResponse = serde_json::from_str(json).unwrap();
        let lists: Vec<Playlist> = r.playlists.items.into_iter().flatten().collect();
        assert_eq!(lists.len(), 1);
        assert_eq!(lists[0].items.total, 52);
        assert!(r.albums.items.is_empty());
    }

    #[test]
    fn episode_uses_show() {
        let json = r#"{"uri":"spotify:episode:e","name":"Ep","duration_ms":1000,
            "show":{"id":"s","name":"A Podcast","images":[{"url":"u","width":300}]}}"#;
        let item: Item = serde_json::from_str(json).unwrap();
        assert_eq!(item.artist_line(), "A Podcast");
        assert_eq!(item.cover(640).unwrap().0, "s");
    }
}
