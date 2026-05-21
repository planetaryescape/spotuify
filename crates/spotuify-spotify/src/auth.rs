use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result as AnyResult};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use fs2::FileExt;
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use spotuify_keychain as keychain;
use tokio::sync::Mutex;

use crate::client::user_agent_string;
use crate::config::Config;
use crate::error::{SpotifyError, SpotifyResult};
use url::form_urlencoded;

const KEYCHAIN_SERVICE: &str = "spotuify";
const KEYCHAIN_USER: &str = "spotify";
const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(20);
const TOKEN_LOCK_TIMEOUT: Duration = Duration::from_secs(15);
const TOKEN_LOCK_POLL: Duration = Duration::from_millis(50);
const SPOTIFY_TOKEN_ENDPOINT: &str = "https://accounts.spotify.com/api/token";

#[cfg(test)]
static TEST_TOKEN_ENDPOINT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

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

/// Progress events emitted during the OAuth flow. Callers (CLI, TUI,
/// MCP) decide how to render — `print!` to stdout, push into a UI
/// channel, log structured metrics, etc. The auth code itself never
/// writes to the terminal so the TUI's alt-screen buffer is never
/// corrupted by a concurrent `println!`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginProgress {
    OpeningBrowser {
        auth_url: String,
        redirect_uri: String,
    },
    BrowserLaunchFailed {
        auth_url: String,
        redirect_uri: String,
        error: String,
    },
    WaitingForCallback,
    Saved,
}

pub async fn login(
    config: &Config,
    mut progress: impl FnMut(LoginProgress) + Send,
) -> SpotifyResult<()> {
    let verifier = random_string(96);
    let challenge = pkce_challenge(&verifier);
    let state = random_string(32);
    let auth_url = authorization_url(config, &challenge, &state)?;
    let listener = bind_redirect_listener(&config.redirect_uri)?;

    progress(LoginProgress::OpeningBrowser {
        auth_url: auth_url.clone(),
        redirect_uri: config.redirect_uri.clone(),
    });
    // Headless / SSH fallback: `open::that_detached` errors when there's
    // no DISPLAY or no registered browser handler. Don't bail — surface
    // the URL through the progress sink so the caller can show it
    // prominently, and keep listening on the callback socket so the
    // user can complete the flow by pasting the URL into any browser
    // (possibly on a different machine, with the loopback port
    // forwarded over SSH).
    if let Err(err) = open::that_detached(auth_url.as_str()) {
        tracing::warn!(error = %err, "browser launch failed; falling back to manual URL");
        progress(LoginProgress::BrowserLaunchFailed {
            auth_url: auth_url.clone(),
            redirect_uri: config.redirect_uri.clone(),
            error: err.to_string(),
        });
    }

    progress(LoginProgress::WaitingForCallback);
    let code =
        wait_for_code(listener, &state).context("failed while waiting for OAuth redirect")?;
    let token = exchange_code(config, &code, &verifier).await?;
    save_token_bounded(&token)?;
    progress(LoginProgress::Saved);
    Ok(())
}

pub fn logout() -> SpotifyResult<()> {
    Ok(delete_token_bounded()?)
}

fn keychain_get_token() -> Result<String, keychain::KeychainError> {
    #[cfg(test)]
    {
        Err(keychain::KeychainError::NoEntry {
            service: KEYCHAIN_SERVICE.to_string(),
            account: KEYCHAIN_USER.to_string(),
        })
    }
    #[cfg(not(test))]
    {
        keychain::get_password(KEYCHAIN_SERVICE, KEYCHAIN_USER)
    }
}

fn keychain_set_token(raw: &str) -> Result<(), keychain::KeychainError> {
    #[cfg(test)]
    {
        let _ = raw;
        Ok(())
    }
    #[cfg(not(test))]
    {
        keychain::set_password(KEYCHAIN_SERVICE, KEYCHAIN_USER, raw)
    }
}

fn keychain_delete_token() -> Result<(), keychain::KeychainError> {
    #[cfg(test)]
    {
        Ok(())
    }
    #[cfg(not(test))]
    {
        keychain::delete_password(KEYCHAIN_SERVICE, KEYCHAIN_USER)
    }
}

