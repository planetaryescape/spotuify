//! First-party (keymaster) Web API auth minting.
//!
//! Replaces the per-user dev-app PKCE flow. A first-party login uses
//! librespot's keymaster client id, which is never in Spotify's
//! Development Mode, so the minted Web API bearer can write playlists
//! where a dev-app token gets a 403 (verified 2026-05-24: dev-app token
//! -> 403; keymaster token -> 429, i.e. authorized, only rate-limited).
//!
//! Flow:
//! 1. One browser login (`librespot-oauth`, keymaster id) yields an
//!    [`OAuthToken`] with a long-lived refresh token. We persist only
//!    the refresh token (as [`FirstPartyCredentials`]); the Web API
//!    bearer is minted live and never written to disk.
//! 2. The librespot `Session` bootstraps from the OAuth access token
//!    (or its own cached native credentials on later starts).
//! 3. `session.login5().auth_token()` mints the full-scope Web API
//!    bearer for ALL Web API calls. It re-mints from the live session
//!    without a browser, and survives keymaster-OAuth-endpoint outages
//!    (the failure mode spotify-player hit in Aug 2025), so it is the
//!    steady-state token source; OAuth refresh is the fallback.
//!
//! This module owns the librespot calls; the persisted credential type
//! ([`FirstPartyCredentials`]) lives in `spotuify-spotify`, which has no
//! librespot dependency.

use std::time::{Duration, Instant};

use librespot_core::authentication::Credentials;
use librespot_core::session::Session;
use librespot_oauth::{OAuthClient, OAuthClientBuilder, OAuthToken};
use spotuify_spotify::first_party::FirstPartyCredentials;

use crate::backends::token_bridge::TokenWithExpiry;
use crate::PlayerError;

/// librespot's first-party "keymaster" client id. Same id spotify-player
/// and ncspot use; never in Development Mode.
pub const KEYMASTER_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";

/// Loopback redirect the keymaster client accepts. Distinct from the
/// dev-app flow's `:8888/callback` so the two can coexist during
/// migration.
pub const REDIRECT_URI: &str = "http://127.0.0.1:8898/login";

