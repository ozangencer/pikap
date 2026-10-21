//! Web API client with quota discipline for Development Mode apps:
//! Development Mode apps get small per-endpoint daily quotas, so a long `Retry-After`
//! puts that endpoint on hold instead of being retried, and reads Spotify refuses
//! (403/404) are not asked again for a while.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;

use super::auth::{self, Token};
use super::config::Config;
use crate::log::plog;

const BASE: &str = "https://api.spotify.com/v1/";
const MAX_RETRIES: u32 = 4;
const BASE_DELAY: Duration = Duration::from_secs(2);
/// Longer waits (daily quotas) put the endpoint on hold rather than blocking.
const MAX_INLINE_WAIT: Duration = Duration::from_secs(120);
const REFUSED_READ_HOLD: Duration = Duration::from_secs(600);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quota {
    Normal,
    Retrying,
    Holding(usize),
}

/// An endpoint is on hold until the given time; the caller should not treat it as a failure.
#[derive(Debug)]
pub struct OnHold(pub Duration);

impl std::fmt::Display for OnHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "on hold for {}s", self.0.as_secs())
    }
}

impl std::error::Error for OnHold {}

pub struct Api {
    http: reqwest::Client,
    cfg: Config,
    token: tokio::sync::Mutex<Token>,
    holds: Mutex<HashMap<String, Instant>>,
    retrying: AtomicBool,
}

impl Api {
    pub fn new(http: reqwest::Client, cfg: Config, token: Token) -> Self {
        Self {
            http,
            cfg,
            token: tokio::sync::Mutex::new(token),
            holds: Mutex::new(HashMap::new()),
            retrying: AtomicBool::new(false),
        }
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn quota(&self) -> Quota {
        let now = Instant::now();
        let holding = self
            .holds
            .lock()
            .map_or(0, |h| h.values().filter(|until| **until > now).count());
        if holding > 0 {
            Quota::Holding(holding)
        } else if self.retrying.load(Ordering::Relaxed) {
            Quota::Retrying
        } else {
            Quota::Normal
        }
    }

    /// GET returning `None` for `204 No Content` (e.g. nothing is playing).
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>> {
        match self.request(Method::GET, path, None).await? {
            Some(body) if !body.trim().is_empty() => Ok(Some(serde_json::from_str(&body)?)),
            _ => Ok(None),
        }
    }

    pub async fn send(&self, method: Method, path: &str) -> Result<()> {
        self.request(method, path, None).await.map(|_| ())
    }

    pub async fn send_json(
        &self,
        method: Method,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<()> {
        self.request(method, path, Some(body)).await.map(|_| ())
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<Option<String>> {
        let key = endpoint_key(&method, path);
        if let Some(left) = self.hold_left(&key) {
            return Err(OnHold(left).into());
        }
        let mut refreshed = false;
        let mut attempt = 0;
        loop {
            let access = self.access_token(false).await?;
            let request = self
                .http
                .request(method.clone(), format!("{BASE}{path}"))
                .bearer_auth(access);
            // Spotify answers 411 to a bodiless PUT/POST without an explicit length.
            let request = match body {
                Some(body) => request.json(body),
                None => request.header(reqwest::header::CONTENT_LENGTH, 0),
            };
            let started = Instant::now();
            let response = request.send().await?;
            let status = response.status();
            plog!(
                "{method} {path} -> {} ({} ms)",
                status.as_u16(),
                started.elapsed().as_millis()
            );
            if status.is_success() {
                self.retrying.store(false, Ordering::Relaxed);
                return Ok(Some(response.text().await?));
            }
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            let body = response.text().await.unwrap_or_default();
            match status {
                StatusCode::UNAUTHORIZED if !refreshed => {
                    refreshed = true;
                    self.access_token(true).await?;
                }
                StatusCode::TOO_MANY_REQUESTS => {
                    let wait = retry_after.unwrap_or(BASE_DELAY * 2u32.pow(attempt));
                    if wait > MAX_INLINE_WAIT || attempt >= MAX_RETRIES {
                        self.hold(key, wait);
                        self.retrying.store(false, Ordering::Relaxed);
                        return Err(OnHold(wait).into());
                    }
                    self.retrying.store(true, Ordering::Relaxed);
                    attempt += 1;
                    tokio::time::sleep(wait).await;
                }
                StatusCode::FORBIDDEN | StatusCode::NOT_FOUND if method == Method::GET => {
                    self.hold(key, REFUSED_READ_HOLD);
                    bail!("{method} {path}: {status} {}", short(&body));
                }
                _ => bail!("{method} {path}: {status} {}", short(&body)),
            }
        }
    }

    async fn access_token(&self, force_refresh: bool) -> Result<String> {
        let mut token = self.token.lock().await;
        if force_refresh || token.expires_within(60) {
            *token = auth::refresh(&self.http, &self.cfg, &token).await?;
        }
        Ok(token.access_token.clone())
    }

    fn hold_left(&self, key: &str) -> Option<Duration> {
        let holds = self.holds.lock().ok()?;
        let until = holds.get(key)?;
        until.checked_duration_since(Instant::now())
    }

    fn hold(&self, key: String, wait: Duration) {
        if let Ok(mut holds) = self.holds.lock() {
            holds.insert(key, Instant::now() + wait);
        }
    }
}

/// Holds apply per endpoint, not per resource: `GET playlists/{id}/items` shares one
/// quota whatever the id, so ids and query strings are dropped from the key.
fn endpoint_key(method: &Method, path: &str) -> String {
    let path = path.split('?').next().unwrap_or(path);
    let segments: Vec<&str> = path
        .split('/')
        .map(|s| if looks_like_id(s) { ":id" } else { s })
        .collect();
    format!("{method} {}", segments.join("/"))
}

fn looks_like_id(s: &str) -> bool {
    s.len() == 22 && s.chars().all(|c| c.is_ascii_alphanumeric())
}

fn short(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| body.chars().take(120).collect())
}

pub fn is_on_hold(err: &anyhow::Error) -> bool {
    err.downcast_ref::<OnHold>().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_ignore_ids_and_queries() {
        assert_eq!(
            endpoint_key(
                &Method::GET,
                "playlists/37i9dQZF1E36c3f87B1Xo0/items?limit=50"
            ),
            "GET playlists/:id/items"
        );
        assert_eq!(
            endpoint_key(&Method::PUT, "me/player/pause"),
            "PUT me/player/pause"
        );
    }
}
