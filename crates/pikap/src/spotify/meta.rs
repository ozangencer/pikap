//! Spotify's internal metadata through librespot: artists, albums, radio, related
//! artists and synced lyrics. Development Mode removed most of these from the Web API,
//! and nothing here counts against its quota.
//!
//! librespot signs in as Spotify's desktop client with its own credentials, kept in
//! `~/.cache/pikap/librespot/`; `pikap auth` creates them.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use librespot_core::authentication::Credentials;
use librespot_core::cache::Cache;
use librespot_core::{Session, SessionConfig, SpotifyUri};
use librespot_metadata::lyrics::SyncType;
use librespot_metadata::{Album, Artist, Lyrics, Metadata, Playlist, Track};
use serde::{Deserialize, Serialize};

use super::config;

/// Spotify's own desktop client, which librespot authenticates as.
pub const DESKTOP_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
pub const SCOPES: &[&str] = &[
    "streaming",
    "user-read-private",
    "user-read-playback-state",
    "playlist-read-private",
    "user-library-read",
];

/// spclient answers bursts with "resource exhausted", and that limit seems to apply
/// to the whole account: Spotify.app's own player went grey while pikap loaded a
/// 728-track playlist track by track. Lookups stay few and spaced out.
const CONCURRENCY: usize = 4;
const PACE: Duration = Duration::from_millis(50);
/// Radio tracks read to name related artists; a station holds about fifty.
const RELATED_FROM_RADIO: usize = 20;
const RETRIES: u32 = 4;
const TOP_TRACKS: usize = 20;
const RELATED_ARTISTS: usize = 12;
/// Releases shown on an artist's page; each is one lookup.
const ALBUMS_SHOWN: usize = 24;
const SINGLES_SHOWN: usize = 12;

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
    /// Spotify's 0-100 popularity.
    pub popularity: u8,
    /// Album and single URIs (`true` for singles), albums first, as Spotify orders them.
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
    /// Image CDN address of a 300 px cover.
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

/// What a playlist is, without its tracks.
pub struct PlaylistSummary {
    pub name: String,
    pub count: usize,
    /// Spotify's kind for generated lists: `discover-weekly`, `daily-mix`,
    /// `release-radar`, `inspiredby-mix` (radio), `liked-songs`; empty for others.
    pub format: String,
    /// Made For You: personal lists Spotify generates for the user (not radios, not
    /// the liked songs mirror).
    pub made_for_you: bool,
}

pub struct LyricLine {
    pub start: Duration,
    pub words: String,
}

pub struct LyricsPage {
    pub synced: bool,
    pub lines: Vec<LyricLine>,
}

fn cache() -> Result<Cache> {
    let dir = config::cache_dir().join("librespot");
    private_dir(&dir)?;
    Cache::new(Some(dir), None, None, None).map_err(|e| anyhow!("librespot cache: {e}"))
}

fn session_config() -> SessionConfig {
    SessionConfig {
        client_id: DESKTOP_CLIENT_ID.to_owned(),
        ..SessionConfig::default()
    }
}

pub fn has_credentials() -> bool {
    cache().ok().and_then(|c| c.credentials()).is_some()
}

/// Trades an OAuth access token for reusable credentials, which the cache keeps.
pub async fn store_credentials(access_token: String) -> Result<()> {
    let session = Session::new(session_config(), Some(cache()?));
    session
        .connect(Credentials::with_access_token(access_token), true)
        .await
        .context("librespot sign-in")?;
    session.shutdown();
    restrict_credentials();
    Ok(())
}

