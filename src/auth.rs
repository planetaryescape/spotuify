use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use keyring::Entry;
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::form_urlencoded;

use crate::config::Config;

const KEYCHAIN_SERVICE: &str = "spotuify";
const KEYCHAIN_USER: &str = "spotify";
const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(3);
const SCOPES: &[&str] = &[
    "user-read-playback-state",
    "user-read-currently-playing",
    "user-read-recently-played",
    "user-modify-playback-state",
    "user-read-private",
    "playlist-read-private",
    "playlist-read-collaborative",
    "playlist-modify-private",
    "playlist-modify-public",
    "user-library-read",
    "user-library-modify",
];

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StoredToken {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    pub scope: String,
    pub token_type: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    token_type: String,
    expires_in: u64,
    refresh_token: Option<String>,
    scope: Option<String>,
}

pub async fn login(config: &Config) -> Result<()> {
    let verifier = random_string(96);
    let challenge = pkce_challenge(&verifier);
    let state = random_string(32);
    let auth_url = authorization_url(config, &challenge, &state)?;
    let listener = bind_redirect_listener(&config.redirect_uri)?;

    println!("Opening Spotify authorization in your browser...");
    println!("Spotify Dashboard Redirect URI should be one of:");
    println!("  {}", config.redirect_uri);
    println!("  http://127.0.0.1/callback  (loopback dynamic-port allowlist)");
    println!("Do not use the Website field, localhost, or a trailing slash.\n");
    println!("If it does not open, visit:\n{auth_url}\n");
    open::that_detached(auth_url.as_str()).context("failed to open browser")?;

    let code =
        wait_for_code(listener, &state).context("failed while waiting for OAuth redirect")?;
    let token = exchange_code(config, &code, &verifier).await?;
    save_token_bounded(&token)?;
    println!("Spotify auth saved in macOS Keychain.");
    Ok(())
}

pub fn logout() -> Result<()> {
    delete_token_bounded()
}

fn delete_token() -> Result<()> {
    let entry = token_entry()?;
    match entry.delete_credential() {
        Ok(()) => println!("Removed Spotify token from macOS Keychain."),
        Err(keyring::Error::NoEntry) => println!("No Spotify token was stored."),
        Err(err) => return Err(err).context("failed to remove keychain token"),
    }
    Ok(())
}

pub fn token_status() -> Result<Option<String>> {
    let Some(token) = load_token_bounded()? else {
        return Ok(None);
    };

    let now = unix_now();
    let status = if token.expires_at > now {
        let mins = (token.expires_at - now) / 60;
        format!("present, access token expires in {mins}m")
    } else {
        "present, access token expired; refresh token available".to_string()
    };

    Ok(Some(status))
}

pub async fn access_token(config: &Config, http: &Client) -> Result<String> {
    let mut token =
        load_token_bounded()?.ok_or_else(|| anyhow!("not logged in; run `spotuify login`"))?;
    if token.expires_at > unix_now() + 60 {
        return Ok(token.access_token);
    }

    tracing::info!("refreshing Spotify access token");
    token = refresh_token(config, http, &token).await?;
    save_token_bounded(&token)?;
    Ok(token.access_token)
}

fn token_entry() -> Result<Entry> {
    Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER).context("failed to open keychain entry")
}

fn load_token() -> Result<Option<StoredToken>> {
    let entry = token_entry()?;
    match entry.get_password() {
        Ok(raw) => serde_json::from_str(&raw)
            .map(Some)
            .context("stored token is invalid JSON"),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(err).context("failed to read keychain token"),
    }
}

fn load_token_bounded() -> Result<Option<StoredToken>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(load_token());
    });
    recv_keychain_result(rx, "read keychain token")
}

fn save_token(token: &StoredToken) -> Result<()> {
    let raw = serde_json::to_string(token).context("failed to encode token")?;
    token_entry()?
        .set_password(&raw)
        .context("failed to save token to keychain")
}

fn save_token_bounded(token: &StoredToken) -> Result<()> {
    let token = token.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(save_token(&token));
    });
    recv_keychain_result(rx, "save keychain token")
}

fn delete_token_bounded() -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(delete_token());
    });
    recv_keychain_result(rx, "delete keychain token")
}

fn recv_keychain_result<T>(rx: std::sync::mpsc::Receiver<Result<T>>, action: &str) -> Result<T> {
    match rx.recv_timeout(KEYCHAIN_TIMEOUT) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            bail!("timed out trying to {action}")
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            bail!("keychain worker exited while trying to {action}")
        }
    }
}

