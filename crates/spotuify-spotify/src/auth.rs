use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::net::IpAddr;
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
use tokio::sync::Mutex;
use tokio::{io::AsyncReadExt, io::AsyncWriteExt, net::TcpListener, sync::watch};

use spotuify_core::ProviderId;

use crate::client::user_agent_string;
use crate::config::Config;
use crate::error::{SpotifyError, SpotifyResult};
use crate::first_party::{classify_credential, FirstPartyCredentials, StoredCredential};
use url::form_urlencoded;

const TOKEN_LOCK_TIMEOUT: Duration = Duration::from_secs(15);
const TOKEN_LOCK_POLL: Duration = Duration::from_millis(50);
const SPOTIFY_TOKEN_ENDPOINT: &str = "https://accounts.spotify.com/api/token";

#[cfg(test)]
static TEST_TOKEN_ENDPOINT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub const SCOPES: &[&str] = &[
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
    // Required for `PUT /playlists/{id}/images` (custom cover art).
    // Adding this scope marks existing tokens as "needs reauth" via
    // `missing_required_scopes`, which surfaces ScopeReauthRequired
    // to the TUI/CLI on next daemon start.
    "ugc-image-upload",
    // Embedded librespot playback uses the Web Playback SDK
    // streaming scope + app-remote-control to drive transport.
    "streaming",
    "app-remote-control",
];

/// Scopes accepted by Spotify's keymaster client. Keep this separate
/// from dev-app-only capabilities such as custom playlist cover upload.
pub const FIRST_PARTY_SCOPES: &[&str] = &[
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
    "streaming",
    "app-remote-control",
];

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct StoredToken {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    pub scope: String,
    pub token_type: String,
}

/// Secret-free credential metadata for daemon-owned auth status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialInventory {
    pub dev_app: Option<DevAppCredentialMetadata>,
    pub first_party: Option<FirstPartyCredentialMetadata>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevAppCredentialMetadata {
    pub expires_at: u64,
    pub scopes: Vec<String>,
    pub missing_scopes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirstPartyCredentialMetadata {
    pub scopes: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CredentialPurgeResult {
    pub removed_dev_app: bool,
    pub removed_first_party: bool,
    pub removed_librespot: bool,
}

// Manual impl so a stray `{:?}` in logs or error chains can never leak
// the live access/refresh tokens.
impl std::fmt::Debug for StoredToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredToken")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .field("token_type", &self.token_type)
            .finish()
    }
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
    token.is_some_and(|t| !missing_required_scopes(t).is_empty())
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
    let (_cancel_tx, cancel_rx) = watch::channel(false);
    let token = authorize_token(config, SCOPES, cancel_rx, &mut progress).await?;
    save_dev_app_token(&token)?;
    progress(LoginProgress::Saved);
    Ok(())
}

/// Run the PKCE browser flow without deciding how the resulting refresh token
/// is persisted. Daemon auth sessions use this for both dev-app and keymaster
/// auth, while [`login`] remains the compatibility wrapper for older callers.
pub async fn authorize_token(
    config: &Config,
    scopes: &[&str],
    mut cancel: watch::Receiver<bool>,
    mut progress: impl FnMut(LoginProgress) + Send,
) -> SpotifyResult<StoredToken> {
    let verifier = random_string(96);
    let challenge = pkce_challenge(&verifier);
    let state = random_string(32);
    let auth_url = authorization_url_with_scopes(config, &challenge, &state, scopes)?;
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
    let code = wait_for_code(listener, &state, &mut cancel)
        .await
        .context("failed while waiting for OAuth redirect")?;
    let exchange = tokio::time::timeout(
        Duration::from_secs(30),
        exchange_code(config, &code, &verifier),
    );
    let token = tokio::select! {
        _ = wait_for_cancel(&mut cancel) => {
            return Err(SpotifyError::Client { message: "OAuth flow cancelled".to_string() });
        }
        result = exchange => result.context("Spotify token exchange timed out")??,
    };
    Ok(token)
}

/// Persist a dev-app PKCE token under the provider-scoped auth directory.
pub fn save_dev_app_token(token: &StoredToken) -> SpotifyResult<()> {
    Ok(save_token_bounded(token)?)
}

pub fn save_dev_app_token_for(provider: &str, token: &StoredToken) -> SpotifyResult<()> {
    if provider == "spotify" {
        return save_dev_app_token(token);
    }
    let paths = ProviderCredentialPaths::new(provider)?;
    let _lock = acquire_provider_token_store_lock(&paths)?;
    save_token_to_path(&paths.token, token)?;
    Ok(())
}

pub fn logout() -> SpotifyResult<()> {
    Ok(delete_token_bounded()?)
}

fn delete_token(verbose: bool) -> AnyResult<()> {
    let removed = delete_token_from_disk()?;
    if verbose {
        if removed {
            println!("Removed Spotify token from auth file.");
        } else {
            println!("No Spotify token was stored.");
        }
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
    access_token_cached_for("spotify", config, http, cache).await
}

pub async fn access_token_cached_for(
    provider: &str,
    config: &Config,
    http: &Client,
    cache: &Arc<Mutex<Option<StoredToken>>>,
) -> SpotifyResult<String> {
    // Single-flight token acquisition keeps cold concurrent daemon requests
    // from racing the shared auth file.
    let mut cached = cache.lock().await;
    let token = match cached.clone() {
        Some(token) => token,
        None => load_token_for_access_blocking_for(provider)
            .await?
            .ok_or(SpotifyError::AuthRequired)?,
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
    let _lock = acquire_token_store_lock_blocking_for(provider).await?;
    // Re-read after taking the writer lock. Logout removes the on-disk
    // credential while holding this same lock; falling back to the stale
    // in-memory token here would let a refresh resurrect it after logout.
    let token = load_token_for_access_blocking_for(provider)
        .await?
        .ok_or(SpotifyError::AuthRequired)?;
    if !should_refresh_token(&token) {
        *cached = Some(token.clone());
        return Ok(token.access_token);
    }

    refresh_access_token_locked(provider, config, http, &mut cached, &token)
        .await
        .map(|token| token.access_token)
}

pub async fn refresh_access_token_cached(
    config: &Config,
    http: &Client,
    cache: &Arc<Mutex<Option<StoredToken>>>,
) -> SpotifyResult<String> {
    refresh_access_token_cached_for("spotify", config, http, cache).await
}

pub async fn refresh_access_token_cached_for(
    provider: &str,
    config: &Config,
    http: &Client,
    cache: &Arc<Mutex<Option<StoredToken>>>,
) -> SpotifyResult<String> {
    let mut cached = cache.lock().await;
    tracing::info!("refreshing Spotify access token after 401");
    let _lock = acquire_token_store_lock_blocking_for(provider).await?;
    let token = load_token_for_access_blocking_for(provider)
        .await?
        .ok_or(SpotifyError::AuthRequired)?;
    if cached
        .as_ref()
        .is_some_and(|old| token_changed(old, &token))
        && !should_refresh_token(&token)
    {
        *cached = Some(token.clone());
        return Ok(token.access_token);
    }

    refresh_access_token_locked(provider, config, http, &mut cached, &token)
        .await
        .map(|token| token.access_token)
}

/// Snapshot the stored Spotify token so callers (e.g. the daemon's
/// startup check) can inspect its scopes without going through the
/// refresh path. Returns `Ok(None)` when the user isn't logged in yet.
pub fn stored_token_snapshot() -> SpotifyResult<Option<StoredToken>> {
    Ok(load_token_bounded()?)
}

// ---------------------------------------------------------------------
// First-party (keymaster) credential persistence.
//
// Mirrors the StoredToken machinery but stores a `FirstPartyCredentials`
// blob in a distinct file. The Web API bearer is never persisted here —
// only the long-lived librespot-oauth refresh token. The bearer is
// minted live (login5).
// ---------------------------------------------------------------------

fn first_party_cache_file() -> PathBuf {
    token_cache_dir().join("first-party.json")
}

fn legacy_first_party_cache_file() -> PathBuf {
    legacy_token_cache_dir().join("first-party.json")
}

fn previous_first_party_cache_file() -> PathBuf {
    previous_token_cache_dir().join("first-party.json")
}

fn read_first_party_file(path: &std::path::Path) -> AnyResult<Option<FirstPartyCredentials>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| {
                format!("failed to read first-party credentials {}", path.display())
            })
        }
    };
    let creds = serde_json::from_str::<FirstPartyCredentials>(&raw).with_context(|| {
        format!(
            "stored first-party credentials at {} are invalid JSON",
            path.display()
        )
    })?;
    if creds.is_first_party() && !creds.refresh_token.is_empty() {
        Ok(Some(creds))
    } else {
        Ok(None)
    }
}

/// True when the ONLY stored Web API credential is a first-party refresh
/// token: no dev-app OAuth token on disk (current or legacy path) and a
/// valid first-party credential file present. `Config::is_first_party()`
/// falls back to this when `SPOTUIFY_USE_FIRST_PARTY` is unset, so a
/// daemon restarted without the env var in its environment cannot pick
/// the dev-app mode that has zero credentials (where every request fails
/// "not logged in" while `spotuify auth bearer` still works — the trap
/// hit on 2026-07-05). Disk-only on purpose: never probes the keychain,
/// which can prompt (see the dev-build keychain-storm incident).
pub fn stored_first_party_only() -> bool {
    stored_first_party_only_for("spotify")
}