fn delete_token(verbose: bool) -> AnyResult<()> {
    match keychain_delete_token() {
        Ok(()) if verbose => println!("Removed Spotify token from system keychain."),
        Ok(()) => {}
        Err(err) if err.is_no_entry() && verbose => println!("No Spotify token was stored."),
        Err(err) if err.is_no_entry() => {}
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
    let token = match cached.clone() {
        Some(token) => token,
        None => load_token_bounded()?.ok_or(SpotifyError::AuthRequired)?,
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
    let _lock = acquire_token_store_lock_bounded()?;
    let token = load_token_bounded()?.unwrap_or(token);
    if !should_refresh_token(&token) {
        *cached = Some(token.clone());
        return Ok(token.access_token);
    }

    refresh_access_token_locked(config, http, &mut cached, &token)
        .await
        .map(|token| token.access_token)
}

pub async fn refresh_access_token_cached(
    config: &Config,
    http: &Client,
    cache: &Arc<Mutex<Option<StoredToken>>>,
) -> SpotifyResult<String> {
    let mut cached = cache.lock().await;
    let token = match cached.clone() {
        Some(token) => token,
        None => load_token_bounded()?.ok_or(SpotifyError::AuthRequired)?,
    };
    tracing::info!("refreshing Spotify access token after 401");
    let _lock = acquire_token_store_lock_bounded()?;
    let token = load_token_bounded()?.unwrap_or(token);
    if cached
        .as_ref()
        .is_some_and(|old| token_changed(old, &token))
        && !should_refresh_token(&token)
    {
        *cached = Some(token.clone());
        return Ok(token.access_token);
    }

    refresh_access_token_locked(config, http, &mut cached, &token)
        .await
        .map(|token| token.access_token)
}

/// Snapshot the stored Spotify token from the system keychain so
/// callers (e.g. the daemon's startup check) can inspect its scopes
/// without going through the refresh path. Returns `Ok(None)` when the
/// user isn't logged in yet.
pub fn stored_token_snapshot() -> SpotifyResult<Option<StoredToken>> {
    Ok(load_token_bounded()?)
}

pub fn disk_token_cache_status() -> String {
    let path = token_cache_file();
    let state = match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => "present",
        Ok(_) => "non-file",
        Err(err) if err.kind() == ErrorKind::NotFound => "absent",
        Err(_) => "unreadable",
    };
    format!(
        "{state}; full OAuth token mirror at {} with mode 0600 on Unix",
        path.display()
    )
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
fn token_cache_dir() -> PathBuf {
    spotuify_protocol::paths::data_dir().join("auth")
}

fn token_cache_file() -> PathBuf {
    token_cache_dir().join("token.json")
}

fn token_lock_file() -> PathBuf {
    token_cache_dir().join("token.lock")
}

#[derive(Debug)]
struct TokenStoreLock {
    file: File,
}

impl Drop for TokenStoreLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn acquire_token_store_lock_bounded() -> AnyResult<TokenStoreLock> {
    acquire_token_store_lock_with_timeout(TOKEN_LOCK_TIMEOUT)
}

fn acquire_token_store_lock_with_timeout(timeout: Duration) -> AnyResult<TokenStoreLock> {
    let path = token_lock_file();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create Spotify token lock dir {}",
                parent.display()
            )
        })?;
    }

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("failed to open Spotify token lock {}", path.display()))?;
    let started = Instant::now();
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(TokenStoreLock { file }),
            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                if started.elapsed() >= timeout {
                    bail!(
                        "timed out waiting for Spotify token lock at {}",
                        path.display()
                    );
                }
                let remaining = timeout.saturating_sub(started.elapsed());
                std::thread::sleep(std::cmp::min(TOKEN_LOCK_POLL, remaining));
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("failed to lock Spotify token store {}", path.display())
                });
            }
        }
    }
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
    let Some(parent) = path.parent() else {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            "token cache path has no parent",
        ));
    };
    std::fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "token".into());
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tmp = parent.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), nonce));
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
    let result = std::fs::rename(&tmp, path);
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
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
    match keychain_get_token() {
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
    let token_result = recv_keychain_result(rx, "read keychain token");
    #[cfg(target_os = "macos")]
    {
        match token_result {
            Ok(token) => Ok(token),
            Err(err) => {
                tracing::debug!(
                    error = %err,
                    "keychain crate read failed; trying macOS security CLI fallback"
                );
                load_token_via_security_cli()
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        token_result
    }
}

#[cfg(target_os = "macos")]
fn load_token_via_security_cli() -> AnyResult<Option<StoredToken>> {
    use std::process::{Command, Stdio};

    let mut child = Command::new("/usr/bin/security")
        .args([
            "find-generic-password",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            KEYCHAIN_USER,
            "-w",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to start macOS security CLI")?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().context("failed to poll security CLI")? {
            if !status.success() {
                return Err(anyhow!("macOS security CLI could not read Spotify token"));
            }
            let mut raw = String::new();
            child
                .stdout
                .take()
                .context("security CLI stdout unavailable")?
                .read_to_string(&mut raw)
                .context("failed to read security CLI output")?;
            let token = serde_json::from_str::<StoredToken>(raw.trim())
                .context("stored token from security CLI is invalid JSON")?;
            save_token_to_disk(&token);
            return Ok(Some(token));
        }
        if started.elapsed() >= KEYCHAIN_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timed out trying to read keychain token via macOS security CLI");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn save_token(token: &StoredToken) -> AnyResult<()> {
    let raw = serde_json::to_string(token).context("failed to encode token")?;
    keychain_set_token(&raw).map_err(|err| anyhow!("failed to save token to keychain: {err}"))?;
    // Mirror to disk so the next cold-start daemon doesn't have to
    // prompt the keychain again.
    save_token_to_disk(token);
    Ok(())
}

fn save_token_bounded(token: &StoredToken) -> AnyResult<()> {
    let _lock = acquire_token_store_lock_bounded()?;
    save_token_unlocked_bounded(token)
}

fn save_token_unlocked_bounded(token: &StoredToken) -> AnyResult<()> {
    let token = token.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(save_token(&token));
    });
    recv_keychain_result(rx, "save keychain token")
}

fn delete_token_bounded() -> AnyResult<()> {
    let _lock = acquire_token_store_lock_bounded()?;
    delete_token_unlocked_bounded(true)
}

fn delete_token_unlocked_bounded(verbose: bool) -> AnyResult<()> {
    // Clear the disk cache first regardless of keychain outcome —
    // we never want a stale on-disk token to outlive an explicit
    // logout, even if the keychain delete races or fails.
    delete_token_from_disk();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(delete_token(verbose));
    });
    recv_keychain_result(rx, "delete keychain token")
}

