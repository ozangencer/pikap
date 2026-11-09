//! YouTube Music's internal API (innertube), signed in with a browser cookie or not
//! at all. ytmapi-rs opens the session and signs the requests; the answers are read
//! by [`super::parse`], because its own parsers drop the artist and album ids pikap
//! needs and miss parts of the artist page.

use std::borrow::Cow;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};
use ytmapi_rs::auth::noauth::NoAuthToken;
use ytmapi_rs::auth::{AuthToken, BrowserToken};
use ytmapi_rs::parse::{ParseFrom, ProcessedResult};
use ytmapi_rs::query::{PostMethod, PostQuery, Query};
use ytmapi_rs::{YtMusic, YtMusicBuilder};

use crate::spotify::config;

/// A `Cookie:` header copied from music.youtube.com; present means signed in.
pub fn cookie_path() -> PathBuf {
    config::config_dir().join("ytmusic-cookie.txt")
}

pub fn read_cookie() -> Option<String> {
    let text = std::fs::read_to_string(cookie_path()).ok()?;
    let header = text
        .trim()
        .trim_start_matches("cookie:")
        .trim_start_matches("Cookie:")
        .trim();
    header.contains("SAPISID=").then(|| header.to_owned())
}

enum Session {
    In(YtMusic<BrowserToken>),
    Out(YtMusic<NoAuthToken>),
}

pub struct Client {
    session: Session,
}

impl Client {
    pub async fn connect(cookie: Option<&str>) -> Result<Self> {
        let session = match cookie {
            Some(cookie) => Session::In(
                YtMusicBuilder::new_with_client(http()?)
                    .with_browser_token_cookie(cookie.to_owned())
                    .build()
                    .await
                    .context("YouTube Music sign-in")?,
            ),
            None => Session::Out(
                YtMusicBuilder::new_with_client(http()?)
                    .build()
                    .await
                    .context("YouTube Music")?,
            ),
        };
        Ok(Self { session })
    }

    pub fn signed_in(&self) -> bool {
        matches!(self.session, Session::In(_))
    }

    /// One innertube call: `path` is `browse`, `next`, `search`, `like/like`…, `body`
    /// the request fields next to the client context.
    pub async fn call(&self, path: &str, body: Value) -> Result<Value> {
        let Value::Object(body) = body else {
            bail!("innertube body must be an object");
        };
        let query = Raw {
            path: path.to_owned(),
            body,
        };
        let text = match &self.session {
            Session::In(yt) => yt.raw_json_query(query).await,
            Session::Out(yt) => yt.raw_json_query(query).await,
        }
        .map_err(|e| {
            anyhow::Error::new(crate::backend::Problem::Unreachable)
                .context(format!("YouTube Music {path}: {e}"))
        })?;
        Ok(serde_json::from_str(&text)?)
    }

    /// Whether the cookie really opened an account: signed out, the library answers
    /// with a "looking for what you've liked?" message instead of the playlist grid.
    pub async fn has_account(&self) -> Result<bool> {
        let library = self.browse("FEmusic_liked_playlists").await?;
        Ok(opened_account(&library))
    }

    pub async fn browse(&self, id: &str) -> Result<Value> {
        self.call("browse", serde_json::json!({ "browseId": id }))
            .await
    }
}

/// Whether the `FEmusic_liked_playlists` answer is a signed-in library.
pub fn opened_account(library: &Value) -> bool {
    library.to_string().contains("\"gridRenderer\"")
}

/// Google resets ytmapi-rs's (old Firefox) user agent over HTTP/2; HTTP/1 answers.
/// English answers keep the few words the parser reads ("Single", "EP") the same.
fn http() -> Result<ytmapi_rs::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT_LANGUAGE,
        reqwest::header::HeaderValue::from_static("en-US,en;q=0.9"),
    );
    let inner = reqwest::Client::builder()
        .http1_only()
        .default_headers(headers)
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    Ok(ytmapi_rs::Client::new_from_reqwest_client(inner))
}

/// Any innertube endpoint, answered as raw JSON.
struct Raw {
    path: String,
    body: Map<String, Value>,
}

#[derive(Debug)]
struct Unparsed;

impl ParseFrom<Raw> for Unparsed {
    fn parse_from(_: ProcessedResult<Raw>) -> ytmapi_rs::Result<Self> {
        Ok(Unparsed)
    }
}

impl<A: AuthToken> Query<A> for Raw {
    type Output = Unparsed;
    type Method = PostMethod;
}

impl PostQuery for Raw {
    fn header(&self) -> Map<String, Value> {
        self.body.clone()
    }
    fn params(&self) -> Vec<(&str, Cow<'_, str>)> {
        vec![]
    }
    fn path(&self) -> &str {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sign_in_prompt_is_not_an_account() {
        let signed_out = serde_json::json!({ "contents": { "messageRenderer": {
            "text": { "runs": [{ "text": "Looking for what you’ve liked?" }] }
        }}});
        let signed_in = serde_json::json!({ "contents": { "gridRenderer": { "items": [] } } });
        assert!(!opened_account(&signed_out));
        assert!(opened_account(&signed_in));
    }
}
