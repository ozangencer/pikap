//! Reads innertube answers into pikap's plain types.
//!
//! The answers are deep renderer trees. Two shapes carry almost everything: list rows
//! (`musicResponsiveListItemRenderer`, also the radio queue's
//! `playlistPanelVideoRenderer`) and carousel cards (`musicTwoRowItemRenderer`). Runs of
//! text link to what they name, and the link's page type says what that is (artist,
//! album, playlist), so rows are read by their links rather than by column position or
//! by headings, which change with the language.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{album_uri, artist_uri, playlist_uri, track_uri};
use crate::backend::{AlbumPage, ArtistPage, Card, TrackInfo};
use crate::data::{Entry, Kind};

/// A track as YouTube Music lists it, with what [`TrackInfo`] has no room for.
#[derive(Clone, Serialize, Deserialize)]
pub struct Song {
    pub info: TrackInfo,
    /// Largest thumbnail offered.
    pub thumb: Option<String>,
    pub year: Option<u16>,
}

/// One run of text and where it links.
struct Run<'a> {
    text: &'a str,
    browse_id: &'a str,
    page_type: &'a str,
    video_id: &'a str,
}

fn runs(text: &Value) -> Vec<Run<'_>> {
    text.get("runs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|r| {
            let nav = &r["navigationEndpoint"];
            let browse = &nav["browseEndpoint"];
            Run {
                text: r["text"].as_str().unwrap_or(""),
                browse_id: browse["browseId"].as_str().unwrap_or(""),
                page_type: page_type(browse),
                video_id: nav["watchEndpoint"]["videoId"].as_str().unwrap_or(""),
            }
        })
        .collect()
}

fn page_type(browse: &Value) -> &str {
    browse["browseEndpointContextSupportedConfigs"]["browseEndpointContextMusicConfig"]["pageType"]
        .as_str()
        .unwrap_or("")
        .trim_start_matches("MUSIC_PAGE_TYPE_")
}

fn text(v: &Value) -> String {
    runs(v).iter().map(|r| r.text).collect()
}

/// Every value under `key`, depth first, in document order.
fn find<'a>(v: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                if k == key {
                    out.push(child);
                }
                find(child, key, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|i| find(i, key, out)),
        _ => {}
    }
}

fn all<'a>(v: &'a Value, key: &str) -> Vec<&'a Value> {
    let mut out = Vec::new();
    find(v, key, &mut out);
    out
}

fn first<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    all(v, key).into_iter().next()
}

/// `3:21` or `1:02:03`.
pub fn duration(text: &str) -> Option<Duration> {
    let parts: Vec<&str> = text.trim().split(':').collect();
    if !(2..=3).contains(&parts.len()) || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let mut secs = 0u64;
    for p in parts {
        secs = secs * 60 + p.parse::<u64>().ok()?;
    }
    Some(Duration::from_secs(secs))
}

fn year(text: &str) -> Option<u16> {
    let t = text.trim();
    (t.len() == 4).then(|| t.parse().ok()).flatten()
}

/// The largest thumbnail under `v`.
fn thumbnail(v: &Value) -> Option<String> {
    first(v, "thumbnails")?
        .as_array()?
        .iter()
        .max_by_key(|t| t["width"].as_u64().unwrap_or(0))
        .and_then(|t| t["url"].as_str())
        .map(String::from)
}

/// Asks Google's image host for a square `px` wide; other hosts are left as they are.
pub fn sized(url: &str, px: u32) -> String {
    match url.rfind('=') {
        Some(i) if url.contains("googleusercontent.com") => {
            format!("{}=w{px}-h{px}-l90-rj", &url[..i])
        }
        _ => url.to_owned(),
    }
}

/// What a row's linked runs say: artists, album, duration and year.
#[derive(Default)]
struct Byline {
    artists: Vec<(String, String)>,
    album: Option<(String, String)>,
    duration: Option<Duration>,
    year: Option<u16>,
}

impl Byline {
    fn read(&mut self, runs: &[Run]) {
        for r in runs {
            match r.page_type {
                "ARTIST" | "USER_CHANNEL" if !r.browse_id.is_empty() => self
                    .artists
                    .push((artist_uri(r.browse_id), r.text.to_owned())),
                "ALBUM" => self.album = Some((album_uri(r.browse_id), r.text.to_owned())),
                _ if r.browse_id.is_empty() => {
                    if let Some(d) = duration(r.text) {
                        self.duration = Some(d);
                    } else if let Some(y) = year(r.text) {
                        self.year = Some(y);
                    }
                }
                _ => {}
            }
        }
    }
}