fn purge_revoked_token_unlocked(
    cache: &mut Option<StoredToken>,
    failed: &StoredToken,
) -> Option<StoredToken> {
    match load_token_bounded() {
        Ok(Some(current)) if token_changed(failed, &current) => {
            *cache = Some(current);
            tracing::info!(
                "Spotify refresh token was replaced while revoked refresh was in-flight; keeping newer token"
            );
            cache.clone()
        }
        Ok(_) | Err(_) => {
            *cache = None;
            delete_token_from_disk();
            if let Err(err) = delete_token_unlocked_bounded(false) {
                tracing::warn!(
                    error = %err,
                    "failed to clear revoked Spotify token from keychain; re-login will overwrite it"
                );
            }
            None
        }
    }
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
        .post(token_endpoint())
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
        .post(token_endpoint())
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
        // Spotify returns 400 + body `{"error":"invalid_grant", ...}` when
        // the refresh token has been revoked (Spotify-side: user logged out
        // everywhere, password reset, app removed from authorized apps).
        // Surface as a typed AuthRevoked so daemon middleware can emit a
        // sticky AuthError event and the TUI shows a re-login banner
        // instead of letting downstream playback fail silently.
        if status == reqwest::StatusCode::BAD_REQUEST
            && (body.contains("invalid_grant") || body.contains("Refresh token revoked"))
        {
            // Log enough of the Spotify response to confirm it's a
            // real revocation (vs. a malformed request masquerading as
            // invalid_grant). The body is small and contains no PII —
            // just `{"error":"invalid_grant","error_description":"..."}`.
            let snippet = body.chars().take(256).collect::<String>();
            tracing::warn!(
                status = %status,
                body_snippet = %snippet,
                "Spotify refresh token revoked — surfacing AuthRevoked",
            );
            return Err(anyhow::Error::new(SpotifyError::AuthRevoked));
        }
        bail!("Spotify token refresh failed ({status}): {body}");
    }

    let token: TokenResponse =
        serde_json::from_str(&body).context("failed to decode refresh response")?;
    Ok(merge_refresh_response(existing, token, unix_now()))
}

async fn refresh_access_token_locked(
    config: &Config,
    http: &Client,
    cached: &mut Option<StoredToken>,
    token: &StoredToken,
) -> SpotifyResult<StoredToken> {
    match refresh_token(config, http, token).await {
        Ok(token) => {
            save_token_unlocked_bounded(&token)?;
            *cached = Some(token.clone());
            Ok(token)
        }
        Err(err)
            if matches!(
                err.downcast_ref::<SpotifyError>(),
                Some(SpotifyError::AuthRevoked)
            ) =>
        {
            if let Some(replacement) = purge_revoked_token_unlocked(cached, token) {
                return Ok(replacement);
            }
            Err(SpotifyError::AuthRevoked)
        }
        Err(err) => Err(SpotifyError::from(err)),
    }
}

