//! Background task that owns Spotify access: polls playback over the Web API, follows
//! track changes (queue, liked state, cover, and through librespot the artist, album
//! and lyrics) and runs remote-control commands from the UI.
//!
//! Polling is the only steady cost against the Web API quota, so it slows down when
//! nothing plays and never runs more often than every three seconds. librespot calls
//! do not touch that quota.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::sync::mpsc::UnboundedReceiver;

use super::api::{self, Api, OnHold, Quota};
use super::config;
use super::meta::{AlbumPage, ArtistPage, ListPage, LyricsPage, Meta, TrackInfo};
use super::model::{
    Devices, Item, Page, PlayHistory, Playback, Playlist, Queue, Recent, SavedAlbum, SavedTrack,
    SearchResponse,
};
use crate::data::{Entry, Kind, LibraryIndex, SearchResults};
use crate::log::plog;

const POLL_PLAYING: Duration = Duration::from_secs(3);
const POLL_IDLE: Duration = Duration::from_secs(8);
/// Spotify needs a moment before a command shows up in the playback state.
const SETTLE: Duration = Duration::from_millis(450);
const COVER_PX: u32 = 640;
/// Development Mode caps search at ten results per type.
const SEARCH_LIMIT: usize = 10;
/// Jumps into the queue skip at most this many tracks (the queue shows twenty).
const MAX_SKIP: usize = 20;
/// Pause between skips so Spotify applies them in order.
const SKIP_GAP: Duration = Duration::from_millis(150);
const SPOTIFY_APP: &str = "/Applications/Spotify.app";
/// Spotify.app needs a few seconds after launch before it shows up as a Connect device.
const DEVICE_WAIT: Duration = Duration::from_millis(1500);
const DEVICE_TRIES: u32 = 12;
const LAUNCH_SETTLE: Duration = Duration::from_secs(3);
/// Checks after resuming; Spotify sometimes pauses a session it just resumed.
const RESUME_CHECKS: u32 = 8;
const MAX_REPLAYS: u32 = 2;
/// Poll interval while a resume is in flight, so the screen follows within a second.
const POLL_HURRY: Duration = Duration::from_secs(1);
/// How long fast polling continues after the resume flow itself has finished.
const HURRY_TAIL: Duration = Duration::from_secs(10);
/// How often the macOS output volume is read while Spotify plays on this Mac.
const SYSTEM_VOLUME_EVERY: Duration = Duration::from_millis(1500);

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
    /// Starts `uris[0]` inside `context` when Spotify accepts that, otherwise plays
    /// `uris` as a list. Used for "jump to this queue entry", which the API lacks.
    PlayFrom {
        context: Option<String>,
        uris: Vec<String>,
    },
    /// Plays a playlist, album, artist or liked songs from the start.
    PlayContext(String),
    LoadRadio(String),
    StartRadio(String),
    /// Asks for Spotify's queue again (the queue popup).
    LoadQueue,
    AddToQueue(String),
    /// Skips this many tracks: jumping into the queue keeps it intact, which a fresh
    /// play request would not.
    SkipAhead(usize),
    LoadDevices,
    /// Moves playback to a Connect device, playing or not.
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
    /// Wakes a device (launching Spotify.app if needed) and resumes the last session;
    /// plays `fallback` (context, track) when Spotify has no session to resume.
    Resume {
        fallback: Option<(Option<String>, String)>,
    },
}

pub enum ResumeStage {
    Launching,
    Resumed(String),
    NoDevice,
}

pub enum Update {
    Playback(Option<Box<Playback>>),
    Queue(Vec<Item>),
    Liked(String, bool),
    Cover(String, PathBuf),
    Quota(Quota),
    Error(String),
    /// Whether librespot metadata is available at all.
    Meta(bool),
    Artist(Arc<ArtistPage>),
    Album(Arc<AlbumPage>),
    /// A seed and Spotify's radio playlist for it.
    Radio(String, Arc<ListPage>),
    Lyrics(String, Option<Arc<LyricsPage>>),
    LastPlayed(Option<Box<PlayHistory>>),
    Resume(ResumeStage),
    /// The tracks of the context playback runs from.
    Context(Arc<ListPage>),
    /// A command was refused because Spotify put its endpoint on hold.
    OnHold,
    /// The macOS output volume changed (only while playing on this Mac).
    Volume(u8),
    Devices(Vec<super::model::Device>),
    /// A release cover on an artist page is on disk: (album uri, file).
    CardCover(String, PathBuf),
    /// All liked songs, for "liked by you" lists.
    LikedLibrary(Arc<Vec<TrackInfo>>),
    Library(Box<LibraryIndex>),
    /// The Made For You entries with their names, once librespot read them.
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

/// librespot session plus page caches, shared by the spawned metadata tasks.
#[derive(Default)]
struct Library {
    session: tokio::sync::Mutex<Option<Arc<Meta>>>,
    artists: Mutex<HashMap<String, Arc<ArtistPage>>>,
    albums: Mutex<HashMap<String, Arc<AlbumPage>>>,
    radios: Mutex<HashMap<String, Arc<ListPage>>>,
    /// Where each recently played track was played from.
    recent_contexts: Mutex<HashMap<String, String>>,
    lists: Mutex<HashMap<String, Arc<ListPage>>>,
    /// Liked songs, refreshed from the Web API once per run; the disk copy shows first.
    liked: tokio::sync::Mutex<Option<Arc<Vec<TrackInfo>>>>,
    /// Set while a resume is in flight; the poll loop speeds up until music plays.
    hurry: std::sync::atomic::AtomicBool,
    /// Spotify plays on this Mac: the volume shown and set is the system volume, which
    /// the volume keys change and Spotify neither reports nor follows.
    local: std::sync::atomic::AtomicBool,
}

impl Library {
    /// The live session, reconnecting when Spotify dropped it.
    async fn meta(&self) -> Option<Arc<Meta>> {
        let mut session = self.session.lock().await;
        if let Some(meta) = session.as_ref().filter(|m| m.is_alive()) {
            return Some(meta.clone());
        }
        let meta = Arc::new(Meta::connect().await.ok()?);
        *session = Some(meta.clone());
        Some(meta)
    }