pub fn stored_first_party_only_for(provider: &str) -> bool {
    if provider != "spotify" {
        let Ok(paths) = ProviderCredentialPaths::new(provider) else {
            return false;
        };
        let _lock = match acquire_provider_token_store_lock(&paths) {
            Ok(lock) => lock,
            Err(error) => {
                tracing::warn!(
                    provider,
                    %error,
                    "failed to acquire provider token lock; assuming dev-app mode"
                );
                return false;
            }
        };
        return read_token_file(&paths.token).ok().flatten().is_none()
            && read_first_party_file(&paths.first_party)
                .ok()
                .flatten()
                .is_some();
    }
    if load_token_from_disk().is_some() {
        return false;
    }
    load_first_party_from_disk().ok().flatten().is_some()
}

fn load_first_party_from_disk() -> AnyResult<Option<FirstPartyCredentials>> {
    ensure_instance_scoped_auth_dir()?;
    let path = first_party_cache_file();
    validate_config_auth_path(&path)?;
    if let Some(creds) = read_first_party_file(&path)? {
        return Ok(Some(creds));
    }

    for (migration_path, legacy) in [
        (previous_first_party_cache_file(), false),
        (legacy_first_party_cache_file(), true),
    ] {
        if migration_path == path {
            continue;
        }
        if legacy {
            validate_legacy_auth_path(&migration_path)?;
        } else {
            validate_config_auth_path(&migration_path)?;
        }
        match read_first_party_file(&migration_path) {
            Ok(Some(creds)) => {
                save_first_party_to_disk(&creds)?;
                return Ok(Some(creds));
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(
                    path = %migration_path.display(),
                    error = %err,
                    "older first-party credential file is unreadable; ignoring migration source"
                );
            }
        }
    }
    Ok(None)
}

fn save_first_party_to_disk(creds: &FirstPartyCredentials) -> AnyResult<()> {
    ensure_instance_scoped_auth_dir()?;
    let path = first_party_cache_file();
    validate_config_auth_path(&path)?;
    let Some(parent) = path.parent() else {
        bail!("first-party credential path has no parent");
    };
    spotuify_protocol::paths::ensure_private_dir(parent).with_context(|| {
        format!(
            "failed to create first-party credential dir {}",
            parent.display()
        )
    })?;
    let raw = creds
        .to_json()
        .context("failed to encode first-party credentials for disk")?;
    atomic_write_mode_0600(&path, raw.as_bytes())
        .with_context(|| format!("failed to write first-party credentials {}", path.display()))?;
    Ok(())
}

fn remove_file_if_exists(path: PathBuf, legacy: bool) -> AnyResult<bool> {
    if legacy {
        validate_legacy_auth_path(&path)?;
    } else {
        validate_config_auth_path(&path)?;
    }
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => {
            Err(err).with_context(|| format!("failed to remove auth file {}", path.display()))
        }
    }
}

fn delete_first_party_from_disk() -> AnyResult<bool> {
    let current = first_party_cache_file();
    let mut removed = remove_file_if_exists(current.clone(), false)?;
    for (older, legacy) in [
        (previous_first_party_cache_file(), false),
        (legacy_first_party_cache_file(), true),
    ] {
        if older != current {
            removed |= remove_file_if_exists(older, legacy)?;
        }
    }
    Ok(removed)
}

fn save_first_party(creds: &FirstPartyCredentials) -> AnyResult<()> {
    save_first_party_to_disk(creds)
}

/// Persist first-party credentials to the auth file, serialized through
/// the shared token-store lock so a concurrent login can't interleave
/// with a read.
pub fn save_first_party_credentials(creds: &FirstPartyCredentials) -> SpotifyResult<()> {
    ensure_instance_scoped_auth_dir()?;
    let _lock = acquire_token_store_lock_bounded()?;
    Ok(save_first_party(creds)?)
}

pub fn save_first_party_credentials_for(
    provider: &str,
    creds: &FirstPartyCredentials,
) -> SpotifyResult<()> {
    if provider == "spotify" {
        return save_first_party_credentials(creds);
    }
    let paths = ProviderCredentialPaths::new(provider)?;
    let _lock = acquire_provider_token_store_lock(&paths)?;
    save_first_party_to_path(&paths.first_party, creds)?;
    Ok(())
}

/// Persist a rotated first-party refresh token only if the credential which
/// produced it is still current. This prevents an in-flight refresh from
/// recreating credentials after a concurrent daemon-owned logout.
pub fn save_rotated_first_party_credentials(
    expected_refresh_token: &str,
    rotated: &FirstPartyCredentials,
) -> SpotifyResult<bool> {
    ensure_instance_scoped_auth_dir()?;
    let _lock = acquire_token_store_lock_bounded()?;
    let Some(current) = load_first_party_from_disk()? else {
        return Ok(false);
    };
    if current.refresh_token != expected_refresh_token {
        return Ok(false);
    }
    save_first_party(rotated)?;
    Ok(true)
}

pub fn save_rotated_first_party_credentials_for(
    provider: &str,
    expected_refresh_token: &str,
    rotated: &FirstPartyCredentials,
) -> SpotifyResult<bool> {
    if provider == "spotify" {
        return save_rotated_first_party_credentials(expected_refresh_token, rotated);
    }
    let paths = ProviderCredentialPaths::new(provider)?;
    let _lock = acquire_provider_token_store_lock(&paths)?;
    let Some(current) = read_first_party_file(&paths.first_party)? else {
        return Ok(false);
    };
    if current.refresh_token != expected_refresh_token {
        return Ok(false);
    }
    save_first_party_to_path(&paths.first_party, rotated)?;
    Ok(true)
}

/// Read both credential kinds under the same store lock. Only metadata is
/// returned; access and refresh tokens cannot cross the daemon wire through
/// this API.
pub fn credential_inventory() -> SpotifyResult<CredentialInventory> {
    ensure_instance_scoped_auth_dir()?;
    let _lock = acquire_token_store_lock_bounded()?;
    let dev_app = load_token_from_store()?.map(|token| DevAppCredentialMetadata {
        expires_at: token.expires_at,
        scopes: token
            .scope
            .split_whitespace()
            .map(ToString::to_string)
            .collect(),
        missing_scopes: missing_required_scopes(&token)
            .into_iter()
            .map(ToString::to_string)
            .collect(),
    });
    let first_party =
        load_first_party_from_disk()?.map(|credentials| FirstPartyCredentialMetadata {
            scopes: credentials.scopes,
        });
    Ok(CredentialInventory {
        dev_app,
        first_party,
    })
}

pub fn credential_inventory_for(provider: &str) -> SpotifyResult<CredentialInventory> {
    if provider == "spotify" {
        return credential_inventory();
    }
    let paths = ProviderCredentialPaths::new(provider)?;
    let _lock = acquire_provider_token_store_lock(&paths)?;
    inventory_from_paths(&paths)
}

/// Atomically remove every credential source the Spotify adapter can use and
/// verify that none remain before releasing the shared writer lock.
pub fn purge_all_credentials() -> SpotifyResult<CredentialPurgeResult> {
    ensure_instance_scoped_auth_dir()?;
    let _lock = acquire_token_store_lock_bounded()?;
    let result = CredentialPurgeResult {
        removed_dev_app: delete_token_from_disk()?,
        removed_first_party: delete_first_party_from_disk()?,
        removed_librespot: remove_librespot_credentials()?,
    };
    verify_credentials_absent()?;
    Ok(result)
}

pub fn purge_all_credentials_for(provider: &str) -> SpotifyResult<CredentialPurgeResult> {
    if provider == "spotify" {
        return purge_all_credentials();
    }
    let paths = ProviderCredentialPaths::new(provider)?;
    let _lock = acquire_provider_token_store_lock(&paths)?;
    let mut removed_dev_app = remove_provider_file(&paths.token)?;
    let mut removed_first_party = remove_provider_file(&paths.first_party)?;
    let removed_librespot = remove_librespot_credentials_at(&paths.librespot)?;
    for legacy_dir in &paths.legacy_dirs {
        let token = legacy_dir.join("token.json");
        let first_party = legacy_dir.join("first-party.json");
        removed_dev_app |= remove_provider_file(&token)?;
        removed_first_party |= remove_provider_file(&first_party)?;
    }
    verify_provider_credentials_absent(&paths)?;
    Ok(CredentialPurgeResult {
        removed_dev_app,
        removed_first_party,
        removed_librespot,
    })
}

/// Load first-party credentials from the auth file. Returns `Ok(None)`
/// when no first-party login has happened.
pub fn load_first_party_credentials() -> SpotifyResult<Option<FirstPartyCredentials>> {
    Ok(load_first_party_from_disk()?)
}

pub fn load_first_party_credentials_for(
    provider: &str,
) -> SpotifyResult<Option<FirstPartyCredentials>> {
    if provider == "spotify" {
        return load_first_party_credentials();
    }
    let paths = ProviderCredentialPaths::new(provider)?;
    let _lock = acquire_provider_token_store_lock(&paths)?;
    Ok(read_first_party_file(&paths.first_party)?)
}

/// Remove first-party credentials from disk.
pub fn delete_first_party_credentials() -> SpotifyResult<()> {
    ensure_instance_scoped_auth_dir()?;
    let _lock = acquire_token_store_lock_bounded()?;
    delete_first_party_from_disk()?;
    Ok(())
}