/// Scopes requested at browser login. `login5` mints a full-scope bearer
/// regardless, but requesting these makes the OAuth access token usable
/// directly as a fallback if `login5` is ever unavailable.
pub const WEB_API_SCOPES: &[&str] = &[
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

fn build_oauth_client() -> Result<OAuthClient, PlayerError> {
    OAuthClientBuilder::new(KEYMASTER_CLIENT_ID, REDIRECT_URI, WEB_API_SCOPES.to_vec())
        .open_in_browser()
        .build()
        .map_err(|err| PlayerError::Auth(format!("oauth client build failed: {err}")))
}

/// Run the interactive browser login. **Blocking**: librespot-oauth opens
/// the browser, prints the URL, and waits on a synchronous loopback
/// listener. Call from a blocking context (see [`login`] for the async
/// wrapper).
pub fn login_blocking() -> Result<OAuthToken, PlayerError> {
    let client = build_oauth_client()?;
    client
        .get_access_token()
        .map_err(|err| PlayerError::Auth(format!("first-party login failed: {err}")))
}

/// Async wrapper over [`login_blocking`] that keeps the blocking OAuth
/// listener off the runtime's worker threads.
pub async fn login() -> Result<OAuthToken, PlayerError> {
    tokio::task::spawn_blocking(login_blocking)
        .await
        .map_err(|err| PlayerError::Auth(format!("login task join failed: {err}")))?
}

/// Refresh the OAuth token (no browser) from a stored refresh token.
/// This is the *fallback* path for re-bootstrapping the session; the
/// steady-state bearer comes from [`mint_via_login5`].
pub async fn refresh_oauth(refresh_token: &str) -> Result<OAuthToken, PlayerError> {
    let client = build_oauth_client()?;
    client
        .refresh_token_async(refresh_token)
        .await
        .map_err(|err| PlayerError::Auth(format!("oauth refresh failed: {err}")))
}

/// Bound on a single `login5().auth_token()` call. The manager caches
/// internally so this is normally instant; the timeout exists so a hung
/// network call can't wedge the player actor (which serializes minting
/// with transport commands).
const LOGIN5_MINT_TIMEOUT: Duration = Duration::from_secs(10);

/// Mint a full-scope Web API bearer from a live librespot session via
/// `login5`. The session's `Login5Manager` caches internally and only
/// re-mints when within seconds of expiry, so this is cheap to call.
/// Bounded by [`LOGIN5_MINT_TIMEOUT`] so a stuck call surfaces as a
/// timeout instead of blocking the actor forever.
pub async fn mint_via_login5(session: &Session) -> Result<TokenWithExpiry, PlayerError> {
    let token = tokio::time::timeout(LOGIN5_MINT_TIMEOUT, session.login5().auth_token())
        .await
        .map_err(|_| PlayerError::Timeout(LOGIN5_MINT_TIMEOUT))?
        .map_err(|err| PlayerError::Auth(format!("login5 mint failed: {err}")))?;
    Ok(web_api_token_with_expiry(
        token.access_token,
        token.expires_in,
        Instant::now(),
    ))
}

/// librespot `Credentials` that bootstrap a session from an OAuth access
/// token. After the first connect, librespot persists reusable native
/// credentials to its own cache, so later starts need no token.
pub fn credentials_from_oauth(token: &OAuthToken) -> Credentials {
    Credentials::with_access_token(token.access_token.clone())
}

/// Convert a fresh OAuth login/refresh into the persisted credential
/// shape. Only the refresh token (and scopes, for diagnostics) is kept.
pub fn credentials_from_oauth_token(token: &OAuthToken) -> FirstPartyCredentials {
    FirstPartyCredentials::new(token.refresh_token.clone(), token.scopes.clone())
}

/// Pure mapping from a `login5` token's relative `expires_in` to the
/// absolute `Instant` the [`crate::backends::token_bridge::TokenBridge`]
/// expects. Factored out so it can be unit-tested without a session.
fn web_api_token_with_expiry(
    access_token: String,
    expires_in: Duration,
    now: Instant,
) -> TokenWithExpiry {
    TokenWithExpiry {
        access_token,
        expires_at: now + expires_in,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        credentials_from_oauth_token, web_api_token_with_expiry, KEYMASTER_CLIENT_ID, REDIRECT_URI,
        WEB_API_SCOPES,
    };
    use librespot_oauth::OAuthToken;
    use std::time::{Duration, Instant};

    fn sample_oauth_token() -> OAuthToken {
        OAuthToken {
            access_token: "access-xyz".to_string(),
            refresh_token: "refresh-xyz".to_string(),
            expires_at: Instant::now() + Duration::from_secs(3600),
            token_type: "Bearer".to_string(),
            scopes: vec!["playlist-modify-private".to_string()],
        }
    }

    #[test]
    fn keymaster_client_id_is_the_first_party_id() {
        // Locking this guards against an accidental swap back to a
        // dev-app id, which would re-introduce the 403 on writes.
        assert_eq!(KEYMASTER_CLIENT_ID, "65b708073fc0480ea92a077233ca87bd");
        assert!(REDIRECT_URI.starts_with("http://127.0.0.1:"));
    }

    #[test]
    fn requested_scopes_include_playlist_and_library_writes() {
        // Adversarial: the whole point of the rework is write access.
        // If these drop out, writes silently regress to read-only.
        assert!(WEB_API_SCOPES.contains(&"playlist-modify-private"));
        assert!(WEB_API_SCOPES.contains(&"playlist-modify-public"));
        assert!(WEB_API_SCOPES.contains(&"user-library-modify"));
    }

    #[test]
    fn credentials_keep_only_the_refresh_token_and_scopes() {
        // The access token is a live bearer and must never be persisted.
        let creds = credentials_from_oauth_token(&sample_oauth_token());
        assert_eq!(creds.refresh_token, "refresh-xyz");
        assert_eq!(creds.scopes, vec!["playlist-modify-private".to_string()]);
        let json = creds.to_json().expect("serialize");
        assert!(!json.contains("access-xyz"), "bearer must not be persisted");
    }

    #[test]
    fn login5_expiry_is_relative_to_now() {
        let now = Instant::now();
        let token = web_api_token_with_expiry("bearer".to_string(), Duration::from_secs(3600), now);
        assert_eq!(token.access_token, "bearer");
        assert_eq!(token.expires_at, now + Duration::from_secs(3600));
    }
}