    async fn artist(&self, uri: &str) -> Result<Arc<ArtistPage>> {
        if let Some(page) = self.artists.lock().ok().and_then(|c| c.get(uri).cloned()) {
            return Ok(page);
        }
        let meta = self.meta().await.context("librespot unavailable")?;
        let page = Arc::new(meta.artist(uri).await?);
        if let Ok(mut cache) = self.artists.lock() {
            cache.insert(uri.to_owned(), page.clone());
        }
        Ok(page)
    }

    async fn album(&self, uri: &str) -> Result<Arc<AlbumPage>> {
        if let Some(page) = self.albums.lock().ok().and_then(|c| c.get(uri).cloned()) {
            return Ok(page);
        }
        let meta = self.meta().await.context("librespot unavailable")?;
        let page = Arc::new(meta.album(uri).await?);
        if let Ok(mut cache) = self.albums.lock() {
            cache.insert(uri.to_owned(), page.clone());
        }
        Ok(page)
    }

    /// The liked-songs library: the disk copy is announced at once, then the Web API
    /// version (15 requests for ~700 songs) replaces it, once per run.
    async fn liked(&self, api: &Api, ui: &Sender<Update>) -> Result<Arc<Vec<TrackInfo>>> {
        let mut liked = self.liked.lock().await;
        if let Some(tracks) = liked.as_ref() {
            return Ok(tracks.clone());
        }
        let cached = read_liked_cache();
        if let Some(cached) = &cached {
            let _ = ui.send(Update::LikedLibrary(Arc::new(cached.clone())));
        }
        let tracks = match liked_songs(api, cached.as_deref()).await {
            Ok(Some(fresh)) => Arc::new(fresh),
            // Unchanged since the saved copy, or the endpoint is on hold: keep the copy.
            Ok(None) => Arc::new(cached.unwrap_or_default()),
            Err(err) => match cached {
                Some(cached) => {
                    plog!("liked songs: keeping the saved copy ({err:#})");
                    let cached = Arc::new(cached);
                    *liked = Some(cached.clone());
                    return Ok(cached);
                }
                None => return Err(err),
            },
        };
        write_liked_cache(&tracks);
        // Liked songs are usually most of a playlist; librespot need not look them up.
        if let Some(meta) = self.meta().await {
            meta.remember(&tracks);
        }
        let _ = ui.send(Update::LikedLibrary(tracks.clone()));
        *liked = Some(tracks.clone());
        Ok(tracks)
    }

    /// A track's album: from the track details librespot already knows, otherwise one
    /// Web API request.
    async fn album_of(&self, api: &Api, track: &str) -> Result<String> {
        if let Some(album) = self.meta().await.and_then(|m| m.album_of(track)) {
            return Ok(album);
        }
        let id = track.rsplit(':').next().unwrap_or_default();
        api.get::<Item>(&format!("tracks/{id}"))
            .await?
            .and_then(|item| item.album)
            .map(|album| album.uri)
            .filter(|uri| !uri.is_empty())
            .with_context(|| format!("no album for {track}"))
    }