/// A `musicResponsiveListItemRenderer` that is a playable track.
fn list_row(row: &Value) -> Option<Song> {
    let columns: Vec<Vec<Run>> = row["flexColumns"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| runs(&c["musicResponsiveListItemFlexColumnRenderer"]["text"]))
        .chain(
            row["fixedColumns"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| runs(&c["musicResponsiveListItemFixedColumnRenderer"]["text"])),
        )
        .collect();
    let title_runs = columns.first()?;
    let video = row["playlistItemData"]["videoId"]
        .as_str()
        .filter(|v| !v.is_empty())
        .or_else(|| {
            title_runs
                .iter()
                .map(|r| r.video_id)
                .find(|v| !v.is_empty())
        })?;
    let mut byline = Byline::default();
    for column in &columns[1..] {
        byline.read(column);
    }
    Some(Song {
        info: TrackInfo {
            uri: track_uri(video),
            name: title_runs.iter().map(|r| r.text).collect(),
            artists: byline.artists,
            duration: byline.duration.unwrap_or_default(),
            album: byline.album,
        },
        thumb: thumbnail(&row["thumbnail"]),
        year: byline.year,
    })
}

/// A `playlistPanelVideoRenderer`: a radio or "up next" entry.
fn panel_row(row: &Value) -> Option<Song> {
    let video = row["videoId"].as_str().filter(|v| !v.is_empty())?;
    let mut byline = Byline::default();
    byline.read(&runs(&row["longBylineText"]));
    Some(Song {
        info: TrackInfo {
            uri: track_uri(video),
            name: text(&row["title"]),
            artists: byline.artists,
            duration: duration(&text(&row["lengthText"])).unwrap_or_default(),
            album: byline.album,
        },
        thumb: thumbnail(&row["thumbnail"]),
        year: byline.year,
    })
}

/// Every playable track in an answer, in order: playlist and album rows, search
/// results, radio entries.
pub fn songs(v: &Value) -> Vec<Song> {
    let mut out = Vec::new();
    collect_songs(v, &mut out);
    out
}

fn collect_songs(v: &Value, out: &mut Vec<Song>) {
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                let song = match k.as_str() {
                    "musicResponsiveListItemRenderer" => list_row(child),
                    "playlistPanelVideoRenderer" => panel_row(child),
                    _ => {
                        collect_songs(child, out);
                        continue;
                    }
                };
                out.extend(song);
            }
        }
        Value::Array(items) => items.iter().for_each(|i| collect_songs(i, out)),
        _ => {}
    }
}

/// The token for the next page of a long list, if there is one.
pub fn continuation(v: &Value) -> Option<String> {
    first(v, "continuationCommand")?["token"]
        .as_str()
        .or_else(|| first(v, "nextContinuationData")?["continuation"].as_str())
        .map(String::from)
}

/// A playlist page: its name and first tracks.
pub fn playlist(v: &Value) -> (String, Vec<Song>) {
    let name = first(v, "musicResponsiveHeaderRenderer")
        .or_else(|| first(v, "musicEditablePlaylistDetailHeaderRenderer"))
        .map(|h| text(&h["title"]))
        .filter(|t| !t.is_empty())
        .or_else(|| first(v, "musicDetailHeaderRenderer").map(|h| text(&h["title"])))
        .unwrap_or_default();
    (name, songs(v))
}