fn remove_librespot_credentials() -> AnyResult<bool> {
    let path = spotuify_protocol::paths::cache_dir()
        .join("librespot")
        .join("creds");
    remove_librespot_credentials_at(&path)
}

fn remove_librespot_credentials_at(path: &std::path::Path) -> AnyResult<bool> {
    validate_auth_path(path, &spotuify_protocol::paths::cache_dir())?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!(
                "refusing to remove symlinked librespot credential path {}",
                path.display()
            )
        }
        Ok(metadata) if metadata.is_dir() => {
            std::fs::remove_dir_all(path).with_context(|| {
                format!("failed to remove librespot credentials {}", path.display())
            })?;
            Ok(true)
        }
        Ok(_) => bail!(
            "librespot credential path is not a directory: {}",
            path.display()
        ),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err)
            .with_context(|| format!("failed to inspect librespot credentials {}", path.display())),
    }
}

#[derive(Debug)]
struct ProviderCredentialPaths {
    token: PathBuf,
    first_party: PathBuf,
    lock: PathBuf,
    librespot: PathBuf,
    legacy_dirs: Vec<PathBuf>,
}

impl ProviderCredentialPaths {
    fn new(provider: &str) -> SpotifyResult<Self> {
        let provider = ProviderId::new(provider).map_err(|error| {
            SpotifyError::from(anyhow!("invalid auth provider id `{provider}`: {error}"))
        })?;
        ensure_instance_scoped_auth_dir()?;
        let current = spotuify_protocol::paths::config_dir()
            .join("auth")
            .join(provider.as_str());
        validate_config_auth_path(&current)?;
        let legacy = spotuify_protocol::paths::data_dir()
            .join("auth")
            .join(provider.as_str());
        validate_legacy_auth_path(&legacy)?;
        let librespot = spotuify_protocol::paths::cache_dir()
            .join("librespot")
            .join(provider.as_str())
            .join("creds");
        validate_auth_path(&librespot, &spotuify_protocol::paths::cache_dir())?;
        Ok(Self {
            token: current.join("token.json"),
            first_party: current.join("first-party.json"),
            lock: current.join("token.lock"),
            librespot,
            legacy_dirs: vec![legacy],
        })
    }
}

fn acquire_provider_token_store_lock(paths: &ProviderCredentialPaths) -> AnyResult<TokenStoreLock> {
    if let Some(parent) = paths.lock.parent() {
        spotuify_protocol::paths::ensure_private_dir(parent)
            .with_context(|| format!("failed to create provider auth dir {}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&paths.lock)
        .with_context(|| {
            format!(
                "failed to open provider token lock {}",
                paths.lock.display()
            )
        })?;
    let started = Instant::now();
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(TokenStoreLock { file }),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if started.elapsed() >= TOKEN_LOCK_TIMEOUT {
                    bail!(
                        "timed out waiting for provider token lock at {}",
                        paths.lock.display()
                    );
                }
                let remaining = TOKEN_LOCK_TIMEOUT.saturating_sub(started.elapsed());
                std::thread::sleep(std::cmp::min(TOKEN_LOCK_POLL, remaining));
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to lock provider token store {}",
                        paths.lock.display()
                    )
                })
            }
        }
    }
}

fn save_token_to_path(path: &std::path::Path, token: &StoredToken) -> AnyResult<()> {
    validate_config_auth_path(path)?;
    let Some(parent) = path.parent() else {
        bail!("provider token path has no parent");
    };
    spotuify_protocol::paths::ensure_private_dir(parent)?;
    let raw = serde_json::to_vec(token).context("failed to encode provider token")?;
    atomic_write_mode_0600(path, &raw)
        .with_context(|| format!("failed to write provider token {}", path.display()))
}

fn save_first_party_to_path(
    path: &std::path::Path,
    credentials: &FirstPartyCredentials,
) -> AnyResult<()> {
    validate_config_auth_path(path)?;
    let Some(parent) = path.parent() else {
        bail!("provider first-party credential path has no parent");
    };
    spotuify_protocol::paths::ensure_private_dir(parent)?;
    let raw = credentials
        .to_json()
        .context("failed to encode provider first-party credentials")?;
    atomic_write_mode_0600(path, raw.as_bytes()).with_context(|| {
        format!(
            "failed to write provider first-party credentials {}",
            path.display()
        )
    })
}

fn inventory_from_paths(paths: &ProviderCredentialPaths) -> SpotifyResult<CredentialInventory> {
    let dev_app = read_token_file(&paths.token)?.map(|token| DevAppCredentialMetadata {
        expires_at: token.expires_at,
        scopes: token
            .scope
            .split_whitespace()
            .map(ToString::to_string)
            .collect(),
        missing_scopes: missing_required_scopes(&token)
            .into_iter()
            .map(ToString::to_string)
            .collect(),
    });
    let first_party = read_first_party_file(&paths.first_party)?.map(|credentials| {
        FirstPartyCredentialMetadata {
            scopes: credentials.scopes,
        }
    });
    Ok(CredentialInventory {
        dev_app,
        first_party,
    })
}

fn remove_provider_file(path: &std::path::Path) -> AnyResult<bool> {
    if path.starts_with(spotuify_protocol::paths::data_dir()) {
        validate_legacy_auth_path(path)?;
    } else {
        validate_config_auth_path(path)?;
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error)
            .with_context(|| format!("failed to remove provider auth file {}", path.display())),
    }
}

fn verify_provider_credentials_absent(paths: &ProviderCredentialPaths) -> AnyResult<()> {
    let mut files = vec![paths.token.clone(), paths.first_party.clone()];
    for directory in &paths.legacy_dirs {
        files.push(directory.join("token.json"));
        files.push(directory.join("first-party.json"));
    }
    for path in files {
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Ok(_) => bail!(
                "credential deletion verification failed for {}",
                path.display()
            ),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to verify provider auth file {}", path.display())
                })
            }
        }
    }
    match std::fs::symlink_metadata(&paths.librespot) {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Ok(_) => bail!(
            "credential deletion verification failed for {}",
            paths.librespot.display()
        ),
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to verify provider librespot credentials {}",
                paths.librespot.display()
            )
        }),
    }
}

fn verify_credentials_absent() -> AnyResult<()> {
    let credential_files = [
        token_cache_file(),
        previous_token_cache_file(),
        legacy_token_cache_file(),
        first_party_cache_file(),
        previous_first_party_cache_file(),
        legacy_first_party_cache_file(),
    ];
    for path in credential_files {
        match std::fs::symlink_metadata(&path) {
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Ok(_) => bail!(
                "credential deletion verification failed for {}",
                path.display()
            ),
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "failed to verify credential deletion for {}",
                        path.display()
                    )
                })
            }
        }
    }
    let librespot = spotuify_protocol::paths::cache_dir()
        .join("librespot")
        .join("creds");
    match std::fs::symlink_metadata(&librespot) {
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Ok(_) => bail!(
            "credential deletion verification failed for {}",
            librespot.display()
        ),
        Err(err) => Err(err).with_context(|| {
            format!(
                "failed to verify credential deletion for {}",
                librespot.display()
            )
        }),
    }
}

/// Human-readable login status for `spotuify doctor`, reporting the
/// RESOLVED auth mode rather than whichever credential happens to be
/// preferred on disk. Mirrors [`Config::is_first_party`]: the
/// `SPOTUIFY_USE_FIRST_PARTY` override wins, otherwise the mode follows
/// the stored credentials (dev-app token wins over a lone first-party
/// refresh token). `Ok(None)` means not logged in (no credential stored).
///
/// - dev-app token only (or env-forced dev-app): the dev-app token
///   status message.
/// - both credentials, resolved to dev-app: hybrid (dev-app reads,
///   first-party writes).
/// - resolved to first-party (first-party-only on disk, or env-forced
///   first-party): flagged as rate-limited with the migration command,
///   because Spotify polices sustained Web API traffic on the keymaster
///   token hard.
pub fn credential_status() -> SpotifyResult<Option<String>> {
    const FIRST_PARTY_RATE_LIMITED: &str =
        "present (first-party login — rate-limited; run `spotuify login --dev-app` to switch)";

    let token = load_token_bounded()?;
    let has_first_party = load_first_party_credentials()?.is_some();

    if token.is_none() && !has_first_party {
        return Ok(None);
    }

    // Resolve the effective mode exactly as `Config::is_first_party` does.
    let resolved_first_party = match Config::first_party_env_override() {
        Some(explicit) => explicit,
        None => token.is_none() && has_first_party,
    };

    if resolved_first_party {
        return Ok(Some(FIRST_PARTY_RATE_LIMITED.to_string()));
    }

    match token {
        Some(token) => {
            let base = token_status_message(&token, unix_now());
            if has_first_party {
                Ok(Some(format!(
                    "{base} — hybrid (dev-app reads, first-party writes)"
                )))
            } else {
                Ok(Some(base))
            }
        }
        // Env-forced dev-app with only a first-party credential on disk:
        // there is no dev-app token to describe, so surface the
        // first-party credential (and its rate-limit caveat) that IS
        // present. Rare edge; keeps the label honest.
        None => Ok(Some(FIRST_PARTY_RATE_LIMITED.to_string())),
    }
}