async fn exchange_code(config: &Config, code: &str, verifier: &str) -> Result<StoredToken> {
    let mut params = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code.to_string()),
        ("redirect_uri", config.redirect_uri.clone()),
        ("client_id", config.client_id.clone()),
        ("code_verifier", verifier.to_string()),
    ];

    let response = Client::builder()
        .connect_timeout(Duration::from_secs(4))
        .read_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(8))
        .build()
        .context("failed to build token HTTP client")?
        .post("https://accounts.spotify.com/api/token")
        .form(&params)
        .send()
        .await
        .context("token request failed")?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("failed to read token response")?;
    if !status.is_success() {
        bail!("Spotify token exchange failed ({status}): {body}");
    }

    let token: TokenResponse =
        serde_json::from_str(&body).context("failed to decode token response")?;
    let refresh_token = token
        .refresh_token
        .ok_or_else(|| anyhow!("Spotify did not return a refresh token"))?;
    params.clear();

    Ok(StoredToken {
        access_token: token.access_token,
        refresh_token,
        expires_at: unix_now() + token.expires_in,
        scope: token.scope.unwrap_or_default(),
        token_type: token.token_type,
    })
}

async fn refresh_token(
    config: &Config,
    http: &Client,
    existing: &StoredToken,
) -> Result<StoredToken> {
    let params = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", existing.refresh_token.clone()),
        ("client_id", config.client_id.clone()),
    ];
    let response = http
        .post("https://accounts.spotify.com/api/token")
        .form(&params)
        .send()
        .await
        .context("token refresh request failed")?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("failed to read refresh response")?;
    if !status.is_success() {
        bail!("Spotify token refresh failed ({status}): {body}");
    }

    let token: TokenResponse =
        serde_json::from_str(&body).context("failed to decode refresh response")?;
    Ok(StoredToken {
        access_token: token.access_token,
        refresh_token: token
            .refresh_token
            .unwrap_or_else(|| existing.refresh_token.clone()),
        expires_at: unix_now() + token.expires_in,
        scope: token.scope.unwrap_or_else(|| existing.scope.clone()),
        token_type: token.token_type,
    })
}

fn authorization_url(config: &Config, challenge: &str, state: &str) -> Result<String> {
    let scope = SCOPES.join(" ");
    let mut serializer = form_urlencoded::Serializer::new(String::new());
    serializer
        .append_pair("client_id", &config.client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &config.redirect_uri)
        .append_pair("scope", &scope)
        .append_pair("state", state)
        .append_pair("code_challenge_method", "S256")
        .append_pair("code_challenge", challenge);
    Ok(format!(
        "https://accounts.spotify.com/authorize?{}",
        serializer.finish()
    ))
}

fn bind_redirect_listener(redirect_uri: &str) -> Result<TcpListener> {
    let url = url::Url::parse(redirect_uri).context("redirect URI is invalid")?;
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("redirect URI host missing"))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow!("redirect URI port missing"))?;
    TcpListener::bind((host, port)).with_context(|| format!("failed to bind {host}:{port}"))
}

fn wait_for_code(listener: TcpListener, expected_state: &str) -> Result<String> {
    listener
        .set_nonblocking(false)
        .context("failed to configure redirect listener")?;
    let (mut stream, _) = listener
        .accept()
        .context("failed to accept OAuth redirect")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(180)))
        .context("failed to set OAuth redirect timeout")?;

    let mut buffer = [0_u8; 4096];
    let bytes = stream
        .read(&mut buffer)
        .context("failed to read OAuth redirect")?;
    let request = String::from_utf8_lossy(&buffer[..bytes]);
    let first_line = request
        .lines()
        .next()
        .ok_or_else(|| anyhow!("empty OAuth redirect request"))?;
    let path = first_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow!("OAuth redirect did not include a path"))?;
    let url = url::Url::parse(&format!("http://127.0.0.1{path}"))?;

    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }

    let response = "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\n\r\n<h1>spotuify login complete</h1><p>You can close this tab.</p>";
    stream
        .write_all(response.as_bytes())
        .context("failed to write OAuth browser response")?;

    if let Some(error) = error {
        bail!("Spotify authorization failed: {error}");
    }
    if state.as_deref() != Some(expected_state) {
        bail!("OAuth state mismatch");
    }
    code.ok_or_else(|| anyhow!("Spotify redirect did not include a code"))
}

fn pkce_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

fn random_string(len: usize) -> String {
    thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