fn should_refresh_token(token: &StoredToken) -> bool {
    crate::refresh_planner::should_refresh(
        unix_now() as i64,
        token.expires_at as i64,
        crate::refresh_planner::PROACTIVE_HEADROOM,
    )
}

fn token_changed(left: &StoredToken, right: &StoredToken) -> bool {
    left.access_token != right.access_token
        || left.refresh_token != right.refresh_token
        || left.expires_at != right.expires_at
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

fn token_endpoint() -> String {
    #[cfg(test)]
    {
        if let Some(endpoint) = TEST_TOKEN_ENDPOINT
            .lock()
            .expect("token endpoint lock")
            .clone()
        {
            return endpoint;
        }
    }
    SPOTIFY_TOKEN_ENDPOINT.to_string()
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
    use std::ffi::OsString;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use reqwest::Client;
    use tokio::sync::Mutex;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{
        access_token_cached, acquire_token_store_lock_with_timeout, authorization_url,
        disk_token_cache_status, load_token_from_disk, merge_refresh_response,
        refresh_access_token_cached, save_token_to_disk, token_cache_dir, StoredToken,
        TokenResponse, TEST_TOKEN_ENDPOINT,
    };
    use crate::config::Config;
    use crate::error::SpotifyError;

    static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct TestAuthEnv {
        _temp: tempfile::TempDir,
        old_data_dir: Option<OsString>,
    }

    impl TestAuthEnv {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("tempdir");
            let old_data_dir = std::env::var_os("SPOTUIFY_DATA_DIR");
            std::env::set_var("SPOTUIFY_DATA_DIR", temp.path());
            *TEST_TOKEN_ENDPOINT.lock().expect("endpoint lock") = None;
            Self {
                _temp: temp,
                old_data_dir,
            }
        }
    }

    impl Drop for TestAuthEnv {
        fn drop(&mut self) {
            match &self.old_data_dir {
                Some(value) => std::env::set_var("SPOTUIFY_DATA_DIR", value),
                None => std::env::remove_var("SPOTUIFY_DATA_DIR"),
            }
            *TEST_TOKEN_ENDPOINT.lock().expect("endpoint lock") = None;
        }
    }

    fn with_auth_env<R>(f: impl FnOnce() -> R) -> R {
        let _guard = TEST_ENV_LOCK.lock().expect("auth test env lock");
        let _env = TestAuthEnv::new();
        f()
    }

    fn run_auth_async<F, R>(f: impl FnOnce() -> F) -> R
    where
        F: std::future::Future<Output = R>,
    {
        with_auth_env(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(f())
        })
    }

    fn set_token_endpoint(endpoint: String) {
        *TEST_TOKEN_ENDPOINT.lock().expect("endpoint lock") = Some(endpoint);
    }

    fn http_client() -> Client {
        Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("client")
    }

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

    fn fresh_token(access: &str, refresh: &str) -> StoredToken {
        StoredToken {
            access_token: access.to_string(),
            refresh_token: refresh.to_string(),
            expires_at: super::unix_now() + 3_600,
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
    fn concurrent_disk_token_mirrors_do_not_share_temp_path() {
        with_auth_env(|| {
            let handles = (0..16)
                .map(|idx| {
                    std::thread::spawn(move || {
                        let token =
                            fresh_token(&format!("access-{idx}"), &format!("refresh-{idx}"));
                        save_token_to_disk(&token);
                    })
                })
                .collect::<Vec<_>>();

            for handle in handles {
                handle.join().expect("token mirror writer should not panic");
            }

            let token = load_token_from_disk().expect("one mirrored token should remain");
            assert!(token.access_token.starts_with("access-"));
            let leftovers = std::fs::read_dir(token_cache_dir())
                .expect("token cache dir should exist")
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
                .count();
            assert_eq!(leftovers, 0, "temp token files should be cleaned up");
        });
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

    #[test]
    fn disk_token_cache_status_never_prints_token_material() {
        with_auth_env(|| {
            let token = fresh_token("access-secret-should-not-print", "refresh-secret-hidden");
            save_token_to_disk(&token);

            let status = disk_token_cache_status();

            assert!(status.contains("present"));
            assert!(status.contains("token.json"));
            assert!(!status.contains("access-secret-should-not-print"));
            assert!(!status.contains("refresh-secret-hidden"));
            assert!(!status.contains("Bearer"));
        });
    }

    #[test]
    fn invalid_grant_clears_memory_and_disk_cache() {
        run_auth_async(|| async {
            let server = MockServer::start().await;
            set_token_endpoint(format!("{}/api/token", server.uri()));
            Mock::given(method("POST"))
                .and(path("/api/token"))
                .respond_with(ResponseTemplate::new(400).set_body_string(
                    r#"{"error":"invalid_grant","error_description":"Refresh token revoked"}"#,
                ))
                .expect(1)
                .mount(&server)
                .await;

            let old = existing_token();
            save_token_to_disk(&old);
            let cache = Arc::new(Mutex::new(Some(old)));

            let err = access_token_cached(&config(), &http_client(), &cache)
                .await
                .expect_err("revoked refresh should fail");

            assert!(matches!(err, SpotifyError::AuthRevoked));
            assert!(cache.lock().await.is_none(), "memory cache should clear");
            assert!(
                load_token_from_disk().is_none(),
                "disk cache should be removed"
            );
        });
    }

    #[test]
    fn refresh_success_stores_replacement_refresh_token() {
        run_auth_async(|| async {
            let server = MockServer::start().await;
            set_token_endpoint(format!("{}/api/token", server.uri()));
            Mock::given(method("POST"))
                .and(path("/api/token"))
                .respond_with(ResponseTemplate::new(200).set_body_string(
                    r#"{
                        "access_token":"new-access",
                        "token_type":"Bearer",
                        "expires_in":3600,
                        "refresh_token":"new-refresh",
                        "scope":"playlist-read-private"
                    }"#,
                ))
                .expect(1)
                .mount(&server)
                .await;

            let old = existing_token();
            save_token_to_disk(&old);
            let cache = Arc::new(Mutex::new(Some(old)));

            let access = access_token_cached(&config(), &http_client(), &cache)
                .await
                .expect("refresh should succeed");

            assert_eq!(access, "new-access");
            assert_eq!(
                cache
                    .lock()
                    .await
                    .as_ref()
                    .map(|token| token.refresh_token.as_str()),
                Some("new-refresh")
            );
            assert_eq!(
                load_token_from_disk()
                    .as_ref()
                    .map(|token| token.refresh_token.as_str()),
                Some("new-refresh")
            );
        });
    }

    #[test]
    fn stale_memory_uses_newer_disk_token_without_refreshing_old_token() {
        run_auth_async(|| async {
            set_token_endpoint("http://127.0.0.1:9/api/token".to_string());
            let old = existing_token();
            let newer = fresh_token("newer-access", "newer-refresh");
            save_token_to_disk(&newer);
            let cache = Arc::new(Mutex::new(Some(old)));

            let access = access_token_cached(&config(), &http_client(), &cache)
                .await
                .expect("newer disk token should win");

            assert_eq!(access, "newer-access");
            assert_eq!(
                cache
                    .lock()
                    .await
                    .as_ref()
                    .map(|token| token.refresh_token.as_str()),
                Some("newer-refresh")
            );
        });
    }

    #[test]
    fn forced_refresh_uses_newer_disk_token_without_refreshing_old_token() {
        run_auth_async(|| async {
            set_token_endpoint("http://127.0.0.1:9/api/token".to_string());
            let old = fresh_token("old-access", "old-refresh");
            let newer = fresh_token("newer-access", "newer-refresh");
            save_token_to_disk(&newer);
            let cache = Arc::new(Mutex::new(Some(old)));

            let access = refresh_access_token_cached(&config(), &http_client(), &cache)
                .await
                .expect("newer disk token should satisfy forced refresh");

            assert_eq!(access, "newer-access");
            assert_eq!(
                cache
                    .lock()
                    .await
                    .as_ref()
                    .map(|token| token.refresh_token.as_str()),
                Some("newer-refresh")
            );
        });
    }

    #[test]
    fn token_lock_times_out_instead_of_hanging() {
        with_auth_env(|| {
            let _held =
                acquire_token_store_lock_with_timeout(Duration::from_secs(1)).expect("held lock");
            let started = Instant::now();
            let err = acquire_token_store_lock_with_timeout(Duration::from_millis(80))
                .expect_err("second lock should time out");

            assert!(started.elapsed() < Duration::from_secs(1));
            assert!(
                err.to_string()
                    .contains("timed out waiting for Spotify token lock"),
                "{err:#}"
            );
        });
    }
}
