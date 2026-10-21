//! Authorization code flow with PKCE against the user's own Developer app, and the
//! token file.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, ToSocketAddrs};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::config::{self, Config};

const AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";

pub const SCOPES: &[&str] = &[
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
    "user-read-recently-played",
    "user-top-read",
    "user-library-read",
    "user-library-modify",
    "user-follow-read",
    "playlist-read-private",
    "playlist-read-collaborative",
    "playlist-modify-private",
    "playlist-modify-public",
];

#[derive(Clone, Serialize, Deserialize)]
pub struct Token {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix seconds.
    pub expires_at: u64,
    #[serde(default)]
    pub scope: String,
}

impl Token {
    /// True when the token expires within `margin` seconds.
    pub fn expires_within(&self, margin: u64) -> bool {
        now_secs() + margin >= self.expires_at
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
    #[serde(default)]
    scope: String,
}

impl TokenResponse {
    fn into_token(self, previous_refresh: Option<String>) -> Token {
        Token {
            access_token: self.access_token,
            refresh_token: self.refresh_token.or(previous_refresh),
            expires_at: now_secs() + self.expires_in,
            scope: self.scope,
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn token_path() -> PathBuf {
    config::cache_dir().join("token.json")
}

pub fn load_token() -> Option<Token> {
    let text = std::fs::read_to_string(token_path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// Writes through a temporary file and a rename, so a crash can never leave an empty
/// token file behind (an empty token once made every poll open a browser).
pub fn save_token(token: &Token) -> Result<()> {
    let path = token_path();
    let dir = path.parent().context("token path has no parent")?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(".token.json.tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(serde_json::to_string_pretty(token)?.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub async fn refresh(http: &reqwest::Client, cfg: &Config, token: &Token) -> Result<Token> {
    let refresh_token = token
        .refresh_token
        .clone()
        .context("token has no refresh token; run `pikap auth`")?;
    let response = http
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", cfg.client_id.as_str()),
        ])
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("token refresh failed ({status}): {body}; run `pikap auth`");
    }
    let fresh = response
        .json::<TokenResponse>()
        .await?
        .into_token(Some(refresh_token));
    save_token(&fresh)?;
    Ok(fresh)
}

/// Interactive login for `pikap auth` against the user's Developer app (Web API).
pub async fn login(http: &reqwest::Client, cfg: &Config, announce: impl Fn(&str)) -> Result<Token> {
    let token = authorize(http, &cfg.client_id, &cfg.redirect_uri, SCOPES, announce)
        .await?
        .into_token(None);
    save_token(&token)?;
    Ok(token)
}

/// Interactive login as Spotify's desktop client; the access token is only used once,
/// to obtain librespot's reusable credentials.
pub async fn login_librespot(
    http: &reqwest::Client,
    redirect_uri: &str,
    announce: impl Fn(&str),
) -> Result<String> {
    let scopes = super::meta::SCOPES;
    let client_id = super::meta::DESKTOP_CLIENT_ID;
    Ok(authorize(http, client_id, redirect_uri, scopes, announce)
        .await?
        .access_token)
}

/// Authorization code flow with PKCE: prints the URL, opens the browser and waits for
/// Spotify's redirect on the loopback redirect URI.
async fn authorize(
    http: &reqwest::Client,
    client_id: &str,
    redirect_uri: &str,
    scopes: &[&str],
    announce: impl Fn(&str),
) -> Result<TokenResponse> {
    let verifier = random_url_safe(64)?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_url_safe(16)?;

    let mut url = reqwest::Url::parse(AUTHORIZE_URL)?;
    url.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("code_challenge_method", "S256")
        .append_pair("code_challenge", &challenge)
        .append_pair("state", &state)
        .append_pair("scope", &scopes.join(" "));
    announce(url.as_str());
    let _ = std::process::Command::new("open")
        .arg(url.as_str())
        .status();

    let addr = loopback_addr(redirect_uri)?;
    let expected_state = state.clone();
    let code = tokio::task::spawn_blocking(move || wait_for_code(addr, &expected_state)).await??;

    let response = http
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("token exchange failed ({status}): {body}");
    }
    Ok(response.json::<TokenResponse>().await?)
}

fn loopback_addr(redirect_uri: &str) -> Result<SocketAddr> {
    let url = reqwest::Url::parse(redirect_uri)?;
    let host = url.host_str().context("redirect_uri has no host")?;
    let port = url.port().context("redirect_uri needs an explicit port")?;
    (host, port)
        .to_socket_addrs()?
        .next()
        .context("redirect_uri host does not resolve")
}

/// Waits for the redirect carrying `code`. Browsers prefetch `/favicon.ico` and the like
/// from the same port, so any request without a code is answered and ignored.
fn wait_for_code(addr: SocketAddr, state: &str) -> Result<String> {
    let listener = TcpListener::bind(addr).with_context(|| format!("listening on {addr}"))?;
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            continue;
        }
        let target = line.split_whitespace().nth(1).unwrap_or_default();
        let Ok(url) = reqwest::Url::parse(&format!("http://localhost{target}")) else {
            continue;
        };
        let param = |name: &str| {
            url.query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
        };
        if let Some(error) = param("error") {
            respond(
                &mut stream,
                "pikap: Spotify izin vermedi / authorization was denied.",
            );
            bail!("Spotify denied the authorization: {error}");
        }
        let Some(code) = param("code") else {
            respond(&mut stream, "");
            continue;
        };
        if param("state").as_deref() != Some(state) {
            respond(&mut stream, "pikap: state mismatch, ignored.");
            continue;
        }
        respond(
            &mut stream,
            "pikap: giriş tamam, bu sekmeyi kapatabilirsin. / Signed in, you can close this tab.",
        );
        return Ok(code);
    }
    bail!("callback listener closed")
}

fn respond(stream: &mut std::net::TcpStream, body: &str) {
    let reply = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(reply.as_bytes());
}

fn random_url_safe(len: usize) -> Result<String> {
    let mut bytes = vec![0u8; len];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let mut s = URL_SAFE_NO_PAD.encode(bytes);
    s.truncate(len);
    Ok(s)
}
