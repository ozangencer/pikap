//! Lyrics from LRCLIB (lrclib.net), a free, open database of synced lyrics. YouTube
//! Music's own lyrics are plain text, so LRCLIB comes first.

use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;

use crate::backend::{LyricLine, LyricsPage};

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Answer {
    synced_lyrics: Option<String>,
    plain_lyrics: Option<String>,
    instrumental: bool,
}

/// Looks the track up by its names and length; `None` when LRCLIB does not know it.
pub async fn lrclib(
    http: &reqwest::Client,
    artist: &str,
    title: &str,
    album: &str,
    duration: Duration,
) -> Result<Option<LyricsPage>> {
    let secs = duration.as_secs().to_string();
    let mut query = vec![("artist_name", artist), ("track_name", title)];
    if !album.is_empty() {
        query.push(("album_name", album));
    }
    if duration > Duration::ZERO {
        query.push(("duration", secs.as_str()));
    }
    let res = http
        .get("https://lrclib.net/api/get")
        .query(&query)
        .send()
        .await?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let answer: Answer = res.error_for_status()?.json().await?;
    if answer.instrumental {
        return Ok(None);
    }
    if let Some(synced) = answer.synced_lyrics.filter(|s| !s.trim().is_empty()) {
        return Ok(Some(LyricsPage {
            synced: true,
            lines: parse_lrc(&synced),
        }));
    }
    Ok(answer
        .plain_lyrics
        .filter(|p| !p.trim().is_empty())
        .map(|plain| LyricsPage {
            synced: false,
            lines: plain
                .lines()
                .map(|l| LyricLine {
                    start: Duration::ZERO,
                    words: l.trim().to_owned(),
                })
                .collect(),
        }))
}

/// `[01:17.83] words` lines; a line may carry several time tags.
pub fn parse_lrc(text: &str) -> Vec<LyricLine> {
    let mut lines = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut starts = Vec::new();
        while let Some(tag) = rest.strip_prefix('[') {
            let Some(end) = tag.find(']') else { break };
            if let Some(start) = timestamp(&tag[..end]) {
                starts.push(start);
            }
            rest = &tag[end + 1..];
        }
        for start in starts {
            lines.push(LyricLine {
                start,
                words: rest.trim().to_owned(),
            });
        }
    }
    lines.sort_by_key(|l| l.start);
    lines
}

fn timestamp(tag: &str) -> Option<Duration> {
    let (min, sec) = tag.split_once(':')?;
    let min: u64 = min.parse().ok()?;
    let sec: f64 = sec.parse().ok()?;
    Some(Duration::from_millis(
        min * 60_000 + (sec * 1000.0).round() as u64,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lrc_lines_in_time_order() {
        let lines = parse_lrc("[ar: Gorillaz]\n[00:12.50]first\n[01:17.83][00:30.00]again\n\n");
        let got: Vec<(u128, &str)> = lines
            .iter()
            .map(|l| (l.start.as_millis(), l.words.as_str()))
            .collect();
        assert_eq!(
            got,
            [(12_500, "first"), (30_000, "again"), (77_830, "again")]
        );
    }
}