    /// Spotify's own radio playlist for a seed, with its tracks: what shows is what
    /// plays, as a context.
    async fn radio(&self, seed: &str) -> Result<Arc<ListPage>> {
        if let Some(radio) = self.radios.lock().ok().and_then(|c| c.get(seed).cloned()) {
            return Ok(radio);
        }
        let meta = self.meta().await.context("librespot unavailable")?;
        let playlist = meta.radio_playlist(seed).await?;
        let radio = Arc::new(meta.playlist(&playlist).await?);
        if let Ok(mut cache) = self.radios.lock() {
            cache.insert(seed.to_owned(), radio.clone());
        }
        Ok(radio)
    }
}

/// Loads the list behind a playback context and sends it, from cache when possible.
fn spawn_context(
    api: &Arc<Api>,
    library: &Arc<Library>,
    ui: &Sender<Update>,
    kind: &str,
    uri: String,
) {
    let Some(kind) = Kind::from_context(kind) else {
        return;
    };
    let (api, library, ui) = (api.clone(), library.clone(), ui.clone());
    tokio::spawn(async move {
        match list_page(&api, &library, &ui, kind, &uri).await {
            Ok(page) => drop(ui.send(Update::Context(page))),
            Err(err) => report(&ui, &err),
        }
    });
}

/// The tracks behind a context or a library entry, cached per URI. Recently played
/// changes all the time and is always asked again.
async fn list_page(
    api: &Api,
    library: &Library,
    ui: &Sender<Update>,
    kind: Kind,
    uri: &str,
) -> Result<Arc<ListPage>> {
    let cached = library.lists.lock().ok().and_then(|c| c.get(uri).cloned());
    if let Some(page) = cached.filter(|_| kind != Kind::Recent) {
        return Ok(page);
    }
    let named = |name: &str, tracks: Vec<TrackInfo>| ListPage {
        uri: uri.to_owned(),
        name: name.to_owned(),
        tracks,
    };
    let page = match kind {
        Kind::Liked => named("", library.liked(api, ui).await?.as_ref().clone()),
        Kind::Album => {
            let album = library.album(uri).await?;
            named(&album.name, album.tracks.clone())
        }
        Kind::Artist => {
            let artist = library.artist(uri).await?;
            named(&artist.name, artist.top.clone())
        }
        Kind::Playlist => {
            // Lets a liked-songs fetch in flight finish first: it seeds the track
            // details, so most of the playlist needs no lookup.
            drop(library.liked.lock().await);
            let meta = library.meta().await.context("librespot unavailable")?;
            meta.playlist(uri).await?
        }
        Kind::Recent => named("", recently_played(api, library).await?),
        Kind::Heading => bail!("nothing to open"),
    };
    plog!("list {kind:?} {uri}: {} tracks", page.tracks.len());
    let page = Arc::new(page);
    if kind != Kind::Recent
        && let Ok(mut cache) = library.lists.lock()
    {
        cache.insert(uri.to_owned(), page.clone());
    }
    Ok(page)
}

/// Every item of a paged Web API list. Development Mode rejects large pages on some
/// accounts, so the page size drops to 10 on an "invalid limit" answer.
async fn all_pages<T: DeserializeOwned>(api: &Api, path: &str) -> Result<Vec<T>> {
    let mut items = Vec::new();
    let mut offset = 0;
    let mut limit = 50;
    loop {
        let url = format!("{path}?limit={limit}&offset={offset}");
        let page = match api.get::<Page<T>>(&url).await {
            Ok(page) => page,
            Err(err) if limit > 10 && format!("{err:#}").to_lowercase().contains("limit") => {
                limit = 10;
                continue;
            }
            Err(err) => return Err(err),
        };
        let Some(page) = page else { break };
        let done = page.next.is_none() || page.items.is_empty();
        offset += page.items.len();
        items.extend(page.items.into_iter().flatten());
        if done {
            break;
        }
    }
    Ok(items)
}

/// The last fifty tracks played, each once, newest first.
/// The contexts are remembered: a track chosen from this list plays where it was
/// played from.
async fn recently_played(api: &Api, library: &Library) -> Result<Vec<TrackInfo>> {
    let recent = api
        .get::<Recent>("me/player/recently-played?limit=50")
        .await?
        .unwrap_or_default();
    if let Ok(mut contexts) = library.recent_contexts.lock() {
        for h in recent.items.iter().rev() {
            let context = h
                .context
                .as_ref()
                .filter(|c| matches!(c.kind.as_str(), "playlist" | "album" | "collection"));
            match context {
                Some(c) => contexts.insert(h.track.uri.clone(), c.uri.clone()),
                None => contexts.remove(&h.track.uri),
            };
        }
    }
    let mut seen = std::collections::HashSet::new();
    Ok(recent
        .items
        .iter()
        .filter(|h| seen.insert(h.track.uri.clone()))
        .map(|h| h.track.info())
        .collect())
}

/// Playlists and saved albums (two requests for a typical library), plus the
/// configured Made For You lists, named later by librespot. Each part loads on its
/// own: one on hold keeps its saved copy instead of failing the rest.
async fn library_index(api: &Api, user: String, saved: Option<&LibraryIndex>) -> LibraryIndex {
    let mut held: Option<Duration> = None;
    let mut keep = |err: anyhow::Error, part: &str, saved: Option<&Vec<Entry>>| {
        plog!("library {part}: {err:#}");
        let saved = saved.cloned().unwrap_or_default();
        if saved.is_empty()
            && let Some(OnHold(wait)) = err.downcast_ref::<OnHold>()
        {
            held = Some(held.map_or(*wait, |h| h.max(*wait)));
        }
        saved
    };
    let made_for_you: Vec<Entry> = api
        .config()
        .made_for_you
        .iter()
        .filter_map(|url| playlist_uri(url))
        .map(|uri| {
            saved
                .and_then(|s| s.made_for_you.iter().find(|e| e.uri == uri).cloned())
                .unwrap_or_else(|| Entry::new(Kind::Playlist, uri, ""))
        })
        .collect();
    let playlists = match all_pages::<Playlist>(api, "me/playlists").await {
        Ok(lists) => lists
            .into_iter()
            .filter(|p| !made_for_you.iter().any(|m| m.uri == p.uri))
            .map(|p| Entry {
                editable: p.collaborative || p.owner.id == user,
                sub: p.owner.display_name.unwrap_or_default(),
                count: Some(p.items.total),
                ..Entry::new(Kind::Playlist, p.uri, p.name)
            })
            .collect(),
        Err(err) => keep(err, "playlists", saved.map(|s| &s.playlists)),
    };
    let albums = match all_pages::<SavedAlbum>(api, "me/albums").await {
        Ok(albums) => albums
            .into_iter()
            .map(|s| Entry {
                sub: artist_names(&s.album.artists),
                artist: first_artist(&s.album.artists),
                ..Entry::new(Kind::Album, s.album.uri, s.album.name)
            })
            .collect(),
        Err(err) => keep(err, "albums", saved.map(|s| &s.albums)),
    };
    LibraryIndex {
        user,
        made_for_you,
        playlists,
        albums,
        held,
    }
}

fn library_path() -> PathBuf {
    config::cache_dir().join("library.json")
}

fn read_library() -> Option<LibraryIndex> {
    serde_json::from_slice(&std::fs::read(library_path()).ok()?).ok()
}

fn write_library(index: &LibraryIndex) {
    let path = library_path();
    let tmp = path.with_extension("json.tmp");
    if let Ok(bytes) = serde_json::to_vec(index)
        && std::fs::write(&tmp, bytes).is_ok()
    {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn first_artist(artists: &[super::model::Artist]) -> Option<(String, String)> {
    artists
        .iter()
        .find(|a| !a.uri.is_empty())
        .map(|a| (a.uri.clone(), a.name.clone()))
}

fn artist_names(artists: &[super::model::Artist]) -> String {
    artists
        .iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `spotify:playlist:…` from a URI or an `open.spotify.com` link.
fn playlist_uri(text: &str) -> Option<String> {
    let text = text.trim();
    let id = match text.strip_prefix("spotify:playlist:") {
        Some(id) => id,
        None => text.split("/playlist/").nth(1)?.split(['?', '/']).next()?,
    };
    (!id.is_empty()).then(|| format!("spotify:playlist:{id}"))
}

/// One request for all four kinds; Development Mode caps each at ten results.
async fn search(api: &Api, query: &str) -> Result<SearchResults> {
    let path = format!(
        "search?q={}&type=track,album,artist,playlist&limit={SEARCH_LIMIT}",
        percent_encode(query)
    );
    let found = api.get::<SearchResponse>(&path).await?.unwrap_or_default();
    Ok(SearchResults {
        query: query.to_owned(),
        tracks: found
            .tracks
            .items
            .iter()
            .flatten()
            .map(Item::info)
            .collect(),
        albums: found
            .albums
            .items
            .into_iter()
            .flatten()
            .map(|a| Entry {
                sub: artist_names(&a.artists),
                artist: first_artist(&a.artists),
                ..Entry::new(Kind::Album, a.uri, a.name)
            })
            .collect(),
        artists: found
            .artists
            .items
            .into_iter()
            .flatten()
            .map(|a| Entry::new(Kind::Artist, a.uri, a.name))
            .collect(),
        playlists: found
            .playlists
            .items
            .into_iter()
            .flatten()
            .map(|p| Entry {
                sub: p.owner.display_name.unwrap_or_default(),
                count: Some(p.items.total),
                ..Entry::new(Kind::Playlist, p.uri, p.name)
            })
            .collect(),
    })
}

fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Fork lesson: removing every occurrence before adding keeps a track from showing
/// twice. After adding, the playlist is read again so the new track shows with its
/// details; after removing, the cached copy is trimmed in place instead.
async fn edit_playlist(
    api: &Api,
    library: &Library,
    ui: &Sender<Update>,
    playlist: String,
    track: String,
    add: bool,
) -> Result<()> {
    let id = playlist.rsplit(':').next().unwrap_or_default();
    let path = format!("playlists/{id}/items");
    api.send_json(
        Method::DELETE,
        &path,
        &json!({ "items": [{ "uri": track }] }),
    )
    .await?;
    if add {
        api.send_json(Method::POST, &path, &json!({ "uris": [track] }))
            .await?;
    }
    let cached = library
        .lists
        .lock()
        .ok()
        .and_then(|mut c| c.remove(&playlist));
    if add {
        if let Ok(page) = list_page(api, library, ui, Kind::Playlist, &playlist).await {
            let _ = ui.send(Update::List(page));
        }
    } else if let Some(old) = cached {
        let page = Arc::new(ListPage {
            uri: old.uri.clone(),
            name: old.name.clone(),
            tracks: old
                .tracks
                .iter()
                .filter(|t| t.uri != track)
                .cloned()
                .collect(),
        });
        if let Ok(mut cache) = library.lists.lock() {
            cache.insert(playlist.clone(), page);
        }
    }
    plog!(
        "playlist {playlist}: {} {track}",
        if add { "added" } else { "removed" }
    );
    let _ = ui.send(Update::Edited {
        playlist,
        track,
        added: add,
    });
    Ok(())
}

/// All liked songs, or `None` when the saved copy is still current: one request
/// compares the count and the newest song before fifteen pages are read again.
async fn liked_songs(api: &Api, saved: Option<&[TrackInfo]>) -> Result<Option<Vec<TrackInfo>>> {
    if let Some(saved) = saved.filter(|s| !s.is_empty()) {
        let head = api
            .get::<Page<SavedTrack>>("me/tracks?limit=1")
            .await?
            .unwrap_or_default();
        let newest = head
            .items
            .first()
            .and_then(|i| i.as_ref())
            .map(|s| &s.track.uri);
        if head.total == Some(saved.len()) && newest == Some(&saved[0].uri) {
            plog!("liked songs: saved copy is current ({} songs)", saved.len());
            return Ok(None);
        }
    }
    Ok(Some(
        all_pages::<SavedTrack>(api, "me/tracks")
            .await?
            .iter()
            .map(|s| s.track.info())
            .collect(),
    ))
}

fn liked_cache_path() -> PathBuf {
    config::cache_dir().join("liked.json")
}

fn read_liked_cache() -> Option<Vec<TrackInfo>> {
    serde_json::from_slice(&std::fs::read(liked_cache_path()).ok()?).ok()
}

fn write_liked_cache(tracks: &[TrackInfo]) {
    let path = liked_cache_path();
    let tmp = path.with_extension("json.tmp");
    if let Ok(bytes) = serde_json::to_vec(tracks)
        && std::fs::write(&tmp, bytes).is_ok()
    {
        let _ = std::fs::rename(&tmp, &path);
    }
}

pub async fn run(api: Arc<Api>, mut commands: UnboundedReceiver<Command>, ui: Sender<Update>) {
    let library = Arc::new(Library::default());
    let has_meta = library.meta().await.is_some();
    let _ = ui.send(Update::Meta(has_meta));
    {
        // One after another, so startup never bursts the Web API quota.
        let (api, library, ui) = (api.clone(), library.clone(), ui.clone());
        tokio::spawn(async move {
            // The saved library shows at once; the fresh one replaces it.
            let saved = read_library();
            if let Some(saved) = &saved {
                let _ = ui.send(Update::Library(Box::new(saved.clone())));
            }
            let user = match library.meta().await {
                Some(meta) => meta.user(),
                None => saved.as_ref().map(|s| s.user.clone()).unwrap_or_default(),
            };
            let mut index = library_index(&api, user, saved.as_ref()).await;
            write_library(&index);
            let _ = ui.send(Update::Library(Box::new(index.clone())));
            if let Err(err) = library.liked(&api, &ui).await {
                report(&ui, &err);
            }
            if let Some(named) = name_made_for_you(&library, &ui, index.made_for_you.clone()).await
            {
                index.made_for_you = named;
                write_library(&index);
            }
        });
    }

    // Commands run in their own tasks; finishing one polls again right away instead of
    // waiting out the idle interval (resuming can take several seconds).
    let poke = Arc::new(tokio::sync::Notify::new());
    let computer = computer_name();
    watch_system_volume(&library, &ui);
    let mut current: Option<String> = None;
    let mut current_context: Option<String> = None;
    let mut asked_last_played = false;
    let mut last_summary = None;
    let mut next_poll = Duration::ZERO;
    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break };
                handle(&api, &library, &ui, &poke, command);
                next_poll = SETTLE;
            }
            () = poke.notified() => {
                next_poll = SETTLE;
                continue;
            }
            () = tokio::time::sleep(next_poll) => {
                let mut playing = false;
                match api.get::<Playback>("me/player?additional_types=episode").await {
                    Ok(mut playback) => {
                        playing = playback.as_ref().is_some_and(|p| p.is_playing);
                        let local = playback.as_ref().is_some_and(|p| {
                            p.device.kind == "Computer"
                                && computer.as_deref() == Some(p.device.name.trim())
                        });
                        library.local.store(local, Ordering::Relaxed);
                        if local
                            && let Some(p) = playback.as_mut()
                            && let Ok(Some(volume)) = tokio::task::spawn_blocking(system_volume).await
                        {
                            p.device.volume_percent = Some(volume);
                        }
                        let summary = playback.as_ref().map(|p| (
                            p.device.name.clone(),
                            p.is_playing,
                            p.item.as_ref().map(|i| i.name.clone()),
                        ));
                        if summary != last_summary {
                            plog!("playback: {summary:?}");
                            last_summary = summary;
                        }
                        let item = playback.as_ref().and_then(|p| p.item.clone());
                        let context = playback
                            .as_ref()
                            .and_then(|p| p.context.clone())
                            .filter(|c| !c.uri.is_empty());
                        if context.as_ref().map(|c| &c.uri) != current_context.as_ref() {
                            current_context = context.as_ref().map(|c| c.uri.clone());
                            if let Some(c) = context {
                                spawn_context(&api, &library, &ui, &c.kind, c.uri);
                            }
                        }
                        if ui.send(Update::Playback(playback.map(Box::new))).is_err() {
                            break;
                        }
                        let uri = item.as_ref().map(|i| i.uri.clone());
                        if uri.is_none() && !asked_last_played {
                            asked_last_played = true;
                            match api.get::<Recent>("me/player/recently-played?limit=1").await {
                                Ok(recent) => {
                                    let last = recent.and_then(|r| r.items.into_iter().next());
                                    let _ = ui.send(Update::LastPlayed(last.map(Box::new)));
                                }
                                Err(err) => report(&ui, &err),
                            }
                        }
                        if uri.is_some() {
                            asked_last_played = false;
                        }
                        if uri != current {
                            current = uri;
                            if let Some(item) = item {
                                follow_track(&api, &library, &item, &ui).await;
                            }
                        }
                    }
                    Err(err) => report(&ui, &err),
                }
                next_poll = if playing {
                    library.hurry.store(false, Ordering::Relaxed);
                    POLL_PLAYING
                } else if library.hurry.load(Ordering::Relaxed) {
                    POLL_HURRY
                } else {
                    POLL_IDLE
                };
            }
        }
        let _ = ui.send(Update::Quota(api.quota()));
    }
}