/// librespot creates its directory and credentials with the default umask, so the
/// directory is made private before librespot writes anything into it.
fn private_dir(dir: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// librespot writes its credentials world-readable; they are as sensitive as a password.
fn restrict_credentials() {
    use std::os::unix::fs::PermissionsExt;
    let dir = config::cache_dir().join("librespot");
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    let _ = std::fs::set_permissions(
        dir.join("credentials.json"),
        std::fs::Permissions::from_mode(0o600),
    );
}

pub struct Meta {
    session: Session,
    /// Track details by URI; playlists, radios and artist pages share many tracks.
    /// Kept on disk (`tracks.json`), so a track is looked up once, not once per run.
    known: std::sync::Mutex<std::collections::HashMap<String, TrackInfo>>,
    /// Set when `known` gained tracks the disk copy lacks.
    dirty: std::sync::atomic::AtomicBool,
    /// When the last spclient lookup started.
    last_lookup: tokio::sync::Mutex<tokio::time::Instant>,
}

fn known_path() -> std::path::PathBuf {
    config::cache_dir().join("tracks.json")
}

fn read_known() -> std::collections::HashMap<String, TrackInfo> {
    std::fs::read(known_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<TrackInfo>>(&bytes).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|t| (t.uri.clone(), t))
        .collect()
}

impl Meta {
    pub async fn connect() -> Result<Self> {
        let cache = cache()?;
        let credentials = cache
            .credentials()
            .context("no librespot credentials; run `pikap auth`")?;
        let session = Session::new(session_config(), Some(cache));
        session
            .connect(credentials, true)
            .await
            .context("librespot sign-in")?;
        restrict_credentials();
        Ok(Self {
            session,
            known: std::sync::Mutex::new(read_known()),
            dirty: std::sync::atomic::AtomicBool::new(false),
            last_lookup: tokio::sync::Mutex::new(tokio::time::Instant::now()),
        })
    }

    /// Tracks already known from elsewhere (liked songs from the Web API), so
    /// librespot never has to look them up.
    pub fn remember(&self, tracks: &[TrackInfo]) {
        if let Ok(mut known) = self.known.lock() {
            for track in tracks.iter().filter(|t| t.album.is_some()) {
                known.insert(track.uri.clone(), track.clone());
            }
        }
        self.dirty.store(true, std::sync::atomic::Ordering::Relaxed);
        self.save_known();
    }

    /// The album a known track belongs to.
    pub fn album_of(&self, track_uri: &str) -> Option<String> {
        let known = self.known.lock().ok()?;
        known
            .get(track_uri)?
            .album
            .as_ref()
            .map(|(uri, _)| uri.clone())
    }

    fn save_known(&self) {
        if !self.dirty.swap(false, std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let Ok(tracks) = self
            .known
            .lock()
            .map(|k| k.values().cloned().collect::<Vec<_>>())
        else {
            return;
        };
        let path = known_path();
        let tmp = path.with_extension("json.tmp");
        if let Ok(bytes) = serde_json::to_vec(&tracks)
            && std::fs::write(&tmp, bytes).is_ok()
        {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// Waits until at least `PACE` passed since the previous lookup started.
    async fn pace(&self) {
        let mut last = self.last_lookup.lock().await;
        let next = *last + PACE;
        tokio::time::sleep_until(next).await;
        *last = tokio::time::Instant::now();
    }

    /// The signed-in Spotify user id, without asking the Web API.
    pub fn user(&self) -> String {
        self.session.username()
    }

    pub fn is_alive(&self) -> bool {
        !self.session.is_invalid()
    }

    /// Track details in the given order; tracks that fail to load are skipped.
    pub async fn tracks(&self, uris: Vec<SpotifyUri>) -> Vec<TrackInfo> {
        let tracks = futures_util::stream::iter(uris)
            .map(|uri| self.track(uri))
            .buffered(CONCURRENCY)
            .filter_map(|t| async move { t })
            .collect()
            .await;
        self.save_known();
        tracks
    }

    /// One track, from memory when seen before. Spotify answers bursts with "resource
    /// exhausted"; those are retried with growing pauses instead of dropped.
    async fn track(&self, uri: SpotifyUri) -> Option<TrackInfo> {
        let key = uri.to_uri().ok()?;
        if let Some(known) = self.known.lock().ok()?.get(&key) {
            return Some(known.clone());
        }
        let lookup = || async {
            self.pace().await;
            Track::get(&self.session, &uri).await
        };
        match patiently(lookup).await {
            Ok(track) => {
                let info = track_info(&track)?;
                if let Ok(mut known) = self.known.lock() {
                    known.insert(key, info.clone());
                }
                self.dirty.store(true, std::sync::atomic::Ordering::Relaxed);
                Some(info)
            }
            Err(err) => {
                crate::log::plog!("track {key}: {err}");
                None
            }
        }
    }

    pub async fn album(&self, uri: &str) -> Result<AlbumPage> {
        let id = SpotifyUri::from_uri(uri)?;
        let album = Album::get(&self.session, &id).await?;
        let tracks = self.tracks(album.tracks().cloned().collect()).await;
        Ok(AlbumPage {
            uri: uri.to_owned(),
            name: album.name.clone(),
            artist: album
                .artists
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            year: u16::try_from(album.date.as_utc().year()).ok(),
            tracks,
        })
    }

    pub async fn artist(&self, uri: &str) -> Result<ArtistPage> {
        let id = SpotifyUri::from_uri(uri)?;
        let artist = Artist::get(&self.session, &id).await?;
        let country = self.session.country();
        let top_uris: Vec<SpotifyUri> = artist
            .top_tracks
            .iter()
            .find(|t| t.country == country)
            .or_else(|| artist.top_tracks.first())
            .map(|t| t.tracks.iter().take(TOP_TRACKS).cloned().collect())
            .unwrap_or_default();

        let listed: Vec<(String, String)> = artist
            .related
            .iter()
            .filter_map(|a| Some((a.id.to_uri().ok()?, a.name.clone())))
            .take(RELATED_ARTISTS)
            .collect();
        let wanted = top_uris.len();
        let countries: Vec<(&str, usize)> = artist
            .top_tracks
            .iter()
            .map(|t| (t.country.as_str(), t.tracks.len()))
            .collect();
        let (top, related) = tokio::join!(self.tracks(top_uris), async {
            if listed.is_empty() {
                self.related_from_radio(uri).await
            } else {
                listed
            }
        });
        crate::log::plog!(
            "artist {}: country {country}, top lists {countries:?}, wanted {wanted}, loaded {}",
            artist.name,
            top.len()
        );
        let group = |groups: &librespot_metadata::artist::AlbumGroups, single: bool, n: usize| {
            groups
                .iter()
                .filter_map(|g| g.0.first()?.to_uri().ok())
                .take(n)
                .map(move |uri| (uri, single))
                .collect::<Vec<_>>()
        };
        let mut releases = group(&artist.albums, false, ALBUMS_SHOWN);
        releases.extend(group(&artist.singles, true, SINGLES_SHOWN));
        Ok(ArtistPage {
            uri: uri.to_owned(),
            name: artist.name.clone(),
            top,
            related,
            popularity: u8::try_from(artist.popularity.clamp(0, 100)).unwrap_or(0),
            releases,
            discography: Vec::new(),
        })
    }

    /// Names, years and covers of an artist's releases, one paced lookup each.
    pub async fn cards(&self, releases: &[(String, bool)]) -> Vec<Card> {
        let mut cards = Vec::with_capacity(releases.len());
        for (uri, single) in releases {
            let Ok(id) = SpotifyUri::from_uri(uri) else {
                continue;
            };
            let album = patiently(|| async {
                self.pace().await;
                Album::get(&self.session, &id).await
            })
            .await;
            match album {
                Ok(album) => cards.push(Card {
                    uri: uri.clone(),
                    name: album.name.clone(),
                    year: u16::try_from(album.date.as_utc().year()).ok(),
                    single: *single,
                    cover: cover_url(&album.covers),
                }),
                Err(err) => crate::log::plog!("release {uri}: {err}"),
            }
        }
        cards
    }

    /// Any playlist, the user's or not; the Web API refuses others' under Development Mode.
    pub async fn playlist(&self, uri: &str) -> Result<ListPage> {
        let id = SpotifyUri::from_uri(uri)?;
        let playlist = patiently(|| async {
            self.pace().await;
            Playlist::get(&self.session, &id).await
        })
        .await?;
        let uris = playlist
            .tracks()
            .filter(|u| matches!(u, SpotifyUri::Track { .. }))
            .cloned()
            .collect();
        Ok(ListPage {
            uri: uri.to_owned(),
            name: playlist.name().to_owned(),
            tracks: self.tracks(uris).await,
        })
    }

    /// A playlist's name, track count and kind, without loading its tracks.
    pub async fn playlist_summary(&self, uri: &str) -> Result<PlaylistSummary> {
        let id = SpotifyUri::from_uri(uri)?;
        let playlist = patiently(|| async {
            self.pace().await;
            Playlist::get(&self.session, &id).await
        })
        .await?;
        let attributes = &playlist.attributes;
        let marks = &attributes.format_attributes.0;
        let made_for = marks.contains_key("madeFor.username")
            || marks.get("madeFor.displayed").is_some_and(|v| v == "1");
        let format = attributes.format.clone();
        Ok(PlaylistSummary {
            name: playlist.name().to_owned(),
            count: playlist.tracks().count(),
            made_for_you: made_for
                && !matches!(format.as_str(), "" | "inspiredby-mix" | "liked-songs"),
            format,
        })
    }

    /// Spotify no longer fills `related` in artist metadata; the other artists in the
    /// artist's radio make a good "fans also like" list.
    async fn related_from_radio(&self, artist_uri: &str) -> Vec<(String, String)> {
        let Ok(station) = self.station(artist_uri).await else {
            return Vec::new();
        };
        let tracks = self
            .tracks(station.into_iter().take(RELATED_FROM_RADIO).collect())
            .await;
        let mut seen = std::collections::HashSet::from([artist_uri.to_owned()]);
        tracks
            .into_iter()
            .flat_map(|t| t.artists)
            .filter(|(uri, _)| seen.insert(uri.clone()))
            .take(RELATED_ARTISTS)
            .collect()
    }

    /// Spotify's own radio for a seed, as a real playlist (what "Go to song radio"
    /// opens in Spotify.app), so it can be played as a context.
    pub async fn radio_playlist(&self, seed: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct Mix {
            #[serde(rename = "mediaItems")]
            media_items: Vec<MediaItem>,
        }
        #[derive(Deserialize)]
        struct MediaItem {
            uri: String,
        }
        let uri = SpotifyUri::from_uri(seed)?;
        let body = patiently(|| async {
            self.pace().await;
            self.session.spclient().get_radio_for_track(&uri).await
        })
        .await?;
        serde_json::from_slice::<Mix>(&body)?
            .media_items
            .into_iter()
            .map(|m| m.uri)
            .find(|u| u.starts_with("spotify:playlist:"))
            .context("no radio playlist")
    }

    /// The track URIs of a seed's radio station.
    async fn station(&self, seed: &str) -> Result<Vec<SpotifyUri>> {
        #[derive(Deserialize)]
        struct Station {
            tracks: Vec<StationTrack>,
        }
        #[derive(Deserialize)]
        struct StationTrack {
            original_gid: String,
        }

        let autoplay = self
            .mercury(&format!("hm://autoplay-enabled/query?uri={seed}"))
            .await?;
        let station_uri = String::from_utf8(autoplay)?;
        let station = self
            .mercury(&format!("hm://radio-apollo/v3/stations/{station_uri}"))
            .await?;
        Ok(serde_json::from_slice::<Station>(&station)?
            .tracks
            .into_iter()
            .filter_map(|t| SpotifyUri::from_uri(&format!("spotify:track:{}", t.original_gid)).ok())
            .collect())
    }

    /// Synced lyrics when Spotify has them; `None` for tracks without lyrics.
    pub async fn lyrics(&self, track_uri: &str) -> Result<Option<LyricsPage>> {
        let SpotifyUri::Track { id } = SpotifyUri::from_uri(track_uri)? else {
            return Ok(None);
        };
        let Ok(lyrics) = Lyrics::get(&self.session, &id).await else {
            return Ok(None);
        };
        let timed = lyrics
            .lyrics
            .lines
            .iter()
            .any(|l| l.start_time_ms.parse::<u64>().is_ok_and(|ms| ms > 0));
        let synced = lyrics.lyrics.sync_type == SyncType::LineSynced || timed;
        crate::log::plog!(
            "lyrics {track_uri}: {:?}, {} lines, timed {timed}",
            lyrics.lyrics.sync_type,
            lyrics.lyrics.lines.len()
        );
        let lines = lyrics
            .lyrics
            .lines
            .into_iter()
            .map(|l| LyricLine {
                start: Duration::from_millis(l.start_time_ms.parse().unwrap_or(0)),
                words: l.words,
            })
            .collect();
        Ok(Some(LyricsPage { synced, lines }))
    }

    async fn mercury(&self, uri: &str) -> Result<Vec<u8>> {
        let response = self
            .session
            .mercury()
            .get(uri)
            .map_err(|e| anyhow!("{uri}: {e}"))?
            .await
            .map_err(|e| anyhow!("{uri}: {e}"))?;
        if response.status_code != 200 {
            bail!("{uri}: status {}", response.status_code);
        }
        response
            .payload
            .into_iter()
            .next()
            .context("empty mercury response")
    }
}

/// Spotify answers bursts with "resource exhausted"; those calls are retried with
/// growing pauses (0.5, 1, 2, 4 s) instead of failing.
async fn patiently<T, E: std::fmt::Display, F: Future<Output = Result<T, E>>>(
    mut call: impl FnMut() -> F,
) -> Result<T, E> {
    let mut pause = Duration::from_millis(500);
    let mut attempt = 0;
    loop {
        match call().await {
            Err(err) if attempt < RETRIES && format!("{err}").contains("exhausted") => {
                attempt += 1;
                tokio::time::sleep(pause).await;
                pause *= 2;
            }
            result => return result,
        }
    }
}

/// The 300 px image (Spotify's default size) on the image CDN.
fn cover_url(images: &librespot_metadata::image::Images) -> Option<String> {
    use librespot_metadata::image::ImageSize;
    let image = images
        .iter()
        .find(|i| i.size == ImageSize::DEFAULT)
        .or_else(|| images.first())?;
    Some(format!(
        "https://i.scdn.co/image/{}",
        image.id.to_base16().ok()?
    ))
}

fn track_info(track: &Track) -> Option<TrackInfo> {
    Some(TrackInfo {
        uri: track.id.to_uri().ok()?,
        name: track.name.clone(),
        artists: track
            .artists
            .iter()
            .filter_map(|a| Some((a.id.to_uri().ok()?, a.name.clone())))
            .collect(),
        duration: Duration::from_millis(u64::try_from(track.duration).unwrap_or(0)),
        album: track
            .album
            .id
            .to_uri()
            .ok()
            .map(|uri| (uri, track.album.name.clone())),
    })
}

#[cfg(test)]
mod live {
    /// `cargo test --release -- --ignored --nocapture radio_playlists` (needs `pikap auth`).
    #[test]
    #[ignore = "talks to Spotify"]
    fn radio_playlists() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let meta = super::Meta::connect().await.unwrap();
            for seed in [
                "spotify:track:3GfOAdcoc3X5GPiiXmpBjK",
                "spotify:artist:7MhMgCo0Bl0Kukl93PZbYS",
                "spotify:album:1bgkxe4t0HNeLn9rhrx79x",
            ] {
                println!(
                    "{seed} -> {:?}",
                    meta.radio_playlist(seed).await.map_err(|e| e.to_string())
                );
            }
        });
    }
}

#[cfg(test)]
mod live_made_for_you {
    /// `cargo test --release -- --ignored --nocapture made_for_you_rule` (needs `pikap auth`).
    #[test]
    #[ignore = "talks to Spotify"]
    fn made_for_you_rule() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let meta = super::Meta::connect().await.unwrap();
            for (id, expected) in [
                ("37i9dQZEVXcUMx7ZNCmeo4", true),  // Discover Weekly
                ("37i9dQZF1E36c3f87B1Xo0", true),  // Daily Mix 1
                ("37i9dQZF1E8Qck6xJNqYNR", false), // a song radio
                ("37i9dQZF1F5p3rmiWPIYgZ", false), // the liked songs mirror
                ("4YHyPgWnKhhfJ9SqX6s4LU", false), // an ordinary playlist
            ] {
                let summary = meta
                    .playlist_summary(&format!("spotify:playlist:{id}"))
                    .await
                    .unwrap();
                println!(
                    "{} ({}): {}",
                    summary.name, summary.format, summary.made_for_you
                );
                assert_eq!(summary.made_for_you, expected, "{}", summary.name);
            }
        });
    }
}