/// Provider-scoped variant used by diagnostics for custom Spotify adapter IDs.
pub fn credential_status_for(provider: &str) -> SpotifyResult<Option<String>> {
    if provider == "spotify" {
        return credential_status();
    }
    const FIRST_PARTY_RATE_LIMITED: &str =
        "present (first-party login — rate-limited; run `spotuify login --dev-app` to switch)";
    let inventory = credential_inventory_for(provider)?;
    if inventory.dev_app.is_none() && inventory.first_party.is_none() {
        return Ok(None);
    }
    let resolved_first_party = match Config::first_party_env_override() {
        Some(explicit) => explicit,
        None => inventory.dev_app.is_none() && inventory.first_party.is_some(),
    };
    if resolved_first_party || inventory.dev_app.is_none() {
        return Ok(Some(FIRST_PARTY_RATE_LIMITED.to_string()));
    }
    let dev_app = inventory.dev_app.expect("dev-app presence checked");
    let mut status = if dev_app.expires_at > unix_now() {
        format!(
            "present, access token expires in {}m",
            (dev_app.expires_at - unix_now()) / 60
        )
    } else {
        "present, access token expired; refresh token available".to_string()
    };
    if !dev_app.missing_scopes.is_empty() {
        status.push_str("; missing scopes: ");
        status.push_str(&dev_app.missing_scopes.join(", "));
        status.push_str("; run `spotuify login`");
    }
    if inventory.first_party.is_some() {
        status.push_str(" — hybrid (dev-app reads, first-party writes)");
    }
    Ok(Some(status))
}

/// Classify whatever credential is on this machine. Prefers first-party;
/// falls back to a legacy dev-app token (which the daemon surfaces as
/// "re-login required" so the user switches to the first-party flow).
/// `None` means no usable credential is stored at all.
pub fn stored_credential_snapshot() -> SpotifyResult<Option<StoredCredential>> {
    if let Some(creds) = load_first_party_credentials()? {
        return Ok(Some(StoredCredential::FirstParty(creds)));
    }
    // Fall back to the raw legacy blob so we can distinguish "legacy
    // dev-app token present" (needs re-login) from "nothing stored".
    if let Some(token) = load_token_bounded()? {
        let raw = serde_json::to_string(&token).unwrap_or_default();
        if let Some(StoredCredential::LegacyDevApp(token)) = classify_credential(&raw) {
            return Ok(Some(StoredCredential::LegacyDevApp(token)));
        }
    }
    Ok(None)
}

/// Disk-only credential snapshot for daemon recovery probes. This never
/// touches anything outside the auth directory, so it is safe to call while an
/// auth-required latch is suppressing interactive credential prompts.
pub fn stored_credential_disk_snapshot() -> Option<StoredCredential> {
    if let Ok(Some(creds)) = load_first_party_from_disk() {
        return Some(StoredCredential::FirstParty(creds));
    }
    let token = load_token_from_disk()?;
    let raw = serde_json::to_string(&token).ok()?;
    classify_credential(&raw)
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
        "{state}; OAuth token file at {} with mode 0600 on Unix",
        path.display()
    )
}

pub fn disk_token_cache_status_for(provider: &str) -> String {
    if provider == "spotify" {
        return disk_token_cache_status();
    }
    let Ok(paths) = ProviderCredentialPaths::new(provider) else {
        return format!("unresolved OAuth token path for provider `{provider}`");
    };
    let state = match std::fs::metadata(&paths.token) {
        Ok(meta) if meta.is_file() => "present",
        Ok(_) => "non-file",
        Err(err) if err.kind() == ErrorKind::NotFound => "absent",
        Err(_) => "unreadable",
    };
    format!(
        "{state}; OAuth token file at {} with mode 0600 on Unix",
        paths.token.display()
    )
}

/// File-backed credential store.
///
/// The auth files live under the app config directory:
/// `<config_dir>/auth/spotify/token.json` and
/// `<config_dir>/auth/spotify/first-party.json`.
/// On Unix the directory is mode 0700 and files are written atomically
/// with mode 0600. Older `<config_dir>/auth/*.json` and
/// `<data_dir>/auth/*.json` files are migration sources, copied
/// atomically into the provider-scoped directory on first read.
fn token_cache_dir() -> PathBuf {
    spotuify_protocol::paths::config_dir()
        .join("auth")
        .join("spotify")
}

fn ensure_instance_scoped_auth_dir() -> AnyResult<()> {
    let instance = spotuify_protocol::paths::app_instance_name();
    let config_dir = spotuify_protocol::paths::config_dir();
    let data_dir = spotuify_protocol::paths::data_dir();
    let explicitly_allowed = std::env::var("SPOTUIFY_ALLOW_PROD_INSTANCE_FROM_TARGET")
        .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    validate_auth_path_for_instance(&config_dir, &config_dir, &instance, explicitly_allowed)?;
    validate_auth_path_for_instance(&data_dir, &data_dir, &instance, explicitly_allowed)
}

fn validate_config_auth_path(path: &std::path::Path) -> AnyResult<()> {
    validate_auth_path(path, &spotuify_protocol::paths::config_dir())
}

fn validate_legacy_auth_path(path: &std::path::Path) -> AnyResult<()> {
    validate_auth_path(path, &spotuify_protocol::paths::data_dir())
}

fn validate_auth_path(path: &std::path::Path, base: &std::path::Path) -> AnyResult<()> {
    let instance = spotuify_protocol::paths::app_instance_name();
    let explicitly_allowed = std::env::var("SPOTUIFY_ALLOW_PROD_INSTANCE_FROM_TARGET")
        .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    validate_auth_path_for_instance(path, base, &instance, explicitly_allowed)
}

fn validate_auth_path_for_instance(
    path: &std::path::Path,
    base: &std::path::Path,
    instance: &str,
    explicitly_allowed: bool,
) -> AnyResult<()> {
    let resolved_base = resolve_path_with_symlinks(base)
        .with_context(|| format!("failed to resolve auth base {}", base.display()))?;
    let resolved_path = resolve_path_with_symlinks(path)
        .with_context(|| format!("failed to resolve auth path {}", path.display()))?;
    if !resolved_path.starts_with(&resolved_base) {
        bail!(
            "auth path {} resolves outside its instance directory {}",
            path.display(),
            base.display()
        );
    }
    if instance != "spotuify"
        && resolved_base
            .file_name()
            .is_some_and(|name| name == std::ffi::OsStr::new("spotuify"))
        && !explicitly_allowed
    {
        bail!(
            "instance `{instance}` refuses to use production auth directory {}; set SPOTUIFY_ALLOW_PROD_INSTANCE_FROM_TARGET=1 only for an intentional override",
            resolved_base.display()
        );
    }
    Ok(())
}

