use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result as AnyResult};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use spotuify_keychain as keychain;
use tokio::sync::Mutex;

use crate::client::user_agent_string;
use crate::config::Config;
use crate::error::SpotifyResult;
use url::form_urlencoded;

const KEYCHAIN_SERVICE: &str = "spotuify";
const KEYCHAIN_USER: &str = "spotify";
const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(20);
const SCOPES: &[&str] = &[
    "user-read-playback-state",
    "user-read-currently-playing",
    "user-read-recently-played",
    "user-read-playback-position",
    "user-modify-playback-state",
    "user-read-private",
    "playlist-read-private",
    "playlist-read-collaborative",
    "playlist-modify-private",
    "playlist-modify-public",
    "user-library-read",
    "user-library-modify",
    "user-follow-read",
    "user-follow-modify",
    // Embedded librespot playback uses the Web Playback SDK
    // streaming scope + app-remote-control to drive transport.
    "streaming",
    "app-remote-control",
];

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StoredToken {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    pub scope: String,
    pub token_type: String,
}

pub fn missing_required_scopes(token: &StoredToken) -> Vec<&'static str> {
    let granted = token.scope.split_whitespace().collect::<Vec<_>>();
    SCOPES
        .iter()
        .copied()
        .filter(|scope| !granted.contains(scope))
        .collect()
}

/// Pure check used by the daemon to decide whether to proactively
/// surface a "re-auth required" banner at startup.
///
/// Returns `true` only when a token exists *and* it is missing one or
/// more scopes that the current `SCOPES` constant requires. `None`
/// (not logged in yet) and a fully-scoped token both return `false` —
/// neither case warrants a banner.
pub fn token_needs_scope_reauth(token: Option<&StoredToken>) -> bool {
    token
        .map(|t| !missing_required_scopes(t).is_empty())
        .unwrap_or(false)
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    token_type: String,
    expires_in: u64,
    refresh_token: Option<String>,
    scope: Option<String>,
}

pub async fn login(config: &Config) -> SpotifyResult<()> {
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

pub fn logout() -> SpotifyResult<()> {
    Ok(delete_token_bounded()?)
}

fn delete_token() -> AnyResult<()> {
    match keychain::delete_password(KEYCHAIN_SERVICE, KEYCHAIN_USER) {
        Ok(()) => println!("Removed Spotify token from system keychain."),
        Err(err) if err.is_no_entry() => println!("No Spotify token was stored."),
        Err(err) => return Err(anyhow!("failed to remove keychain token: {err}")),
    }
    Ok(())
}

pub fn token_status() -> SpotifyResult<Option<String>> {
    let Some(token) = load_token_bounded()? else {
        return Ok(None);
    };

    Ok(Some(token_status_message(&token, unix_now())))
}

fn token_status_message(token: &StoredToken, now: u64) -> String {
    let mut status = if token.expires_at > now {
        let mins = (token.expires_at - now) / 60;
        format!("present, access token expires in {mins}m")
    } else {
        "present, access token expired; refresh token available".to_string()
    };

    let missing = missing_required_scopes(token);
    if !missing.is_empty() {
        status.push_str("; missing scopes: ");
        status.push_str(&missing.join(", "));
        status.push_str("; run `spotuify login`");
    }
    status
}

pub async fn access_token_cached(
    config: &Config,
    http: &Client,
    cache: &Arc<Mutex<Option<StoredToken>>>,
) -> SpotifyResult<String> {
    // Single-flight token acquisition keeps cold concurrent daemon requests from
    // triggering multiple macOS Keychain prompts.
    let mut cached = cache.lock().await;
    let mut token = match cached.clone() {
        Some(token) => token,
        None => {
            load_token_bounded()?.ok_or_else(|| anyhow!("not logged in; run `spotuify login`"))?
        }
    };

    // Phase 6.8: route the refresh decision through the typed
    // refresh_planner so the (Phase 6.8 test suite) PROACTIVE_HEADROOM
    // is the single source of truth.
    if !crate::refresh_planner::should_refresh(
        unix_now() as i64,
        token.expires_at as i64,
        crate::refresh_planner::PROACTIVE_HEADROOM,
    ) {
        *cached = Some(token.clone());
        return Ok(token.access_token);
    }

    tracing::info!("refreshing Spotify access token (proactive or due)");
    token = refresh_token(config, http, &token).await?;
    save_token_bounded(&token)?;
    *cached = Some(token.clone());
    Ok(token.access_token)
}

pub async fn refresh_access_token_cached(
    config: &Config,
    http: &Client,
    cache: &Arc<Mutex<Option<StoredToken>>>,
) -> SpotifyResult<String> {
    let mut cached = cache.lock().await;
    let token = match cached.clone() {
        Some(token) => token,
        None => {
            load_token_bounded()?.ok_or_else(|| anyhow!("not logged in; run `spotuify login`"))?
        }
    };
    tracing::info!("refreshing Spotify access token after 401");
    let token = refresh_token(config, http, &token).await?;
    save_token_bounded(&token)?;
    *cached = Some(token.clone());
    Ok(token.access_token)
}

/// Snapshot the stored Spotify token from the system keychain so
/// callers (e.g. the daemon's startup check) can inspect its scopes
/// without going through the refresh path. Returns `Ok(None)` when the
/// user isn't logged in yet.
pub fn stored_token_snapshot() -> SpotifyResult<Option<StoredToken>> {
    Ok(load_token_bounded()?)
}

/// File-backed mirror of the keychain entry, kept beside the rest of
/// the daemon's data. Exists because the macOS Keychain prompts the
/// user (via GUI dialog) for permission whenever a binary with a new
/// code signature wants to read an entry — and a backgrounded daemon
/// can't show that dialog, so the read hangs until the 20 s timeout.
///
/// The disk cache lives at `<data_dir>/auth/token.json` with mode
/// 0600. On read we try disk first; if absent or invalid, we fall
/// through to the keychain (which prompts once, and the result gets
/// written to disk so future reads bypass the prompt entirely). On
/// save we write to both so the keychain stays the source of truth
/// for `spotuify login`/`spotuify logout` semantics.
fn token_cache_file() -> PathBuf {
    spotuify_protocol::paths::data_dir()
        .join("auth")
        .join("token.json")
}

fn load_token_from_disk() -> Option<StoredToken> {
    let path = token_cache_file();
    let raw = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<StoredToken>(&raw) {
        Ok(token) => Some(token),
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "token cache file is invalid JSON; falling through to keychain"
            );
            None
        }
    }
}