/// An album page. Its rows name neither the album nor (mostly) the artists, so both
/// come from the header.
pub fn album(v: &Value, uri: &str) -> (AlbumPage, Option<String>) {
    let header = first(v, "musicResponsiveHeaderRenderer").unwrap_or(&Value::Null);
    let name = text(&header["title"]);
    let mut by = Byline::default();
    by.read(&runs(&header["straplineTextOne"]));
    by.read(&runs(&header["subtitle"]));
    let artist = by
        .artists
        .iter()
        .map(|(_, n)| n.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let tracks = songs(v)
        .into_iter()
        .map(|s| {
            let mut t = s.info;
            if t.artists.is_empty() {
                t.artists = by.artists.clone();
            }
            t.album = Some((uri.to_owned(), name.clone()));
            t
        })
        .collect();
    let page = AlbumPage {
        uri: uri.to_owned(),
        name,
        artist,
        year: by.year,
        tracks,
    };
    (page, thumbnail(&header["thumbnail"]))
}

/// An artist page: top songs, releases (albums, then singles and EPs), related
/// artists, and the artist's radio playlist; the top songs also come back whole.
pub fn artist(v: &Value, uri: &str) -> (ArtistPage, Option<String>, Vec<Song>) {
    let header = v["header"]
        .as_object()
        .and_then(|h| h.values().next())
        .unwrap_or(&Value::Null);
    let name = text(&header["title"]);
    let radio = first(&header["startRadioButton"], "playlistId")
        .and_then(Value::as_str)
        .map(String::from);

    let mut top = Vec::new();
    let mut discography = Vec::new();
    let mut related = Vec::new();
    for section in all(v, "sectionListRenderer")
        .first()
        .and_then(|s| s["contents"].as_array())
        .into_iter()
        .flatten()
    {
        if let Some(shelf) = section.get("musicShelfRenderer") {
            if top.is_empty() {
                top = songs(shelf);
            }
            continue;
        }
        let Some(carousel) = section.get("musicCarouselShelfRenderer") else {
            continue;
        };
        for item in carousel["contents"].as_array().into_iter().flatten() {
            let Some(card) = item.get("musicTwoRowItemRenderer") else {
                continue;
            };
            let browse = &card["navigationEndpoint"]["browseEndpoint"];
            let id = browse["browseId"].as_str().unwrap_or("");
            match page_type(browse) {
                "ALBUM" => {
                    let sub = runs(&card["subtitle"]);
                    let single = sub.iter().any(|r| matches!(r.text.trim(), "Single" | "EP"));
                    discography.push(Card {
                        uri: album_uri(id),
                        name: text(&card["title"]),
                        year: sub.iter().find_map(|r| year(r.text)),
                        single,
                        cover: thumbnail(&card["thumbnailRenderer"]).map(|u| sized(&u, 300)),
                    });
                }
                "ARTIST" if id != uri.rsplit(':').next().unwrap_or("") => {
                    related.push((artist_uri(id), text(&card["title"])));
                }
                _ => {}
            }
        }
    }
    // Albums first, as the page orders its shelves.
    discography.sort_by_key(|c| c.single);
    let page = ArtistPage {
        uri: uri.to_owned(),
        name,
        top: top.iter().map(|s| s.info.clone()).collect(),
        related,
        popularity: 0,
        releases: discography
            .iter()
            .map(|c| (c.uri.clone(), c.single))
            .collect(),
        discography,
    };
    (page, radio, top)
}

/// Rows of a filtered search that open a page: albums, artists or playlists.
pub fn search_entries(v: &Value, kind: Kind) -> Vec<Entry> {
    all(v, "musicResponsiveListItemRenderer")
        .into_iter()
        .filter_map(|row| {
            let browse = &row["navigationEndpoint"]["browseEndpoint"];
            let id = browse["browseId"].as_str().filter(|i| !i.is_empty())?;
            let columns: Vec<Vec<Run>> = row["flexColumns"]
                .as_array()?
                .iter()
                .map(|c| runs(&c["musicResponsiveListItemFlexColumnRenderer"]["text"]))
                .collect();
            let title: String = columns.first()?.iter().map(|r| r.text).collect();
            let rest = columns.get(1).map(Vec::as_slice).unwrap_or_default();
            entry(kind, page_type(browse), id, title, rest)
        })
        .collect()
}

/// Cards that open a playlist or album: library grids and Home shelves.
pub fn card_entries(v: &Value) -> Vec<Entry> {
    all(v, "musicTwoRowItemRenderer")
        .into_iter()
        .filter_map(|card| {
            let browse = &card["navigationEndpoint"]["browseEndpoint"];
            let id = browse["browseId"].as_str().filter(|i| !i.is_empty())?;
            let kind = match page_type(browse) {
                "PLAYLIST" => Kind::Playlist,
                "ALBUM" => Kind::Album,
                _ => return None,
            };
            let sub = runs(&card["subtitle"]);
            entry(kind, page_type(browse), id, text(&card["title"]), &sub)
        })
        .collect()
}

fn entry(kind: Kind, page: &str, id: &str, title: String, rest: &[Run]) -> Option<Entry> {
    let uri = match (kind, page) {
        (Kind::Album, "ALBUM") => album_uri(id),
        (Kind::Artist, "ARTIST") => artist_uri(id),
        (Kind::Playlist, "PLAYLIST") => playlist_uri(id.trim_start_matches("VL")),
        _ => return None,
    };
    let linked: Vec<(String, String)> = rest
        .iter()
        .filter(|r| !r.browse_id.is_empty())
        .map(|r| (artist_uri(r.browse_id), r.text.to_owned()))
        .collect();
    let sub = match kind {
        // "Album • Gorillaz • 2001": the artist and year say more than the type.
        Kind::Album => rest
            .iter()
            .skip_while(|r| r.browse_id.is_empty())
            .map(|r| r.text)
            .collect::<String>(),
        _ => rest
            .iter()
            .map(|r| r.text)
            .collect::<String>()
            .trim_start_matches("Artist • ")
            .trim_start_matches("Playlist • ")
            .to_owned(),
    };
    Some(Entry {
        sub: sub.trim().to_owned(),
        artist: (kind == Kind::Album)
            .then(|| linked.first().cloned())
            .flatten(),
        ..Entry::new(kind, uri, title)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(text: &str) -> Value {
        json!({ "text": text })
    }

    fn link(text: &str, id: &str, page: &str) -> Value {
        json!({ "text": text, "navigationEndpoint": { "browseEndpoint": {
            "browseId": id,
            "browseEndpointContextSupportedConfigs": { "browseEndpointContextMusicConfig": {
                "pageType": format!("MUSIC_PAGE_TYPE_{page}") } } } } })
    }

    fn column(runs: Vec<Value>) -> Value {
        json!({ "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": runs } } })
    }

    #[test]
    fn durations() {
        assert_eq!(duration("3:21"), Some(Duration::from_secs(201)));
        assert_eq!(duration("1:02:03"), Some(Duration::from_secs(3723)));
        assert_eq!(duration("2005"), None);
        assert_eq!(duration("341M plays"), None);
    }

    #[test]
    fn search_row_reads_its_links() {
        let row = json!({ "musicResponsiveListItemRenderer": {
            "playlistItemData": { "videoId": "q1b" },
            "flexColumns": [
                column(vec![run("Rhinestone Eyes")]),
                column(vec![link("Gorillaz", "UCg", "ARTIST"), run(" • "),
                            link("Plastic Beach", "MPREb_pb", "ALBUM"), run(" • "), run("3:21")]),
                column(vec![run("341M plays")]),
            ],
            "thumbnail": { "musicThumbnailRenderer": { "thumbnail": { "thumbnails": [
                { "url": "small", "width": 60 }, { "url": "big", "width": 120 }] } } }
        }});
        let found = songs(&json!({ "contents": [row] }));
        assert_eq!(found.len(), 1);
        let s = &found[0];
        assert_eq!(s.info.uri, "ytm:track:q1b");
        assert_eq!(s.info.name, "Rhinestone Eyes");
        assert_eq!(
            s.info.artists,
            [("ytm:artist:UCg".into(), "Gorillaz".into())]
        );
        assert_eq!(
            s.info.album,
            Some(("ytm:album:MPREb_pb".into(), "Plastic Beach".into()))
        );
        assert_eq!(s.info.duration, Duration::from_secs(201));
        assert_eq!(s.thumb.as_deref(), Some("big"));
    }

    #[test]
    fn radio_entry_with_two_artists_and_a_year() {
        let panel = json!({ "playlistPanelVideoRenderer": {
            "videoId": "fgi",
            "title": { "runs": [run("Feel Good Inc.")] },
            "longBylineText": { "runs": [
                link("Gorillaz", "UCg", "ARTIST"), run(" & "), link("De La Soul", "UCd", "ARTIST"),
                run(" • "), link("Demon Days", "MPREb_dd", "ALBUM"), run(" • "), run("2005")] },
            "lengthText": { "runs": [run("3:43")] }
        }});
        let s = &songs(&panel)[0];
        assert_eq!(s.info.artist_line(), "Gorillaz, De La Soul");
        assert_eq!(s.year, Some(2005));
        assert_eq!(s.info.duration, Duration::from_secs(223));
    }

    #[test]
    fn artist_page_sorts_cards_by_what_they_open() {
        let card = |title: &str, id: &str, page: &str, sub: Vec<Value>| {
            json!({ "musicTwoRowItemRenderer": {
                "title": { "runs": [run(title)] },
                "subtitle": { "runs": sub },
                "navigationEndpoint": link("", id, page)["navigationEndpoint"].clone(),
            }})
        };
        let page = json!({
            "header": { "musicImmersiveHeaderRenderer": {
                "title": { "runs": [run("Gorillaz")] },
                "startRadioButton": { "buttonRenderer": { "navigationEndpoint": {
                    "watchPlaylistEndpoint": { "playlistId": "RDEMg" } } } } } },
            "contents": { "singleColumnBrowseResultsRenderer": { "tabs": [{ "tabRenderer": {
                "content": { "sectionListRenderer": { "contents": [
                    { "musicShelfRenderer": { "contents": [{ "musicResponsiveListItemRenderer": {
                        "playlistItemData": { "videoId": "fgi" },
                        "flexColumns": [column(vec![run("Feel Good Inc.")])] } }] } },
                    { "musicCarouselShelfRenderer": { "contents": [
                        card("Single One", "MPREb_s", "ALBUM", vec![run("Single"), run(" • "), run("2026")]) ] } },
                    { "musicCarouselShelfRenderer": { "contents": [
                        card("Demon Days", "MPREb_dd", "ALBUM", vec![run("2005")]) ] } },
                    { "musicCarouselShelfRenderer": { "contents": [
                        card("Tame Impala", "UCt", "ARTIST", vec![run("236M monthly audience")]) ] } },
                ] } } } }] } }
        });
        let (artist, radio, top) = artist(&page, "ytm:artist:UCg");
        assert_eq!(top[0].info.name, "Feel Good Inc.");
        assert_eq!(artist.name, "Gorillaz");
        assert_eq!(radio.as_deref(), Some("RDEMg"));
        assert_eq!(artist.top.len(), 1);
        assert_eq!(
            artist.related,
            [("ytm:artist:UCt".into(), "Tame Impala".into())]
        );
        let names: Vec<_> = artist
            .discography
            .iter()
            .map(|c| (c.name.as_str(), c.single, c.year))
            .collect();
        assert_eq!(
            names,
            [
                ("Demon Days", false, Some(2005)),
                ("Single One", true, Some(2026))
            ]
        );
    }

    #[test]
    fn search_entries_by_kind() {
        let row = |id: &str, page: &str, title: &str, rest: Vec<Value>| {
            json!({ "musicResponsiveListItemRenderer": {
            "navigationEndpoint": link("", id, page)["navigationEndpoint"].clone(),
            "flexColumns": [column(vec![run(title)]), column(rest)] } })
        };
        let v = json!([
            row(
                "MPREb_g",
                "ALBUM",
                "Gorillaz",
                vec![
                    run("Album"),
                    run(" • "),
                    link("Gorillaz", "UCg", "ARTIST"),
                    run(" • "),
                    run("2001")
                ]
            ),
            row(
                "VLPLx",
                "PLAYLIST",
                "Mix",
                vec![
                    link("Someone", "UCs", "USER_CHANNEL"),
                    run(" • "),
                    run("23K views")
                ]
            ),
        ]);
        let albums = search_entries(&v, Kind::Album);
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].uri, "ytm:album:MPREb_g");
        assert_eq!(albums[0].sub, "Gorillaz • 2001");
        assert_eq!(
            albums[0].artist,
            Some(("ytm:artist:UCg".into(), "Gorillaz".into()))
        );
        let lists = search_entries(&v, Kind::Playlist);
        assert_eq!(lists[0].uri, "ytm:playlist:PLx");
        assert_eq!(lists[0].sub, "Someone • 23K views");
    }

    #[test]
    fn google_images_ask_for_a_size() {
        assert_eq!(
            sized("https://lh3.googleusercontent.com/abc=w60-h60-l90-rj", 640),
            "https://lh3.googleusercontent.com/abc=w640-h640-l90-rj"
        );
        assert_eq!(
            sized("https://i.ytimg.com/vi/x/hq.jpg", 640),
            "https://i.ytimg.com/vi/x/hq.jpg"
        );
    }
}