fn resolve_path_with_symlinks(path: &std::path::Path) -> AnyResult<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory")?
            .join(path)
    };
    let normalized = normalize_path(&absolute);
    let mut existing = normalized.as_path();
    let mut missing = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .ok_or_else(|| anyhow!("auth path {} has no existing ancestor", path.display()))?;
        missing.push(name.to_os_string());
        existing = existing
            .parent()
            .ok_or_else(|| anyhow!("auth path {} has no parent", path.display()))?;
    }
    let mut resolved = std::fs::canonicalize(existing)
        .with_context(|| format!("failed to canonicalize {}", existing.display()))?;
    for component in missing.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn normalize_path(path: &std::path::Path) -> PathBuf {
    use std::path::Component;

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if normalized.file_name().is_some() {
                    normalized.pop();
                }
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

fn previous_token_cache_dir() -> PathBuf {
    spotuify_protocol::paths::config_dir().join("auth")
}

fn legacy_token_cache_dir() -> PathBuf {
    spotuify_protocol::paths::data_dir().join("auth")
}

fn token_cache_file() -> PathBuf {
    token_cache_dir().join("token.json")
}

fn legacy_token_cache_file() -> PathBuf {
    legacy_token_cache_dir().join("token.json")
}

fn previous_token_cache_file() -> PathBuf {
    previous_token_cache_dir().join("token.json")
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

async fn acquire_token_store_lock_blocking() -> SpotifyResult<TokenStoreLock> {
    tokio::task::spawn_blocking(acquire_token_store_lock_bounded)
        .await
        .map_err(|err| SpotifyError::from(anyhow!("token lock task failed: {err}")))?
        .map_err(SpotifyError::from)
}

async fn acquire_token_store_lock_blocking_for(provider: &str) -> SpotifyResult<TokenStoreLock> {
    if provider == "spotify" {
        return acquire_token_store_lock_blocking().await;
    }
    let paths = ProviderCredentialPaths::new(provider)?;
    tokio::task::spawn_blocking(move || acquire_provider_token_store_lock(&paths))
        .await
        .map_err(|error| SpotifyError::from(anyhow!("provider token lock task failed: {error}")))?
        .map_err(SpotifyError::from)
}

fn acquire_token_store_lock_with_timeout(timeout: Duration) -> AnyResult<TokenStoreLock> {
    ensure_instance_scoped_auth_dir()?;
    let path = token_lock_file();
    validate_config_auth_path(&path)?;
    if let Some(parent) = path.parent() {
        spotuify_protocol::paths::ensure_private_dir(parent).with_context(|| {
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

fn read_token_file(path: &std::path::Path) -> AnyResult<Option<StoredToken>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to read Spotify token file {}", path.display()))
        }
    };
    serde_json::from_str::<StoredToken>(&raw)
        .map(Some)
        .with_context(|| format!("stored token at {} is invalid JSON", path.display()))
}

fn load_token_from_store() -> AnyResult<Option<StoredToken>> {
    ensure_instance_scoped_auth_dir()?;
    let path = token_cache_file();
    validate_config_auth_path(&path)?;
    if let Some(token) = read_token_file(&path)? {
        return Ok(Some(token));
    }

    for (migration_path, legacy) in [
        (previous_token_cache_file(), false),
        (legacy_token_cache_file(), true),
    ] {
        if migration_path == path {
            continue;
        }
        if legacy {
            validate_legacy_auth_path(&migration_path)?;
        } else {
            validate_config_auth_path(&migration_path)?;
        }
        match read_token_file(&migration_path) {
            Ok(Some(token)) => {
                save_token_to_disk(&token)?;
                return Ok(Some(token));
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(
                    path = %migration_path.display(),
                    error = %err,
                    "older token file is unreadable; ignoring migration source"
                );
            }
        }
    }
    Ok(None)
}

fn load_token_from_disk() -> Option<StoredToken> {
    load_token_from_store().ok().flatten()
}

fn save_token_to_disk(token: &StoredToken) -> AnyResult<()> {
    ensure_instance_scoped_auth_dir()?;
    let path = token_cache_file();
    validate_config_auth_path(&path)?;
    let Some(parent) = path.parent() else {
        bail!("Spotify token path has no parent");
    };
    spotuify_protocol::paths::ensure_private_dir(parent)
        .with_context(|| format!("failed to create Spotify token dir {}", parent.display()))?;
    let raw = match serde_json::to_string(token) {
        Ok(raw) => raw,
        Err(err) => {
            return Err(err).context("failed to encode token for disk");
        }
    };
    atomic_write_mode_0600(&path, raw.as_bytes())
        .with_context(|| format!("failed to write Spotify token file {}", path.display()))?;
    Ok(())
}

fn delete_token_from_disk() -> AnyResult<bool> {
    let current = token_cache_file();
    let mut removed = remove_file_if_exists(current.clone(), false)?;
    for (older, legacy) in [
        (previous_token_cache_file(), false),
        (legacy_token_cache_file(), true),
    ] {
        if older != current {
            removed |= remove_file_if_exists(older, legacy)?;
        }
    }
    Ok(removed)
}

#[cfg(unix)]
fn atomic_write_mode_0600(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let Some(parent) = path.parent() else {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            "auth file path has no parent",
        ));
    };
    spotuify_protocol::paths::ensure_private_dir(parent).map_err(std::io::Error::other)?;
    let file_name = path
        .file_name()
        .map_or_else(|| "token".into(), |name| name.to_string_lossy());
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
    load_token_from_store()
}

fn load_token_bounded() -> AnyResult<Option<StoredToken>> {
    load_token()
}

fn load_token_for_access() -> SpotifyResult<Option<StoredToken>> {
    load_token_bounded().map_err(map_token_load_error)
}

async fn load_token_for_access_blocking() -> SpotifyResult<Option<StoredToken>> {
    tokio::task::spawn_blocking(load_token_for_access)
        .await
        .map_err(|err| SpotifyError::from(anyhow!("token load task failed: {err}")))?
}

async fn load_token_for_access_blocking_for(provider: &str) -> SpotifyResult<Option<StoredToken>> {
    if provider == "spotify" {
        return load_token_for_access_blocking().await;
    }
    let paths = ProviderCredentialPaths::new(provider)?;
    tokio::task::spawn_blocking(move || read_token_file(&paths.token))
        .await
        .map_err(|error| SpotifyError::from(anyhow!("provider token load task failed: {error}")))?
        .map_err(SpotifyError::from)
}

fn map_token_load_error(err: anyhow::Error) -> SpotifyError {
    SpotifyError::from(err)
}

fn save_token(token: &StoredToken) -> AnyResult<()> {
    save_token_to_disk(token)
}

fn save_token_bounded(token: &StoredToken) -> AnyResult<()> {
    ensure_instance_scoped_auth_dir()?;
    let _lock = acquire_token_store_lock_bounded()?;
    save_token_unlocked_bounded(token)
}

fn save_token_unlocked_bounded(token: &StoredToken) -> AnyResult<()> {
    save_token(token)
}

async fn save_token_unlocked_blocking(token: StoredToken) -> SpotifyResult<()> {
    tokio::task::spawn_blocking(move || save_token_unlocked_bounded(&token))
        .await
        .map_err(|err| SpotifyError::from(anyhow!("token save task failed: {err}")))?
        .map_err(SpotifyError::from)
}

fn delete_token_bounded() -> AnyResult<()> {
    ensure_instance_scoped_auth_dir()?;
    let _lock = acquire_token_store_lock_bounded()?;
    delete_token_unlocked_bounded(true)
}

fn delete_token_unlocked_bounded(verbose: bool) -> AnyResult<()> {
    ensure_instance_scoped_auth_dir()?;
    delete_token(verbose)
}

async fn purge_revoked_token_unlocked_blocking(
    cache: &mut Option<StoredToken>,
    failed: &StoredToken,
) -> Option<StoredToken> {
    let failed = failed.clone();
    let outcome = tokio::task::spawn_blocking(move || match load_token_bounded() {
        Ok(Some(current)) if token_changed(&failed, &current) => {
            PurgeRevokedOutcome::Replacement(current)
        }
        Ok(_) | Err(_) => {
            if let Err(err) = delete_token_unlocked_bounded(false) {
                tracing::warn!(
                    error = %err,
                    "failed to clear revoked Spotify token file; re-login will overwrite it"
                );
            }
            PurgeRevokedOutcome::Cleared
        }
    })
    .await
    .unwrap_or(PurgeRevokedOutcome::Cleared);

    match outcome {
        PurgeRevokedOutcome::Replacement(token) => {
            *cache = Some(token.clone());
            tracing::info!(
                "Spotify refresh token was replaced while revoked refresh was in-flight; keeping newer token"
            );
            Some(token)
        }
        PurgeRevokedOutcome::Cleared => {
            *cache = None;
            None
        }
    }
}

enum PurgeRevokedOutcome {
    Replacement(StoredToken),
    Cleared,
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
    provider: &str,
    config: &Config,
    http: &Client,
    cached: &mut Option<StoredToken>,
    token: &StoredToken,
) -> SpotifyResult<StoredToken> {
    match refresh_token(config, http, token).await {
        Ok(token) => {
            save_token_unlocked_blocking_for(provider, token.clone()).await?;
            *cached = Some(token.clone());
            Ok(token)
        }
        Err(err)
            if matches!(
                err.downcast_ref::<SpotifyError>(),
                Some(SpotifyError::AuthRevoked)
            ) =>
        {
            if let Some(replacement) =
                purge_revoked_token_unlocked_blocking_for(provider, cached, token).await
            {
                return Ok(replacement);
            }
            Err(SpotifyError::AuthRevoked)
        }
        Err(err) => Err(SpotifyError::from(err)),
    }
}

async fn save_token_unlocked_blocking_for(provider: &str, token: StoredToken) -> SpotifyResult<()> {
    if provider == "spotify" {
        return save_token_unlocked_blocking(token).await;
    }
    let paths = ProviderCredentialPaths::new(provider)?;
    tokio::task::spawn_blocking(move || save_token_to_path(&paths.token, &token))
        .await
        .map_err(|error| SpotifyError::from(anyhow!("provider token save task failed: {error}")))?
        .map_err(SpotifyError::from)
}

async fn purge_revoked_token_unlocked_blocking_for(
    provider: &str,
    cache: &mut Option<StoredToken>,
    failed: &StoredToken,
) -> Option<StoredToken> {
    if provider == "spotify" {
        return purge_revoked_token_unlocked_blocking(cache, failed).await;
    }
    let paths = ProviderCredentialPaths::new(provider).ok()?;
    let failed = failed.clone();
    let outcome = tokio::task::spawn_blocking(move || match read_token_file(&paths.token) {
        Ok(Some(current)) if token_changed(&failed, &current) => {
            PurgeRevokedOutcome::Replacement(current)
        }
        Ok(_) | Err(_) => {
            let _ = remove_provider_file(&paths.token);
            PurgeRevokedOutcome::Cleared
        }
    })
    .await
    .unwrap_or(PurgeRevokedOutcome::Cleared);
    match outcome {
        PurgeRevokedOutcome::Replacement(token) => {
            *cache = Some(token.clone());
            Some(token)
        }
        PurgeRevokedOutcome::Cleared => {
            *cache = None;
            None
        }
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

#[cfg(test)]
fn authorization_url(config: &Config, challenge: &str, state: &str) -> AnyResult<String> {
    authorization_url_with_scopes(config, challenge, state, SCOPES)
}

fn authorization_url_with_scopes(
    config: &Config,
    challenge: &str,
    state: &str,
    scopes: &[&str],
) -> AnyResult<String> {
    let scope = scopes.join(" ");
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
    if !redirect_host_is_loopback(host) {
        bail!("redirect URI host `{host}` is not loopback; use 127.0.0.1");
    }
    if !host_is_literal_ipv4_loopback(host) {
        // Spotify's Nov 2025 OAuth migration rejects `localhost`/`::1`
        // redirect URIs; only the literal 127.0.0.1 is accepted. We still
        // bind so existing configs keep limping, but the authorize step
        // will likely fail upstream.
        tracing::warn!(
            host,
            "redirect URI host is a loopback alias Spotify rejects; \
             use http://127.0.0.1:<port>/callback"
        );
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow!("redirect URI port missing"))?;
    let listener = std::net::TcpListener::bind((host, port))
        .with_context(|| format!("failed to bind {host}:{port}"))?;
    listener
        .set_nonblocking(true)
        .context("failed to configure async redirect listener")?;
    TcpListener::from_std(listener).context("failed to create async redirect listener")
}

fn redirect_host_is_loopback(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(addr) => addr.is_loopback(),
        Err(_) => false,
    }
}

/// Spotify only accepts the literal IPv4 loopback in redirect URIs
/// since the Nov 2025 OAuth migration.
fn host_is_literal_ipv4_loopback(host: &str) -> bool {
    matches!(host.parse::<IpAddr>(), Ok(IpAddr::V4(addr)) if addr.is_loopback())
}

async fn wait_for_code(
    listener: TcpListener,
    expected_state: &str,
    cancel: &mut watch::Receiver<bool>,
) -> AnyResult<String> {
    let accept = tokio::time::timeout(Duration::from_secs(180), listener.accept());
    let (mut stream, _) = tokio::select! {
        _ = wait_for_cancel(cancel) => bail!("OAuth flow cancelled"),
        accepted = accept => accepted.context("timed out waiting for OAuth redirect")??,
    };
    let mut buffer = [0_u8; 4096];
    let read = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buffer));
    let bytes = tokio::select! {
        _ = wait_for_cancel(cancel) => bail!("OAuth flow cancelled"),
        read = read => read.context("timed out reading OAuth redirect")??,
    };
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
    tokio::time::timeout(
        Duration::from_secs(5),
        stream.write_all(response.as_bytes()),
    )
    .await
    .context("timed out writing OAuth browser response")??;

    if let Some(error) = error {
        bail!("Spotify authorization failed: {error}");
    }
    if state.as_deref() != Some(expected_state) {
        bail!("OAuth state mismatch");
    }
    code.ok_or_else(|| anyhow!("Spotify redirect did not include a code"))
}