fn save_token_to_disk(token: &StoredToken) {
    let path = token_cache_file();
    let Some(parent) = path.parent() else {
        return;
    };
    if let Err(err) = std::fs::create_dir_all(parent) {
        tracing::warn!(path = %parent.display(), error = %err, "failed to create token cache dir");
        return;
    }
    let raw = match serde_json::to_string(token) {
        Ok(raw) => raw,
        Err(err) => {
            tracing::warn!(error = %err, "failed to encode token for disk cache");
            return;
        }
    };
    if let Err(err) = atomic_write_mode_0600(&path, raw.as_bytes()) {
        tracing::warn!(path = %path.display(), error = %err, "failed to write token cache");
    }
}

fn delete_token_from_disk() {
    let path = token_cache_file();
    let _ = std::fs::remove_file(&path);
}

#[cfg(unix)]
fn atomic_write_mode_0600(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut tmp = path.to_path_buf();
    tmp.set_extension("json.tmp");
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(not(unix))]
fn atomic_write_mode_0600(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

fn load_token() -> AnyResult<Option<StoredToken>> {
    // Try the disk cache first. If it hits, we skip the keychain
    // entirely — no GUI prompt, no 20 s hang for detached daemons.
    if let Some(token) = load_token_from_disk() {
        return Ok(Some(token));
    }
    // Fall through: ask the keychain (may prompt), then mirror to
    // disk so the prompt never fires again for this binary.
    match keychain::get_password(KEYCHAIN_SERVICE, KEYCHAIN_USER) {
        Ok(raw) => match serde_json::from_str::<StoredToken>(&raw) {
            Ok(token) => {
                save_token_to_disk(&token);
                Ok(Some(token))
            }
            Err(err) => Err(anyhow!("stored token is invalid JSON: {err}")),
        },
        Err(err) if err.is_no_entry() => Ok(None),
        Err(err) => Err(anyhow!("failed to read keychain token: {err}")),
    }
}

fn load_token_bounded() -> AnyResult<Option<StoredToken>> {
    // Fast path: disk hit. Bypass the worker-thread + timeout dance
    // entirely so a cold daemon doesn't pay a 20 s ceiling on its
    // first read.
    if let Some(token) = load_token_from_disk() {
        return Ok(Some(token));
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(load_token());
    });
    recv_keychain_result(rx, "read keychain token")
}

fn save_token(token: &StoredToken) -> AnyResult<()> {
    let raw = serde_json::to_string(token).context("failed to encode token")?;
    keychain::set_password(KEYCHAIN_SERVICE, KEYCHAIN_USER, &raw)
        .map_err(|err| anyhow!("failed to save token to keychain: {err}"))?;
    // Mirror to disk so the next cold-start daemon doesn't have to
    // prompt the keychain again.
    save_token_to_disk(token);
    Ok(())
}

fn save_token_bounded(token: &StoredToken) -> AnyResult<()> {
    let token = token.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(save_token(&token));
    });
    recv_keychain_result(rx, "save keychain token")
}

fn delete_token_bounded() -> AnyResult<()> {
    // Clear the disk cache first regardless of keychain outcome —
    // we never want a stale on-disk token to outlive an explicit
    // logout, even if the keychain delete races or fails.
    delete_token_from_disk();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(delete_token());
    });
    recv_keychain_result(rx, "delete keychain token")
}