/// Runs a command in its own task so a slow librespot call never delays polling.
fn handle(
    api: &Arc<Api>,
    library: &Arc<Library>,
    ui: &Sender<Update>,
    poke: &Arc<tokio::sync::Notify>,
    command: Command,
) {
    let (api, library, ui, poke) = (api.clone(), library.clone(), ui.clone(), poke.clone());
    tokio::spawn(async move {
        let result = match command {
            Command::SetPlaying(play) => {
                let path = if play {
                    "me/player/play"
                } else {
                    "me/player/pause"
                };
                api.send(Method::PUT, path).await
            }
            Command::Next => api.send(Method::POST, "me/player/next").await,
            Command::Previous => api.send(Method::POST, "me/player/previous").await,
            Command::SeekTo(ms) => {
                api.send(Method::PUT, &format!("me/player/seek?position_ms={ms}"))
                    .await
            }
            Command::Volume(v) if library.local.load(Ordering::Relaxed) => {
                let _ = tokio::task::spawn_blocking(move || set_system_volume(v)).await;
                Ok(())
            }
            Command::Volume(v) => {
                api.send(Method::PUT, &format!("me/player/volume?volume_percent={v}"))
                    .await
            }
            Command::SetLiked(uri, liked) => {
                let method = if liked { Method::PUT } else { Method::DELETE };
                let result = api.send(method, &format!("me/library?uris={uri}")).await;
                if result.is_err() {
                    // The heart was set ahead of the answer; put it back.
                    let _ = ui.send(Update::Liked(uri, !liked));
                }
                result
            }
            Command::SetShuffle(on) => {
                api.send(Method::PUT, &format!("me/player/shuffle?state={on}"))
                    .await
            }
            Command::SetRepeat(state) => {
                api.send(Method::PUT, &format!("me/player/repeat?state={state}"))
                    .await
            }
            Command::PlayTrack(track) => play_uris(&api, &library, &[track]).await,
            Command::PlayRecent(track) => {
                let context = library
                    .recent_contexts
                    .lock()
                    .ok()
                    .and_then(|c| c.get(&track).cloned());
                match context {
                    Some(context) => {
                        play(
                            &api,
                            &json!({ "context_uri": context, "offset": { "uri": track } }),
                        )
                        .await
                    }
                    None => play_uris(&api, &library, &[track]).await,
                }
            }
            Command::PlayInContext { context, track } => {
                play(
                    &api,
                    &json!({ "context_uri": context, "offset": { "uri": track } }),
                )
                .await
            }
            Command::PlayFrom { context, uris } => {
                let in_context = match (context, uris.first()) {
                    (Some(context), Some(track)) => {
                        let body = json!({ "context_uri": context, "offset": { "uri": track } });
                        play(&api, &body).await.is_ok()
                    }
                    _ => false,
                };
                if in_context {
                    Ok(())
                } else {
                    play_uris(&api, &library, &uris).await
                }
            }
            Command::Resume { fallback } => {
                library.hurry.store(true, Ordering::Relaxed);
                let result = resume(&api, &library, &ui, fallback).await;
                tokio::time::sleep(HURRY_TAIL).await;
                library.hurry.store(false, Ordering::Relaxed);
                result
            }
            Command::PlayContext(uri) => play(&api, &json!({ "context_uri": uri })).await,
            Command::OpenList(Kind::Artist, uri) => {
                artist_page(&api, &library, &ui, &uri).await;
                Ok(())
            }
            Command::OpenList(kind, uri) => list_page(&api, &library, &ui, kind, &uri)
                .await
                .map(|page| drop(ui.send(Update::List(page)))),
            Command::Search(query) => search(&api, &query)
                .await
                .map(|found| drop(ui.send(Update::Search(Arc::new(found))))),
            Command::AddToPlaylist { playlist, track } => {
                edit_playlist(&api, &library, &ui, playlist, track, true).await
            }
            Command::RemoveFromPlaylist { playlist, track } => {
                edit_playlist(&api, &library, &ui, playlist, track, false).await
            }
            Command::LoadQueue => send_queue(&api, &ui).await,
            Command::AddToQueue(track) => {
                let result = api
                    .send(Method::POST, &format!("me/player/queue?uri={track}"))
                    .await;
                if result.is_ok() {
                    tokio::time::sleep(SETTLE).await;
                    let _ = send_queue(&api, &ui).await;
                }
                result
            }
            Command::SkipAhead(n) => {
                let mut result = Ok(());
                for _ in 0..n.min(MAX_SKIP) {
                    result = api.send(Method::POST, "me/player/next").await;
                    if result.is_err() {
                        break;
                    }
                    tokio::time::sleep(SKIP_GAP).await;
                }
                result
            }
            Command::LoadDevices => api.get::<Devices>("me/player/devices").await.map(|d| {
                let _ = ui.send(Update::Devices(d.map(|d| d.devices).unwrap_or_default()));
            }),
            Command::Transfer { device, play } => {
                let body = json!({ "device_ids": [device], "play": play });
                api.send_json(Method::PUT, "me/player", &body).await
            }
            Command::LoadRadio(seed) => library.radio(&seed).await.map(|tracks| {
                let _ = ui.send(Update::Radio(seed, tracks));
            }),
            Command::StartRadio(seed) => match library.radio(&seed).await {
                Ok(radio) => {
                    let context = radio.uri.clone();
                    let _ = ui.send(Update::Radio(seed, radio));
                    play(&api, &json!({ "context_uri": context })).await
                }
                Err(err) => Err(err),
            },
        };
        match result {
            // Something the user asked for: unlike background reads, say why it did not happen.
            Err(err) if api::is_on_hold(&err) => drop(ui.send(Update::OnHold)),
            Err(err) => report(&ui, &err),
            Ok(()) => {}
        }
        poke.notify_one();
    });
}

