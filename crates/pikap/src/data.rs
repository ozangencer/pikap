//! What the screen shows. Filled by the Spotify worker in live mode, or by [`demo`]
//! (`--demo`), which mirrors the Dalga mockup and borrows any cover pikap has cached.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::spotify::meta::{AlbumPage, ArtistPage, ListPage, LyricsPage, TrackInfo};

#[derive(Clone)]
pub struct Track {
    pub uri: String,
    pub title: String,
    pub artist: String,
    pub duration: Duration,
}

pub struct NowPlaying {
    /// Track URI; also seeds the decorative waveform.
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub year: Option<u16>,
    pub liked: bool,
    pub duration: Duration,
    pub cover: Option<PathBuf>,
    pub artist_uri: Option<String>,
    pub album_uri: Option<String>,
}

/// What a library or search row opens into.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Kind {
    /// A section title; selection and playback skip it.
    Heading,
    Playlist,
    Album,
    Artist,
    Liked,
    Recent,
}

impl Kind {
    /// The kind behind a playback context's `type`.
    pub fn from_context(kind: &str) -> Option<Self> {
        match kind {
            "playlist" => Some(Self::Playlist),
            "album" => Some(Self::Album),
            "artist" => Some(Self::Artist),
            "collection" => Some(Self::Liked),
            _ => None,
        }
    }
}

/// A playlist, album, artist or library list, as the library and search show it.
#[derive(Clone, Serialize, Deserialize)]
pub struct Entry {
    pub kind: Kind,
    pub uri: String,
    pub title: String,
    /// Owner or artist, shown dimmed.
    pub sub: String,
    pub count: Option<usize>,
    /// Own and collaborative playlists accept edits.
    pub editable: bool,
    /// An album's first artist (uri, name), for `g a`.
    pub artist: Option<(String, String)>,
}

impl Entry {
    pub fn new(kind: Kind, uri: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            kind,
            uri: uri.into(),
            title: title.into(),
            sub: String::new(),
            count: None,
            editable: false,
            artist: None,
        }
    }

    pub fn heading(title: impl Into<String>) -> Self {
        Self::new(Kind::Heading, "", title)
    }
}

/// The user's library as the Web API lists it, plus the configured Made For You lists.
/// Kept on disk (`library.json`) so it shows at once and survives the Web API
/// putting an endpoint on hold.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct LibraryIndex {
    /// Spotify user id; liked songs play as the `spotify:user:{id}:collection` context.
    pub user: String,
    pub made_for_you: Vec<Entry>,
    pub playlists: Vec<Entry>,
    pub albums: Vec<Entry>,
    /// How long a part that could not load (and had no saved copy) is on hold.
    #[serde(skip)]
    pub held: Option<Duration>,
}

impl LibraryIndex {
    pub fn liked_uri(&self) -> String {
        format!("spotify:user:{}:collection", self.user)
    }

    pub fn playlist_mut(&mut self, uri: &str) -> Option<&mut Entry> {
        self.playlists
            .iter_mut()
            .chain(self.made_for_you.iter_mut())
            .find(|e| e.uri == uri)
    }

    pub fn editable(&self, uri: &str) -> bool {
        self.playlists.iter().any(|e| e.uri == uri && e.editable)
    }
}

/// One search, all four kinds at once.
pub struct SearchResults {
    pub query: String,
    pub tracks: Vec<TrackInfo>,
    pub albums: Vec<Entry>,
    pub artists: Vec<Entry>,
    pub playlists: Vec<Entry>,
}

impl From<&TrackInfo> for Track {
    fn from(t: &TrackInfo) -> Self {
        Self {
            uri: t.uri.clone(),
            title: t.name.clone(),
            artist: t.artist_line(),
            duration: t.duration,
        }
    }
}

#[derive(Default)]
pub struct Model {
    pub now: Option<NowPlaying>,
    /// Starts with the playing track.
    pub queue: Vec<Track>,
    pub near: Vec<String>,
    pub device: String,
    pub volume: u8,
    pub shuffle: bool,
    /// 0 off, 1 context, 2 track (Spotify's `repeat_state`).
    pub repeat: u8,
    /// Spotify pads a short looping queue with repeats; the list is cut there instead.
    pub queue_loops: bool,
    /// Title and artist of the last played track, shown while nothing plays.
    pub last_played: Option<(String, String)>,
    /// `None` until the worker reports whether librespot signed in.
    pub meta: Option<bool>,
    pub artist: Option<Arc<ArtistPage>>,
    /// Every liked song, once loaded.
    pub liked: Option<Arc<Vec<TrackInfo>>>,
    /// Tracks of the playlist, album, artist or liked songs playback runs from.
    pub context_list: Option<Arc<ListPage>>,
    pub album: Option<Arc<AlbumPage>>,
    /// Seed URI and Spotify's radio playlist for it.
    pub radio: Option<(String, Arc<ListPage>)>,
    /// `None` while loading, `Some(None)` when the track has no lyrics.
    pub lyrics: Option<Option<Arc<LyricsPage>>>,
    /// Track URI the lyrics belong to.
    pub lyrics_for: Option<String>,
    /// Playlists and albums, once the Web API listed them.
    pub library: Option<LibraryIndex>,
    /// Connect devices, once asked for (`None` while loading).
    pub devices: Option<Vec<crate::spotify::model::Device>>,
    /// Release covers on artist pages, by album URI.
    pub card_covers: std::collections::HashMap<String, PathBuf>,
}

fn track(title: &str, artist: &str, secs: u64) -> Track {
    Track {
        uri: String::new(),
        title: title.into(),
        artist: artist.into(),
        duration: Duration::from_secs(secs),
    }
}

fn any_cached_cover() -> Option<PathBuf> {
    std::fs::read_dir(crate::spotify::config::cache_dir().join("covers"))
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|x| x == "jpg"))
}

pub fn demo() -> Model {
    Model {
        now: Some(NowPlaying {
            id: "spotify:track:rhinestone-eyes".into(),
            title: "Rhinestone Eyes".into(),
            artist: "Gorillaz".into(),
            album: "Plastic Beach".into(),
            year: Some(2010),
            liked: true,
            duration: Duration::from_secs(200),
            cover: any_cached_cover(),
            artist_uri: None,
            album_uri: None,
        }),
        queue: vec![
            track("Rhinestone Eyes", "Gorillaz", 200),
            track("Stylo", "Gorillaz, Mos Def", 270),
            track("Nobody Speak", "DJ Shadow, Run The Jewels", 195),
            track("Loser", "Beck", 235),
            track("Castle In The Snow", "Kadebostany", 226),
            track("Mon chagrin", "Vendredi sur Mer", 219),
            track("Cop Killer", "John Maus", 162),
        ],
        near: ["Blur", "Damon Albarn", "De La Soul", "MGMT", "Tame Impala"]
            .map(String::from)
            .to_vec(),
        device: "MacBook Pro · Spotify.app".into(),
        volume: 64,
        ..Model::default()
    }
}