async fn wait_for_cancel(cancel: &mut watch::Receiver<bool>) {
    loop {
        if *cancel.borrow() {
            return;
        }
        if cancel.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
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
        old_config_dir: Option<OsString>,
        old_data_dir: Option<OsString>,
        old_cache_dir: Option<OsString>,
    }

    impl TestAuthEnv {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("tempdir");
            let old_config_dir = std::env::var_os("SPOTUIFY_CONFIG_DIR");
            let old_data_dir = std::env::var_os("SPOTUIFY_DATA_DIR");
            let old_cache_dir = std::env::var_os("SPOTUIFY_CACHE_DIR");
            std::env::set_var("SPOTUIFY_CONFIG_DIR", temp.path().join("config"));
            std::env::set_var("SPOTUIFY_DATA_DIR", temp.path());
            std::env::set_var("SPOTUIFY_CACHE_DIR", temp.path().join("cache"));
            *TEST_TOKEN_ENDPOINT.lock().expect("endpoint lock") = None;
            Self {
                _temp: temp,
                old_config_dir,
                old_data_dir,
                old_cache_dir,
            }
        }
    }

    impl Drop for TestAuthEnv {
        fn drop(&mut self) {
            match &self.old_config_dir {
                Some(value) => std::env::set_var("SPOTUIFY_CONFIG_DIR", value),
                None => std::env::remove_var("SPOTUIFY_CONFIG_DIR"),
            }
            match &self.old_data_dir {
                Some(value) => std::env::set_var("SPOTUIFY_DATA_DIR", value),
                None => std::env::remove_var("SPOTUIFY_DATA_DIR"),
            }
            match &self.old_cache_dir {
                Some(value) => std::env::set_var("SPOTUIFY_CACHE_DIR", value),
                None => std::env::remove_var("SPOTUIFY_CACHE_DIR"),
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
    fn dev_instance_rejects_canonical_and_alternate_production_auth_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let production = temp.path().join("spotuify");
        std::fs::create_dir_all(&production).expect("production dir");

        let direct = production.join("auth/spotify/token.json");
        let error =
            super::validate_auth_path_for_instance(&direct, &production, "spotuify-dev", false)
                .expect_err("dev must reject prod auth");
        assert!(error.to_string().contains("refuses"));

        let alternate = temp.path().join("dev/../spotuify");
        super::validate_auth_path_for_instance(
            &alternate.join("auth/token.json"),
            &alternate,
            "spotuify-dev",
            false,
        )
        .expect_err("alternate path must resolve to prod auth");

        super::validate_auth_path_for_instance(&direct, &production, "spotuify", false)
            .expect("production instance may use production auth");
    }

    #[cfg(unix)]
    #[test]
    fn auth_path_validation_rejects_symlinked_roots_and_files() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let production = temp.path().join("spotuify");
        std::fs::create_dir_all(&production).expect("production dir");
        let root_link = temp.path().join("spotuify-dev-link");
        symlink(&production, &root_link).expect("root symlink");
        super::validate_auth_path_for_instance(
            &root_link.join("auth/spotify/token.json"),
            &root_link,
            "spotuify-dev",
            false,
        )
        .expect_err("symlink to production root must be rejected");

        let dev = temp.path().join("spotuify-dev");
        let auth = dev.join("auth/spotify");
        let outside = temp.path().join("outside-token.json");
        std::fs::create_dir_all(&auth).expect("dev auth dir");
        std::fs::write(&outside, "secret").expect("outside file");
        let token_link = auth.join("token.json");
        symlink(&outside, &token_link).expect("token symlink");
        super::validate_auth_path_for_instance(&token_link, &dev, "spotuify-dev", false)
            .expect_err("auth source symlink outside instance must be rejected");
    }

    #[cfg(unix)]
    #[test]
    fn migration_and_logout_refuse_symlinked_legacy_auth_sources() {
        use std::os::unix::fs::symlink;

        with_auth_env(|| {
            let legacy_auth = spotuify_protocol::paths::data_dir().join("auth");
            std::fs::create_dir_all(&legacy_auth).expect("legacy auth dir");
            let outside = tempfile::tempdir().expect("outside tempdir");
            let outside_token = outside.path().join("token.json");
            let outside_first_party = outside.path().join("first-party.json");
            std::fs::write(&outside_token, "secret-token").expect("outside token");
            std::fs::write(&outside_first_party, "secret-refresh-token")
                .expect("outside first-party credentials");
            symlink(&outside_token, legacy_auth.join("token.json")).expect("legacy token symlink");
            symlink(&outside_first_party, legacy_auth.join("first-party.json"))
                .expect("legacy first-party symlink");

            let token_load = super::load_token_from_store()
                .expect_err("migration must reject a legacy token symlink outside the data dir");
            assert!(token_load.to_string().contains("outside"), "{token_load:#}");
            let first_party_load = super::load_first_party_from_disk().expect_err(
                "migration must reject a legacy first-party symlink outside the data dir",
            );
            assert!(
                first_party_load.to_string().contains("outside"),
                "{first_party_load:#}"
            );

            super::delete_token_from_disk()
                .expect_err("logout must not follow the legacy token symlink");
            super::delete_first_party_from_disk()
                .expect_err("logout must not follow the legacy first-party symlink");
            assert_eq!(
                std::fs::read_to_string(&outside_token).expect("outside token survives"),
                "secret-token"
            );
            assert_eq!(
                std::fs::read_to_string(&outside_first_party)
                    .expect("outside first-party credentials survive"),
                "secret-refresh-token"
            );
        });
    }

    fn write_first_party_creds() {
        let dir = token_cache_dir();
        std::fs::create_dir_all(&dir).expect("auth dir");
        std::fs::write(
            dir.join("first-party.json"),
            r#"{"auth_kind":"first-party","refresh_token":"AQfake","scopes":[]}"#,
        )
        .expect("write first-party creds");
    }

    fn test_config() -> Config {
        Config {
            client_id: "fake-client-id".to_string(),
            client_secret: None,
            redirect_uri: "http://127.0.0.1:8888/callback".to_string(),
            config_path: std::path::PathBuf::from("fake-spotuify.toml"),
            player: crate::config::PlayerConfig::default(),
            cache: crate::config::CacheConfig::default(),
            analytics: crate::config::AnalyticsConfig::default(),
            notifications: crate::config::NotificationsConfig::default(),
            discord: crate::config::DiscordConfig::default(),
            viz: crate::config::VizConfig::default(),
        }
    }

    /// Regression for 2026-07-05: a daemon restarted WITHOUT
    /// `SPOTUIFY_USE_FIRST_PARTY` in its environment silently fell back
    /// to dev-app mode with no `token.json` and failed every Web API
    /// call "not logged in". With the env unset, the mode must follow
    /// the credentials that actually exist on disk.
    #[test]
    fn first_party_mode_follows_stored_credentials_when_env_unset() {
        with_auth_env(|| {
            let old = std::env::var_os("SPOTUIFY_USE_FIRST_PARTY");
            std::env::remove_var("SPOTUIFY_USE_FIRST_PARTY");
            let config = test_config();

            // Nothing stored → dev-app default (fresh setup).
            assert!(!super::stored_first_party_only());
            assert!(!config.is_first_party());

            // Only first-party credentials → first-party, or the daemon
            // would run a mode with zero credentials.
            write_first_party_creds();
            assert!(super::stored_first_party_only());
            assert!(config.is_first_party());

            // A dev-app token appears → dev-app wins (product default;
            // the keymaster token cannot absorb heavy Web API polling).
            save_token_to_disk(&existing_token()).expect("save dev-app token");
            assert!(!super::stored_first_party_only());
            assert!(!config.is_first_party());

            // Env set is an explicit override in BOTH directions.
            std::env::set_var("SPOTUIFY_USE_FIRST_PARTY", "1");
            assert!(config.is_first_party());
            std::env::set_var("SPOTUIFY_USE_FIRST_PARTY", "0");
            assert!(!config.is_first_party());

            match old {
                Some(value) => std::env::set_var("SPOTUIFY_USE_FIRST_PARTY", value),
                None => std::env::remove_var("SPOTUIFY_USE_FIRST_PARTY"),
            }
        });
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
    fn concurrent_auth_file_writes_do_not_share_temp_path() {
        with_auth_env(|| {
            let handles = (0..16)
                .map(|idx| {
                    std::thread::spawn(move || {
                        let token =
                            fresh_token(&format!("access-{idx}"), &format!("refresh-{idx}"));
                        save_token_to_disk(&token).expect("save token");
                    })
                })
                .collect::<Vec<_>>();

            for handle in handles {
                handle.join().expect("token file writer should not panic");
            }

            let token = load_token_from_disk().expect("one token should remain");
            assert!(token.access_token.starts_with("access-"));
            let leftovers = std::fs::read_dir(token_cache_dir())
                .expect("auth dir should exist")
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

    /// Run `f` with `SPOTUIFY_USE_FIRST_PARTY` set to `value` (or removed
    /// when `None`), restoring the previous value afterwards. Callers hold
    /// `TEST_ENV_LOCK` via `with_auth_env`, so this env mutation is serial.
    fn with_first_party_env<R>(value: Option<&str>, f: impl FnOnce() -> R) -> R {
        let old = std::env::var_os("SPOTUIFY_USE_FIRST_PARTY");
        match value {
            Some(v) => std::env::set_var("SPOTUIFY_USE_FIRST_PARTY", v),
            None => std::env::remove_var("SPOTUIFY_USE_FIRST_PARTY"),
        }
        let out = f();
        match old {
            Some(v) => std::env::set_var("SPOTUIFY_USE_FIRST_PARTY", v),
            None => std::env::remove_var("SPOTUIFY_USE_FIRST_PARTY"),
        }
        out
    }

    #[test]
    fn credential_status_none_when_logged_out() {
        with_auth_env(|| {
            with_first_party_env(None, || {
                assert_eq!(super::credential_status().expect("status"), None);
            });
        });
    }

    #[test]
    fn credential_status_reports_dev_app_only() {
        with_auth_env(|| {
            with_first_party_env(None, || {
                save_token_to_disk(&existing_token()).expect("save dev-app token");
                let status = super::credential_status().expect("status").expect("some");
                // Exactly the dev-app token message — no hybrid suffix, no
                // first-party rate-limit caveat.
                assert_eq!(
                    status,
                    super::token_status_message(&existing_token(), super::unix_now())
                );
                assert!(!status.contains("hybrid"), "{status}");
                assert!(!status.contains("first-party"), "{status}");
            });
        });
    }

    #[test]
    fn credential_status_reports_hybrid_when_both_present() {
        with_auth_env(|| {
            with_first_party_env(None, || {
                save_token_to_disk(&existing_token()).expect("save dev-app token");
                write_first_party_creds();
                let status = super::credential_status().expect("status").expect("some");
                assert!(
                    status.contains("hybrid (dev-app reads, first-party writes)"),
                    "{status}"
                );
                assert!(
                    status.starts_with(&super::token_status_message(
                        &existing_token(),
                        super::unix_now()
                    )),
                    "{status}"
                );
            });
        });
    }

    #[test]
    fn credential_status_flags_first_party_only_as_rate_limited() {
        with_auth_env(|| {
            with_first_party_env(None, || {
                write_first_party_creds();
                let status = super::credential_status().expect("status").expect("some");
                assert!(status.contains("rate-limited"), "{status}");
                assert!(status.contains("spotuify login --dev-app"), "{status}");
            });
        });
    }

    #[test]
    fn credential_status_env_forced_first_party_overrides_hybrid() {
        with_auth_env(|| {
            // Both credentials on disk, but the user explicitly forced
            // first-party — the resolved mode (and its rate-limit caveat)
            // must win over the hybrid label.
            with_first_party_env(Some("1"), || {
                save_token_to_disk(&existing_token()).expect("save dev-app token");
                write_first_party_creds();
                let status = super::credential_status().expect("status").expect("some");
                assert!(status.contains("rate-limited"), "{status}");
                assert!(!status.contains("hybrid"), "{status}");
            });
        });
    }

    #[test]
    fn disk_token_cache_status_never_prints_token_material() {
        with_auth_env(|| {
            let token = fresh_token("access-secret-should-not-print", "refresh-secret-hidden");
            save_token_to_disk(&token).expect("save token");

            let status = disk_token_cache_status();

            assert!(status.contains("present"));
            assert!(status.contains("token.json"));
            assert!(!status.contains("access-secret-should-not-print"));
            assert!(!status.contains("refresh-secret-hidden"));
            assert!(!status.contains("Bearer"));
        });
    }

    #[test]
    fn legacy_data_dir_token_migrates_to_config_auth_file() {
        with_auth_env(|| {
            let token = fresh_token("legacy-access", "legacy-refresh");
            let legacy = super::legacy_token_cache_file();
            std::fs::create_dir_all(legacy.parent().expect("legacy parent")).expect("legacy dir");
            std::fs::write(&legacy, serde_json::to_string(&token).expect("json"))
                .expect("legacy token");

            assert!(!super::token_cache_file().exists());

            let loaded = super::load_token_bounded()
                .expect("load token")
                .expect("token present");

            assert_eq!(loaded.access_token, "legacy-access");
            assert!(super::token_cache_file().exists());
        });
    }

    #[test]
    fn unscoped_config_token_migrates_to_provider_auth_directory() {
        with_auth_env(|| {
            let token = fresh_token("old-config-access", "old-config-refresh");
            let previous = super::previous_token_cache_file();
            std::fs::create_dir_all(previous.parent().expect("previous parent"))
                .expect("previous dir");
            std::fs::write(&previous, serde_json::to_string(&token).expect("json"))
                .expect("previous token");

            let loaded = super::load_token_bounded()
                .expect("load token")
                .expect("token present");

            assert_eq!(loaded.access_token, "old-config-access");
            assert!(super::token_cache_file().exists());
        });
    }

    #[test]
    fn unscoped_first_party_credentials_migrate_to_provider_auth_directory() {
        with_auth_env(|| {
            let previous = super::previous_first_party_cache_file();
            std::fs::create_dir_all(previous.parent().expect("previous parent"))
                .expect("previous dir");
            std::fs::write(
                &previous,
                r#"{"auth_kind":"first-party","refresh_token":"AQmigrate","scopes":["streaming"]}"#,
            )
            .expect("previous credentials");

            let loaded = super::load_first_party_credentials()
                .expect("load credentials")
                .expect("credentials present");

            assert_eq!(loaded.refresh_token, "AQmigrate");
            assert!(super::first_party_cache_file().exists());
        });
    }

    #[test]
    fn invalid_auth_file_returns_error() {
        with_auth_env(|| {
            let path = super::token_cache_file();
            spotuify_protocol::paths::ensure_private_dir(path.parent().expect("auth parent"))
                .expect("auth dir");
            std::fs::write(&path, "{ definitely-not-json").expect("bad token file");

            let err = super::load_token_bounded().expect_err("invalid auth file should fail");

            assert!(err.to_string().contains("invalid JSON"), "{err:#}");
        });
    }

    #[test]
    fn auth_file_load_error_maps_to_client_error() {
        let err = super::map_token_load_error(anyhow::anyhow!("stored token is invalid JSON"));

        assert!(matches!(err, SpotifyError::Client { .. }));
    }

    #[test]
    fn disk_credential_snapshot_reads_auth_file_only() {
        with_auth_env(|| {
            save_token_to_disk(&existing_token()).expect("save token");

            let snapshot = super::stored_credential_disk_snapshot();
            assert!(
                matches!(
                    snapshot,
                    Some(crate::first_party::StoredCredential::LegacyDevApp(_))
                ),
                "expected disk legacy token, got {snapshot:?}"
            );
            if let Some(crate::first_party::StoredCredential::LegacyDevApp(token)) = snapshot {
                assert_eq!(token.access_token, "old-access");
            }
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
            save_token_to_disk(&old).expect("save token");
            let cache = Arc::new(Mutex::new(Some(old)));

            let err = access_token_cached(&config(), &http_client(), &cache)
                .await
                .expect_err("revoked refresh should fail");

            assert!(matches!(err, SpotifyError::AuthRevoked));
            assert!(cache.lock().await.is_none(), "memory cache should clear");
            assert!(
                load_token_from_disk().is_none(),
                "auth file should be removed"
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
            save_token_to_disk(&old).expect("save token");
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
            save_token_to_disk(&newer).expect("save token");
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
            save_token_to_disk(&newer).expect("save token");
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

    #[test]
    fn first_party_credentials_round_trip_via_disk() {
        with_auth_env(|| {
            let creds = crate::first_party::FirstPartyCredentials::new(
                "refresh-token-xyz",
                vec!["playlist-modify-private".to_string()],
            );
            super::save_first_party_credentials(&creds).expect("save first-party");

            let loaded = super::load_first_party_credentials()
                .expect("load first-party")
                .expect("first-party credentials present");
            assert_eq!(loaded, creds);

            // The persisted blob must carry only the refresh token, never
            // a Web API bearer.
            let raw = std::fs::read_to_string(super::first_party_cache_file())
                .expect("first-party cache file exists");
            assert!(raw.contains("refresh-token-xyz"));
            assert!(!raw.contains("access_token"));
        });
    }

    #[test]
    fn stored_credential_snapshot_prefers_first_party() {
        with_auth_env(|| {
            let creds = crate::first_party::FirstPartyCredentials::new("rt-first-party", vec![]);
            super::save_first_party_credentials(&creds).expect("save first-party");
            // A legacy token also present must NOT shadow the first-party one.
            super::save_token_to_disk(&existing_token()).expect("save token");

            let snapshot = super::stored_credential_snapshot().expect("snapshot");
            assert!(
                matches!(
                    snapshot,
                    Some(crate::first_party::StoredCredential::FirstParty(_))
                ),
                "expected first-party, got {snapshot:?}"
            );
            if let Some(crate::first_party::StoredCredential::FirstParty(c)) = snapshot {
                assert_eq!(c.refresh_token, "rt-first-party");
            }
        });
    }

    #[test]
    fn stored_credential_snapshot_reports_legacy_dev_app_when_only_legacy_present() {
        with_auth_env(|| {
            super::save_token_to_disk(&existing_token()).expect("save token");
            let snapshot = super::stored_credential_snapshot().expect("snapshot");
            assert!(
                matches!(
                    snapshot,
                    Some(crate::first_party::StoredCredential::LegacyDevApp(_))
                ),
                "expected legacy dev-app, got {snapshot:?}"
            );
            if let Some(crate::first_party::StoredCredential::LegacyDevApp(token)) = snapshot {
                assert_eq!(token.access_token, "old-access");
            }
        });
    }

    #[test]
    fn delete_first_party_clears_disk() {
        with_auth_env(|| {
            let creds = crate::first_party::FirstPartyCredentials::new("rt-del", vec![]);
            super::save_first_party_credentials(&creds).expect("save first-party");
            super::delete_first_party_credentials().expect("delete first-party");
            assert!(
                super::load_first_party_credentials()
                    .expect("load after delete")
                    .is_none(),
                "first-party credentials should be gone after delete"
            );
        });
    }

    #[test]
    fn custom_provider_credentials_are_isolated_by_provider_id() {
        with_auth_env(|| {
            let mut left = existing_token();
            left.access_token = "left-access".to_string();
            let mut right = existing_token();
            right.access_token = "right-access".to_string();
            super::save_dev_app_token_for("spotify-left", &left).expect("save left");
            super::save_dev_app_token_for("spotify-right", &right).expect("save right");

            assert!(super::credential_inventory_for("spotify-left")
                .expect("left inventory")
                .dev_app
                .is_some());
            assert!(super::credential_inventory_for("spotify-right")
                .expect("right inventory")
                .dev_app
                .is_some());

            super::purge_all_credentials_for("spotify-left").expect("purge left");
            assert!(super::credential_inventory_for("spotify-left")
                .expect("left after purge")
                .dev_app
                .is_none());
            assert!(super::credential_inventory_for("spotify-right")
                .expect("right after left purge")
                .dev_app
                .is_some());
        });
    }

    #[test]
    fn custom_provider_auth_mode_uses_only_its_own_credential_kind() {
        with_auth_env(|| {
            let first_party = crate::first_party::FirstPartyCredentials::new(
                "left-refresh",
                vec!["streaming".to_string()],
            );
            super::save_first_party_credentials_for("spotify-left", &first_party)
                .expect("save left first-party credential");
            super::save_dev_app_token_for("spotify-right", &existing_token())
                .expect("save right dev-app credential");

            assert!(super::stored_first_party_only_for("spotify-left"));
            assert!(!super::stored_first_party_only_for("spotify-right"));
            assert!(!super::stored_first_party_only_for("spotify"));
        });
    }

    #[test]
    fn logout_purges_current_previous_legacy_and_librespot_credentials() {
        with_auth_env(|| {
            let token = existing_token();
            let credentials = crate::first_party::FirstPartyCredentials::new(
                "first-party-refresh",
                vec!["streaming".to_string()],
            );
            super::save_dev_app_token(&token).expect("save current token");
            super::save_first_party_credentials(&credentials).expect("save current first-party");

            let token_json = serde_json::to_vec(&token).expect("encode token");
            for path in [
                super::previous_token_cache_file(),
                super::legacy_token_cache_file(),
            ] {
                std::fs::create_dir_all(path.parent().expect("token parent")).expect("mkdir");
                std::fs::write(path, &token_json).expect("write older token");
            }
            let first_party_json = credentials.to_json().expect("encode first-party");
            for path in [
                super::previous_first_party_cache_file(),
                super::legacy_first_party_cache_file(),
            ] {
                std::fs::create_dir_all(path.parent().expect("first-party parent")).expect("mkdir");
                std::fs::write(path, &first_party_json).expect("write older first-party");
            }
            let librespot = spotuify_protocol::paths::cache_dir()
                .join("librespot")
                .join("creds");
            std::fs::create_dir_all(&librespot).expect("librespot creds dir");
            std::fs::write(librespot.join("credentials.json"), "native-secret")
                .expect("librespot credential");

            let result = super::purge_all_credentials().expect("atomic credential purge");
            assert!(result.removed_dev_app);
            assert!(result.removed_first_party);
            assert!(result.removed_librespot);
            assert!(super::credential_inventory()
                .expect("inventory after purge")
                .dev_app
                .is_none());
            assert!(super::load_first_party_credentials()
                .expect("first-party after purge")
                .is_none());
        });
    }

    #[test]
    fn stale_cached_dev_token_cannot_resurrect_after_logout() {
        run_auth_async(|| async {
            let mut token = existing_token();
            token.expires_at = 0;
            super::save_dev_app_token(&token).expect("save token");
            let cache = Arc::new(Mutex::new(Some(token)));
            super::purge_all_credentials().expect("logout purge");

            let error = super::access_token_cached(&config(), &http_client(), &cache)
                .await
                .expect_err("stale cache must not refresh after logout");
            assert!(matches!(error, SpotifyError::AuthRequired));
            assert!(!super::token_cache_file().exists());
        });
    }

    #[test]
    fn in_flight_first_party_rotation_cannot_resurrect_after_logout() {
        with_auth_env(|| {
            let old = crate::first_party::FirstPartyCredentials::new("old-refresh", vec![]);
            let rotated = crate::first_party::FirstPartyCredentials::new("new-refresh", vec![]);
            super::save_first_party_credentials(&old).expect("save old credential");
            super::purge_all_credentials().expect("logout purge");

            assert!(
                !super::save_rotated_first_party_credentials(&old.refresh_token, &rotated,)
                    .expect("conditional rotation")
            );
            assert!(super::load_first_party_credentials()
                .expect("credential after rejected rotation")
                .is_none());
        });
    }

    #[cfg(unix)]
    #[test]
    fn logout_refuses_symlinked_librespot_credentials() {
        use std::os::unix::fs::symlink;

        with_auth_env(|| {
            let outside = tempfile::tempdir().expect("outside tempdir");
            std::fs::write(outside.path().join("keep"), "secret").expect("outside sentinel");
            let librespot_parent = spotuify_protocol::paths::cache_dir().join("librespot");
            std::fs::create_dir_all(&librespot_parent).expect("librespot parent");
            symlink(outside.path(), librespot_parent.join("creds")).expect("credential symlink");

            let error = super::purge_all_credentials()
                .expect_err("logout must reject symlinked credential directory");
            assert!(
                error.to_string().contains("symlinked librespot")
                    || error.to_string().contains("outside its instance directory")
            );
            assert!(outside.path().join("keep").exists());
        });
    }

    #[test]
    fn redirect_listener_rejects_non_loopback_hosts() {
        let err = super::bind_redirect_listener("http://192.0.2.10:8888/callback")
            .expect_err("non-loopback redirect should be refused before bind");

        assert!(
            err.to_string().contains("not loopback"),
            "error should explain loopback requirement, got: {err}"
        );
    }

    #[test]
    fn redirect_loopback_check_accepts_localhost_and_loopback_ips() {
        assert!(super::redirect_host_is_loopback("localhost"));
        assert!(super::redirect_host_is_loopback("LOCALHOST"));
        assert!(super::redirect_host_is_loopback("127.0.0.1"));
        assert!(super::redirect_host_is_loopback("::1"));
        assert!(!super::redirect_host_is_loopback("192.0.2.10"));
        assert!(!super::redirect_host_is_loopback("example.com"));
    }
}