/// Made For You lists come from the config as bare URIs; librespot knows their names.
async fn name_made_for_you(
    library: &Library,
    ui: &Sender<Update>,
    mut entries: Vec<Entry>,
) -> Option<Vec<Entry>> {
    if entries.is_empty() {
        return None;
    }
    let meta = library.meta().await?;
    for entry in &mut entries {
        match meta.playlist_summary(&entry.uri).await {
            Ok((name, count)) => {
                entry.title = name;
                entry.count = Some(count);
            }
            Err(err) => plog!("made for you {}: {err}", entry.uri),
        }
    }
    let _ = ui.send(Update::MadeForYou(entries.clone()));
    Some(entries)
}

async fn resume(
    api: &Api,
    library: &Library,
    ui: &Sender<Update>,
    fallback: Option<(Option<String>, String)>,
) -> Result<()> {
    let mut device = pick_device(api).await?;
    plog!("resume: device before launch: {device:?}");
    let launched = device.is_none() && std::path::Path::new(SPOTIFY_APP).exists();
    if launched {
        let _ = ui.send(Update::Resume(ResumeStage::Launching));
        let terminal = frontmost_bundle();
        plog!("resume: launching Spotify, terminal app {terminal:?}");
        let _ = std::process::Command::new("open")
            .args(["-g", "-j", "-a", "Spotify"])
            .status();
        for _ in 0..DEVICE_TRIES {
            tokio::time::sleep(DEVICE_WAIT).await;
            refocus(terminal.as_deref());
            device = pick_device(api).await?;
            plog!("resume: device: {device:?}");
            if device.is_some() {
                break;
            }
        }
        // Being listed as a Connect device does not mean the app has finished starting:
        // it still restores its own (paused) session and would override ours.
        tokio::time::sleep(LAUNCH_SETTLE).await;
        refocus(terminal.as_deref());
    }
    let Some((id, name)) = device else {
        let _ = ui.send(Update::Resume(ResumeStage::NoDevice));
        return Ok(());
    };
    // Transferring with `play` resumes Spotify's last session, position included.
    let body = json!({ "device_ids": [id], "play": true });
    plog!("resume: transfer to {name} with play");
    api.send_json(Method::PUT, "me/player", &body).await?;
    let _ = ui.send(Update::Resume(ResumeStage::Resumed(name)));

    let play_here = format!("me/player/play?device_id={id}");
    let mut replays = 0;
    for attempt in 0..RESUME_CHECKS {
        tokio::time::sleep(DEVICE_WAIT).await;
        let playback = api.get::<Playback>("me/player").await?;
        plog!(
            "resume: check {attempt}: {}",
            playback
                .as_ref()
                .map_or("no playback (204)".to_owned(), |p| format!(
                    "playing={} item={} device={} active={}",
                    p.is_playing,
                    p.item.as_ref().map_or("-", |i| i.name.as_str()),
                    p.device.name,
                    p.device.is_active
                ))
        );
        match playback {
            Some(p) if p.is_playing && p.item.is_some() => return Ok(()),
            // Loaded but paused: Spotify restored its own state over ours.
            Some(p) if p.item.is_some() => {
                if replays < MAX_REPLAYS {
                    replays += 1;
                    api.send(Method::PUT, &play_here).await?;
                }
            }
            // Nothing loaded yet; a fresh app can take ~10 s to restore the session,
            // so the fallback waits for the last check instead of racing it.
            _ if attempt + 1 == RESUME_CHECKS => {
                if let Some((context, track)) = fallback.as_ref() {
                    plog!("resume: no session, playing the last played track");
                    let context = match context {
                        Some(context) => context.clone(),
                        None => library.album_of(api, track).await?,
                    };
                    let body = json!({ "context_uri": context, "offset": { "uri": track } });
                    return api.send_json(Method::PUT, &play_here, &body).await;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// This Mac's name, which Spotify.app also uses as its Connect device name.
fn computer_name() -> Option<String> {
    let out = std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!name.is_empty()).then_some(name)
}

/// macOS output volume, 0 when muted.
fn system_volume() -> Option<u8> {
    let out = std::process::Command::new("osascript")
        .args(["-e", "get volume settings"])
        .output()
        .ok()?;
    parse_volume_settings(&String::from_utf8_lossy(&out.stdout))
}

/// `output volume:31, input volume:50, alert volume:100, output muted:false`
fn parse_volume_settings(text: &str) -> Option<u8> {
    let field = |name: &str| {
        text.split(',')
            .find_map(|part| part.trim().strip_prefix(name)?.strip_prefix(':'))
            .map(str::trim)
    };
    if field("output muted") == Some("true") {
        return Some(0);
    }
    field("output volume")?.parse().ok()
}

fn set_system_volume(volume: u8) {
    let _ = std::process::Command::new("osascript")
        .args(["-e", &format!("set volume output volume {volume}")])
        .status();
}

/// Follows the volume keys while Spotify plays on this Mac, without any API request.
fn watch_system_volume(library: &Arc<Library>, ui: &Sender<Update>) {
    let (library, ui) = (library.clone(), ui.clone());
    tokio::spawn(async move {
        let mut last = None;
        loop {
            tokio::time::sleep(SYSTEM_VOLUME_EVERY).await;
            if !library.local.load(Ordering::Relaxed) {
                last = None;
                continue;
            }
            let volume = tokio::task::spawn_blocking(system_volume)
                .await
                .ok()
                .flatten();
            if volume.is_some() && volume != last {
                last = volume;
                if volume.is_some_and(|v| ui.send(Update::Volume(v)).is_err()) {
                    return;
                }
            }
        }
    });
}

/// Bundle id of the app in front (the terminal pikap runs in), via `lsappinfo`,
/// which needs no automation permission.
fn frontmost_bundle() -> Option<String> {
    frontmost_raw().filter(|id| !id.is_empty() && id != "com.spotify.client")
}

fn frontmost_raw() -> Option<String> {
    let front = std::process::Command::new("lsappinfo")
        .arg("front")
        .output()
        .ok()?;
    let asn = String::from_utf8_lossy(&front.stdout).trim().to_owned();
    let info = std::process::Command::new("lsappinfo")
        .args(["info", "-only", "bundleid", &asn])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&info.stdout);
    Some(
        text.split("bundleID=\"")
            .nth(1)?
            .split('"')
            .next()?
            .to_owned(),
    )
}

/// Spotify activates itself once its window is up; hand the focus back to the terminal.
fn refocus(bundle: Option<&str>) {
    plog!("refocus {bundle:?}, front is {:?}", frontmost_raw());
    if let Some(bundle) = bundle {
        let _ = std::process::Command::new("open")
            .args(["-b", bundle])
            .status();
    }
}

/// A controllable device, preferring this computer over phones and speakers.
async fn pick_device(api: &Api) -> Result<Option<(String, String)>> {
    let devices = api
        .get::<Devices>("me/player/devices")
        .await?
        .map(|d| d.devices)
        .unwrap_or_default();
    let usable = |d: &&super::model::Device| d.id.is_some() && !d.is_restricted;
    let best = devices
        .iter()
        .filter(usable)
        .find(|d| d.is_active)
        .or_else(|| devices.iter().filter(usable).find(|d| d.kind == "Computer"))
        .or_else(|| devices.iter().find(usable));
    Ok(best.and_then(|d| Some((d.id.clone()?, d.name.clone()))))
}

/// Plays the first of `uris` inside its album. Never as a `uris` list: Spotify.app
/// (October 2026) accepts such a request and then locks up with no track and every
/// action disallowed, even for a single URI; context requests work and unlock it.
/// Lists that are contexts play as such (radio is Spotify's radio playlist, recent
/// tracks their own context); a lone track gets its album.
async fn play_uris(api: &Api, library: &Library, uris: &[String]) -> Result<()> {
    let Some(track) = uris.first() else {
        return Ok(());
    };
    let album = library.album_of(api, track).await?;
    play(
        api,
        &json!({ "context_uri": album, "offset": { "uri": track } }),
    )
    .await
}

/// Starts playback. With no active device Spotify answers 404; the request then goes
/// straight to the best device (Spotify.app open but idle, for example).
async fn play(api: &Api, body: &serde_json::Value) -> Result<()> {
    plog!(
        "play: context {} offset {} uris {} (first {})",
        body["context_uri"],
        body["offset"]["uri"],
        body["uris"].as_array().map_or(0, Vec::len),
        body["uris"][0]
    );
    match api.send_json(Method::PUT, "me/player/play", body).await {
        Err(err) if format!("{err:#}").contains("404") => {
            let Some((id, name)) = pick_device(api).await? else {
                return Err(err);
            };
            plog!("play: no active device, sending to {name}");
            api.send_json(Method::PUT, &format!("me/player/play?device_id={id}"), body)
                .await
        }
        other => other,
    }
}

async fn send_queue(api: &Api, ui: &Sender<Update>) -> Result<()> {
    let queue = api.get::<Queue>("me/player/queue").await?;
    let _ = ui.send(Update::Queue(queue.map(|q| q.queue).unwrap_or_default()));
    Ok(())
}

async fn follow_track(api: &Arc<Api>, library: &Arc<Library>, item: &Item, ui: &Sender<Update>) {
    spawn_metadata(api, library, item, ui);
    match api.get::<Queue>("me/player/queue").await {
        Ok(queue) => {
            let items = queue.map(|q| q.queue).unwrap_or_default();
            let _ = ui.send(Update::Queue(items));
        }
        Err(err) => report(ui, &err),
    }
    if item.uri.starts_with("spotify:track:") {
        match api
            .get::<Vec<bool>>(&format!("me/library/contains?uris={}", item.uri))
            .await
        {
            Ok(Some(flags)) => {
                let liked = flags.first().copied().unwrap_or(false);
                let _ = ui.send(Update::Liked(item.uri.clone(), liked));
            }
            Ok(None) => {}
            // On hold: the saved liked songs answer instead.
            Err(err) if api::is_on_hold(&err) => {
                let saved = library.liked.try_lock().ok().and_then(|l| l.clone());
                if let Some(saved) = saved {
                    let liked = saved.iter().any(|t| t.uri == item.uri);
                    let _ = ui.send(Update::Liked(item.uri.clone(), liked));
                }
            }
            Err(err) => report(ui, &err),
        }
    }
    if let Some((id, url)) = item.cover(COVER_PX) {
        match cover_file(api, &id, &url).await {
            Ok(path) => {
                let _ = ui.send(Update::Cover(item.uri.clone(), path));
            }
            Err(err) => report(ui, &err),
        }
    }
}

/// An artist's page in three steps, each sent as it is ready: popular tracks and
/// related artists, then the discography, then the release covers (image CDN, no
/// quota). Pages are cached with their discography, so this runs once per artist.
async fn artist_page(api: &Api, library: &Library, ui: &Sender<Update>, uri: &str) {
    let page = match library.artist(uri).await {
        Ok(page) => page,
        Err(err) => return report(ui, &err),
    };
    let _ = ui.send(Update::Artist(page.clone()));
    let page = if page.discography.is_empty() && !page.releases.is_empty() {
        let Some(meta) = library.meta().await else {
            return;
        };
        let cards = meta.cards(&page.releases).await;
        let full = Arc::new(ArtistPage {
            uri: page.uri.clone(),
            name: page.name.clone(),
            top: page.top.clone(),
            related: page.related.clone(),
            popularity: page.popularity,
            releases: page.releases.clone(),
            discography: cards,
        });
        if let Ok(mut cache) = library.artists.lock() {
            cache.insert(uri.to_owned(), full.clone());
        }
        let _ = ui.send(Update::Artist(full.clone()));
        full
    } else {
        page
    };
    for card in &page.discography {
        let Some(url) = &card.cover else { continue };
        let id = format!("card-{}", url.rsplit('/').next().unwrap_or_default());
        match cover_file(api, &id, url).await {
            Ok(path) => drop(ui.send(Update::CardCover(card.uri.clone(), path))),
            Err(err) => plog!("cover {}: {err:#}", card.uri),
        }
    }
}

/// Artist page, album page and lyrics for the new track, each in its own task.
fn spawn_metadata(api: &Arc<Api>, library: &Arc<Library>, item: &Item, ui: &Sender<Update>) {
    if !item.uri.starts_with("spotify:track:") {
        return;
    }
    if let Some(artist) = item.artists.first().filter(|a| !a.uri.is_empty()) {
        let (api, library, ui) = (api.clone(), library.clone(), ui.clone());
        let uri = artist.uri.clone();
        tokio::spawn(async move { artist_page(&api, &library, &ui, &uri).await });
    }
    if let Some(album) = item.album.as_ref().filter(|a| !a.uri.is_empty()) {
        let (library, ui, uri) = (library.clone(), ui.clone(), album.uri.clone());
        tokio::spawn(async move {
            match library.album(&uri).await {
                Ok(page) => drop(ui.send(Update::Album(page))),
                Err(err) => report(&ui, &err),
            }
        });
    }
    let (library, ui, uri) = (library.clone(), ui.clone(), item.uri.clone());
    tokio::spawn(async move {
        let Some(meta) = library.meta().await else {
            return;
        };
        match meta.lyrics(&uri).await {
            Ok(page) => drop(ui.send(Update::Lyrics(uri, page.map(Arc::new)))),
            Err(err) => report(&ui, &err),
        }
    });
}

/// Covers are cached by album id; image CDN requests do not count against the API quota.
async fn cover_file(api: &Api, id: &str, url: &str) -> Result<PathBuf> {
    let dir = config::cache_dir().join("covers");
    let path = dir.join(format!("{id}.jpg"));
    if path.exists() {
        return Ok(path);
    }
    std::fs::create_dir_all(&dir)?;
    let bytes = api
        .http()
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

/// On-hold endpoints are expected under Development Mode quotas; they show up in the
/// quota indicator rather than as an error.
fn report(ui: &Sender<Update>, err: &anyhow::Error) {
    if !api::is_on_hold(err) {
        let _ = ui.send(Update::Error(format!("{err:#}")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_links() {
        assert_eq!(
            playlist_uri("https://open.spotify.com/playlist/37i9dQZF1E36c3f87B1Xo0?si=ab")
                .as_deref(),
            Some("spotify:playlist:37i9dQZF1E36c3f87B1Xo0")
        );
        assert_eq!(
            playlist_uri("spotify:playlist:abc").as_deref(),
            Some("spotify:playlist:abc")
        );
        assert_eq!(playlist_uri("https://open.spotify.com/album/x"), None);
    }

    #[test]
    fn volume_settings() {
        let text = "output volume:31, input volume:50, alert volume:100, output muted:false\n";
        assert_eq!(parse_volume_settings(text), Some(31));
        let muted = "output volume:31, input volume:50, alert volume:100, output muted:true";
        assert_eq!(parse_volume_settings(muted), Some(0));
    }

    #[test]
    fn query_encoding() {
        assert_eq!(
            percent_encode("sezen aksu & ş"),
            "sezen%20aksu%20%26%20%C5%9F"
        );
    }
}