fn recv_keychain_result<T>(
    rx: std::sync::mpsc::Receiver<AnyResult<T>>,
    action: &str,
) -> AnyResult<T> {
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

async fn exchange_code(config: &Config, code: &str, verifier: &str) -> AnyResult<StoredToken> {
    let mut params = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code.to_string()),
        ("redirect_uri", config.redirect_uri.clone()),
        ("client_id", config.client_id.clone()),
        ("code_verifier", verifier.to_string()),
    ];

    let response = Client::builder()
        .user_agent(user_agent_string())
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
) -> AnyResult<StoredToken> {
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
    Ok(merge_refresh_response(existing, token, unix_now()))
}

fn merge_refresh_response(existing: &StoredToken, token: TokenResponse, now: u64) -> StoredToken {
    StoredToken {
        access_token: token.access_token,
        refresh_token: token
            .refresh_token
            .unwrap_or_else(|| existing.refresh_token.clone()),
        expires_at: now + token.expires_in,
        scope: token.scope.unwrap_or_else(|| existing.scope.clone()),
        token_type: token.token_type,
    }
}

fn authorization_url(config: &Config, challenge: &str, state: &str) -> AnyResult<String> {
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

fn bind_redirect_listener(redirect_uri: &str) -> AnyResult<TcpListener> {
    let url = url::Url::parse(redirect_uri).context("redirect URI is invalid")?;
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("redirect URI host missing"))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow!("redirect URI port missing"))?;
    TcpListener::bind((host, port)).with_context(|| format!("failed to bind {host}:{port}"))
}

fn wait_for_code(listener: TcpListener, expected_state: &str) -> AnyResult<String> {
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

#[cfg(test)]
mod tests {
    use super::{authorization_url, merge_refresh_response, StoredToken, TokenResponse};
    use crate::config::Config;

    fn config() -> Config {
        Config {
            client_id: "client-id".to_string(),
            client_secret: None,
            redirect_uri: "http://127.0.0.1:8888/callback".to_string(),
            config_path: "spotuify.toml".into(),
            player: crate::config::PlayerConfig::default(),
            cache: crate::config::CacheConfig::default(),
            analytics: crate::config::AnalyticsConfig::default(),
            notifications: crate::config::NotificationsConfig::default(),
            discord: crate::config::DiscordConfig::default(),
            viz: crate::config::VizConfig::default(),
        }
    }

    fn existing_token() -> StoredToken {
        StoredToken {
            access_token: "old-access".to_string(),
            refresh_token: "old-refresh".to_string(),
            expires_at: 10,
            scope: "user-read-playback-state".to_string(),
            token_type: "Bearer".to_string(),
        }
    }

    #[test]
    fn refresh_response_without_refresh_token_preserves_existing_refresh_token() {
        let token = merge_refresh_response(
            &existing_token(),
            TokenResponse {
                access_token: "new-access".to_string(),
                refresh_token: None,
                expires_in: 3_600,
                scope: None,
                token_type: "Bearer".to_string(),
            },
            100,
        );

        assert_eq!(token.access_token, "new-access");
        assert_eq!(token.refresh_token, "old-refresh");
        assert_eq!(token.scope, "user-read-playback-state");
        assert_eq!(token.expires_at, 3_700);
    }

    #[test]
    fn refresh_response_with_refresh_token_replaces_old_refresh_token() {
        let token = merge_refresh_response(
            &existing_token(),
            TokenResponse {
                access_token: "new-access".to_string(),
                refresh_token: Some("new-refresh".to_string()),
                expires_in: 3_600,
                scope: Some("playlist-read-private".to_string()),
                token_type: "Bearer".to_string(),
            },
            100,
        );

        assert_eq!(token.refresh_token, "new-refresh");
        assert_eq!(token.scope, "playlist-read-private");
    }

    #[test]
    fn authorization_url_requests_follow_read_and_modify_scopes() {
        let url = authorization_url(&config(), "challenge", "state").expect("auth url");
        let parsed = url::Url::parse(&url).expect("valid url");
        let scope = parsed
            .query_pairs()
            .find_map(|(key, value)| (key == "scope").then(|| value.into_owned()))
            .expect("scope query parameter");
        let scopes = scope.split_whitespace().collect::<Vec<_>>();

        assert!(scopes.contains(&"user-follow-read"), "{scopes:?}");
        assert!(scopes.contains(&"user-follow-modify"), "{scopes:?}");
    }

    #[test]
    fn token_status_tells_user_to_relogin_when_existing_token_lacks_new_scopes() {
        let message = super::token_status_message(&existing_token(), 1);

        assert!(message.contains("missing scopes: user-read-currently-playing"));
        assert!(message.contains("user-follow-read"));
        assert!(message.contains("user-follow-modify"));
        assert!(message.contains("run `spotuify login`"));
    }
}
