//! What the screen and a music service say to each other, whichever service it is.
//!
//! A backend is a background task that takes [`Command`]s from the UI and answers with
//! [`Update`]s. Everything that crosses this line is plain data: URIs, names, durations
//! and files. What a service's own API returns stays inside that service's module.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::data::{Entry, Kind, LibraryIndex, NowPlaying, SearchResults, Track};

/// The music service a backend talks to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Service {
    #[default]
    Spotify,
    YtMusic,
}

impl Service {
    pub fn name(self) -> &'static str {
        match self {
            Self::Spotify => "Spotify",
            Self::YtMusic => "YouTube Music",
        }
    }

    /// The name on the command line and in the saved choice.
    pub fn key(self) -> &'static str {
        match self {
            Self::Spotify => "spotify",
            Self::YtMusic => "ytmusic",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key.trim() {
            "spotify" => Some(Self::Spotify),
            "ytmusic" | "youtube-music" | "ytm" => Some(Self::YtMusic),
            _ => None,
        }
    }
}

/// Whether a URI names a single track (not an episode, list or heading).
pub fn is_track(uri: &str) -> bool {
    uri.starts_with("spotify:track:") || uri.starts_with("ytm:track:")
}

/// Whether a row is real content rather than a heading or a "show more" row.
pub fn is_item(uri: &str) -> bool {
    uri.starts_with("spotify:") || uri.starts_with("ytm:")
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TrackInfo {
    pub uri: String,
    pub name: String,
    /// (uri, name)
    pub artists: Vec<(String, String)>,
    pub duration: Duration,
    /// (uri, name); missing in caches written before it existed.
    #[serde(default)]
    pub album: Option<(String, String)>,
}

impl TrackInfo {
    pub fn artist_line(&self) -> String {
        self.artists
            .iter()
            .map(|(_, name)| name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub struct ArtistPage {
    pub uri: String,
    pub name: String,
    pub top: Vec<TrackInfo>,
    /// (uri, name)
    pub related: Vec<(String, String)>,
    /// 0-100 popularity, where the service reports one.
    pub popularity: u8,
    /// Album and single URIs (`true` for singles), albums first, as the service orders them.
    pub releases: Vec<(String, bool)>,
    /// The releases with names, years and covers; empty until they are looked up.
    pub discography: Vec<Card>,
}

/// An album or single on an artist's page.
#[derive(Clone)]
pub struct Card {
    pub uri: String,
    pub name: String,
    pub year: Option<u16>,
    pub single: bool,
    /// Address of a cover about 300 px wide.
    pub cover: Option<String>,
}

/// The tracks of whatever playback runs from: a playlist, album, artist or liked songs.
pub struct ListPage {
    pub uri: String,
    /// Empty for liked songs, which the UI names itself.
    pub name: String,
    pub tracks: Vec<TrackInfo>,
}

pub struct AlbumPage {
    pub uri: String,
    pub name: String,
    pub artist: String,
    pub year: Option<u16>,
    pub tracks: Vec<TrackInfo>,
}

pub struct LyricLine {
    pub start: Duration,
    pub words: String,
}

pub struct LyricsPage {
    pub synced: bool,
    pub lines: Vec<LyricLine>,
}

/// How the service is coping with its request limits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quota {
    Normal,
    Retrying,
    Holding(usize),
}

/// Where music can play: a speaker, a phone, this Mac.
#[derive(Clone, Default)]
pub struct Device {
    /// Absent for devices that cannot be controlled.
    pub id: Option<String>,
    pub name: String,
    /// The service's word for it (`Computer`, `Smartphone`, `Speaker`…).
    pub kind: String,
    pub volume_percent: Option<u8>,
    pub is_active: bool,
    pub is_restricted: bool,
}

/// What playback was started from.
#[derive(Clone, Default)]
pub struct PlayContext {
    pub uri: String,
    /// `playlist`, `album`, `artist` or `collection` (liked songs).
    pub kind: String,
}

/// The state of playback, polled from the service.
#[derive(Default)]
pub struct Playback {
    pub device: String,
    pub volume: Option<u8>,
    pub playing: bool,
    /// The track is chosen but not playing yet (YouTube Music resolves its stream).
    pub loading: bool,
    pub position: Duration,
    pub shuffle: bool,
    /// 0 off, 1 context, 2 track.
    pub repeat: u8,
    pub context: Option<PlayContext>,
    /// The playing track; `liked` is always false here and follows in [`Update::Liked`],
    /// `cover` is set only when the file is already on disk.
    pub now: Option<NowPlaying>,
}

/// The last track played, shown while nothing plays and resumed from.
pub struct LastPlayed {
    pub uri: String,
    pub title: String,
    pub artist: String,
    pub context: Option<PlayContext>,
}

pub enum Command {
    SetPlaying(bool),
    Next,
    Previous,
    SeekTo(u64),
    Volume(u8),
    SetLiked(String, bool),
    SetShuffle(bool),
    /// `off`, `context` or `track`.
    SetRepeat(&'static str),
    /// One track inside its album (Spotify.app locks up on `uris` lists).
    PlayTrack(String),
    /// A recently played track, inside the context it was played from.
    PlayRecent(String),
    PlayInContext {
        context: String,
        track: String,
    },
    /// Starts `uris[0]` inside `context` when the service accepts that, otherwise plays
    /// `uris` as a list. Used for "jump to this queue entry", which the API lacks.
    PlayFrom {
        context: Option<String>,
        uris: Vec<String>,
    },
    /// Plays a playlist, album, artist or liked songs from the start.
    PlayContext(String),
    LoadRadio(String),
    StartRadio(String),
    /// Asks for the queue again (the queue popup).
    LoadQueue,
    AddToQueue(String),
    /// Skips this many tracks: jumping into the queue keeps it intact, which a fresh
    /// play request would not.
    SkipAhead(usize),
    LoadDevices,
    /// Moves playback to a device, playing or not.
    Transfer {
        device: String,
        play: bool,
    },
    /// Loads the tracks behind a library or search entry.
    OpenList(Kind, String),
    Search(String),
    /// Adds a track to a playlist (any earlier copy is removed first).
    AddToPlaylist {
        playlist: String,
        track: String,
    },
    RemoveFromPlaylist {
        playlist: String,
        track: String,
    },
    /// Wakes a device (launching the player if needed) and resumes the last session;
    /// plays `fallback` (context, track) when there is no session to resume.
    Resume {
        fallback: Option<(Option<String>, String)>,
    },
    /// The screen shows this service (`true`) or the other one: a resting backend stops
    /// polling and reporting.
    Active(bool),
    /// pikap is closing: a backend that plays the music itself stops it. The sender
    /// answers once it is done.
    Quit(std::sync::mpsc::Sender<()>),
}

pub enum ResumeStage {
    Launching,
    Resumed(String),
    NoDevice,
}

pub enum Update {
    Playback(Option<Box<Playback>>),
    /// The tracks after the playing one.
    Queue(Vec<Track>),
    Liked(String, bool),
    Cover(String, PathBuf),
    Quota(Quota),
    Error(String),
    /// Whether the metadata source (artists, albums, lyrics) is available at all.
    Meta(bool),
    Artist(Arc<ArtistPage>),
    Album(Arc<AlbumPage>),
    /// A seed and the service's radio playlist for it.
    Radio(String, Arc<ListPage>),
    Lyrics(String, Option<Arc<LyricsPage>>),
    LastPlayed(Option<Box<LastPlayed>>),
    Resume(ResumeStage),
    /// The tracks of the context playback runs from.
    Context(Arc<ListPage>),
    /// A command was refused because the service put its endpoint on hold.
    OnHold,
    /// The macOS output volume changed (only while playing on this Mac).
    Volume(u8),
    Devices(Vec<Device>),
    /// A release cover on an artist page is on disk: (album uri, file).
    CardCover(String, PathBuf),
    /// All liked songs, for "liked by you" lists.
    LikedLibrary(Arc<Vec<TrackInfo>>),
    Library(Box<LibraryIndex>),
    /// The Made For You entries with their names, once they were read.
    MadeForYou(Vec<Entry>),
    /// An opened list, or a fresh copy of one after an edit.
    List(Arc<ListPage>),
    Search(Arc<SearchResults>),
    /// A playlist edit went through.
    Edited {
        playlist: String,
        track: String,
        added: bool,
    },
}
