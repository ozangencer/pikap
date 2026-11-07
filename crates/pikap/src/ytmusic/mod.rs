//! YouTube Music: innertube for the data, mpv for the sound, LRCLIB for synced lyrics.
//!
//! URIs follow Spotify's shape so the UI can treat both services alike:
//! `ytm:track:<videoId>`, `ytm:album:<browseId>`, `ytm:artist:<channelId>` and
//! `ytm:playlist:<playlistId>` (without the `VL` that browse ids put in front).

pub mod client;
pub mod lyrics;
pub mod mpv;
pub mod parse;
pub mod worker;

/// The liked songs: YouTube Music's automatic "Liked Music" playlist.
pub const LIKED: &str = "ytm:playlist:LM";

pub fn track_uri(video: &str) -> String {
    format!("ytm:track:{video}")
}

pub fn album_uri(browse: &str) -> String {
    format!("ytm:album:{browse}")
}

pub fn artist_uri(channel: &str) -> String {
    format!("ytm:artist:{channel}")
}

pub fn playlist_uri(id: &str) -> String {
    format!("ytm:playlist:{id}")
}

/// The id inside a `ytm:<kind>:<id>` URI.
pub fn id<'a>(uri: &'a str, kind: &str) -> Option<&'a str> {
    uri.strip_prefix("ytm:")?
        .strip_prefix(kind)?
        .strip_prefix(':')
        .filter(|id| !id.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_round_trip() {
        assert_eq!(id(&track_uri("q1b"), "track"), Some("q1b"));
        assert_eq!(id("ytm:album:MPREb_x", "album"), Some("MPREb_x"));
        assert_eq!(id("ytm:album:MPREb_x", "artist"), None);
        assert_eq!(id("spotify:track:x", "track"), None);
        assert_eq!(id("ytm:track:", "track"), None);
    }
}
