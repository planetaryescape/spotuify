//! Phase 6.1: typed Spotify Web API error model.
//!
//! Replaces `anyhow::Result<T>` in the Spotify client surface with a typed
//! enum so callers can classify rate-limit, auth, network, and decode
//! failures without string-matching.
//!
//! The legacy string-bail paths in `src/spotify.rs` will be migrated to
//! return `Result<T, SpotifyError>` once this enum stabilises. Tests live
//! in `tests/error_classification.rs` and exercise the classifier against
//! all the Spotify-flavoured response shapes documented in
//! `docs/implementation/09-phase-6-sync-hardening.md` §6.1.

use std::time::Duration;

use chrono::{DateTime, Utc};

/// Auth error categories per Phase 6 spec.
///
/// Wired into `DaemonEvent::AuthError { kind }`. Carries no payload because
/// the recovery action depends on the kind, not on a message string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthErrorKind {
    NotLoggedIn,
    ExpiredRefresh,
    InvalidGrant,
    Forbidden,
    /// Stored token was issued before some currently-required scopes
    /// were added. Recovery: `spotuify logout && spotuify login`.
    /// Emitted proactively at daemon startup so the TUI can prompt the
    /// user without waiting for the first 403.
    ScopeReauthRequired,
}

/// Phase 6 typed error for Spotify Web API operations.
pub type SpotifyResult<T> = std::result::Result<T, SpotifyError>;

#[derive(Debug, Clone, thiserror::Error)]
pub enum SpotifyError {
    #[error("not logged in; run `spotuify login`")]
    AuthRequired,
    #[error("rate limited (scope {scope}); retry after {retry_after:?}")]
    RateLimited {
        retry_after: Duration,
        scope: String,
    },
    #[error("auth expired; refresh required")]
    AuthExpired,
    #[error("auth revoked; re-login required")]
    AuthRevoked,
    #[error("forbidden: Spotify token missing the {scope} permission")]
    Forbidden { scope: String },
    #[error("not found")]
    NotFound,
    #[error("Spotify deprecated endpoint {endpoint} after 2024-11")]
    Deprecated { endpoint: &'static str },
    #[error("network failure on {endpoint}: {message}")]
    Network { endpoint: String, message: String },
    #[error("decode failure on {endpoint}: {message}")]
    Decode { endpoint: String, message: String },
    #[error("Spotify API {status} on {endpoint}: {message} (body: {body})")]
    Api {
        status: u16,
        endpoint: String,
        message: String,
        body: String,
    },
    #[error("invalid Spotify request: {message}")]
    InvalidInput { message: String },
    #[error("Spotify client error: {message}")]
    Client { message: String },
}

/// Default Retry-After value when Spotify omits the header on a 429.
///
/// RFC 9110 §15.5.7 (formerly RFC 7231) allows the server to omit
/// `Retry-After`. spotify-tui, ncspot, and spotatui all default to 60s in
/// this case. We use the same default; configurable via daemon settings
/// once Phase 6.3 (rate-limit middleware) lands.
pub const DEFAULT_RETRY_AFTER_SECS: u64 = 60;

/// Clamp ceiling for absurd Retry-After values. Spotify has been seen to
/// return very large values (days) which would freeze sync indefinitely;
/// we cap at 1 hour so the user still sees progress.
pub const MAX_RETRY_AFTER_SECS: u64 = 3600;

impl SpotifyError {
    /// Whether the operation should be retried after the daemon's normal
    /// backoff. RateLimited respects `Retry-After` upstream; 5xx is retried
    /// with jittered exponential backoff; 4xx is fatal except 401/429.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimited { .. } | Self::AuthExpired | Self::Network { .. } => true,
            Self::Api { status, .. } => (500..=599).contains(status),
            _ => false,
        }
    }

    /// Map to `IpcErrorKind` for the protocol response envelope. Stable: any
    /// remapping is a protocol breaking change.
    pub fn ipc_kind(&self) -> spotuify_protocol::IpcErrorKind {
        use spotuify_protocol::IpcErrorKind as K;
        match self {
            Self::RateLimited { .. } => K::RateLimited,
            Self::AuthRequired => K::Auth,
            Self::AuthRevoked => K::AuthRevoked,
            Self::AuthExpired => K::Auth,
            Self::Forbidden { .. } => K::Auth,
            Self::NotFound | Self::Deprecated { .. } | Self::InvalidInput { .. } => K::Provider,
            Self::Network { .. } => K::Network,
            Self::Decode { .. } | Self::Api { .. } | Self::Client { .. } => K::Provider,
        }
    }
}

impl From<anyhow::Error> for SpotifyError {
    fn from(err: anyhow::Error) -> Self {
        if let Some(error) = err.downcast_ref::<SpotifyError>() {
            return error.clone();
        }
        Self::Client {
            message: err.to_string(),
        }
    }
}

impl From<serde_json::Error> for SpotifyError {
    fn from(err: serde_json::Error) -> Self {
        Self::Decode {
            endpoint: "local-json".to_string(),
            message: err.to_string(),
        }
    }
}

/// Parse an HTTP `Retry-After` header per RFC 9110 §10.2.3.
///
/// Accepts either an integer second-delay or an HTTP-date. Clamps result
/// to `[0, MAX_RETRY_AFTER_SECS]`. Returns `DEFAULT_RETRY_AFTER_SECS` for
/// `None` or unparseable input.
pub fn parse_retry_after(header: Option<&str>, now: DateTime<Utc>) -> Duration {
    let secs = match header {
        None => DEFAULT_RETRY_AFTER_SECS,
        Some(raw) => {
            let raw = raw.trim();
            if let Ok(seconds) = raw.parse::<u64>() {
                seconds.min(MAX_RETRY_AFTER_SECS)
            } else if let Ok(when) = DateTime::parse_from_rfc2822(raw) {
                let delta = when.with_timezone(&Utc) - now;
                let secs = delta.num_seconds().max(0) as u64;
                secs.min(MAX_RETRY_AFTER_SECS)
            } else {
                DEFAULT_RETRY_AFTER_SECS
            }
        }
    };
    Duration::from_secs(secs)
}

/// Classify a Spotify Web API response into a typed [`SpotifyError`].
///
/// Inputs are extracted from a `reqwest::Response` so this function stays
/// HTTP-stack-agnostic and testable without any network.
///
/// `endpoint` is a short symbolic label like `"GET /me/player"` used in
/// error messages and telemetry; it must not contain query parameters that
/// could leak user data (URIs, search terms, IDs).
pub fn classify_response(
    status: u16,
    retry_after: Option<&str>,
    endpoint: &str,
    body: &str,
    now: DateTime<Utc>,
) -> SpotifyError {
    match status {
        429 => SpotifyError::RateLimited {
            retry_after: parse_retry_after(retry_after, now),
            scope: endpoint.to_string(),
        },
        401 => SpotifyError::AuthExpired,
        403 => {
            // Only emit `Forbidden { scope }` when Spotify's message
            // actually names a missing scope. Otherwise surface
            // Spotify's body verbatim — many 403s are *not* scope
            // failures (e.g. "Player command failed: Restriction
            // violated", "Premium required", device-not-available)
            // and labelling them as scope issues sends the user on
            // a re-auth chase that fixes nothing.
            if let Some(scope) = parse_required_scope(body) {
                SpotifyError::Forbidden { scope }
            } else {
                let message = spotify_error_message(body);
                let message = if message.is_empty() {
                    "Spotify refused the request (403)".to_string()
                } else {
                    message
                };
                SpotifyError::Api {
                    status: 403,
                    endpoint: endpoint.to_string(),
                    message,
                    body: body.to_string(),
                }
            }
        }
        // A bare 404 (e.g. GET on a deleted track) collapses to the
        // tiny `NotFound` variant since there's nothing useful to say.
        // But Spotify's *playback* 404s come with a structured body
        // explaining what failed (`"Player command failed: No active
        // device found"`, `"Player command failed: Restriction violated"`,
        // etc.). Route those to `Api` so the message reaches the user.
        404 => {
            let message = spotify_error_message(body);
            if message.is_empty() || message.eq_ignore_ascii_case("not found") {
                SpotifyError::NotFound
            } else {
                SpotifyError::Api {
                    status: 404,
                    endpoint: endpoint.to_string(),
                    message,
                    body: body.to_string(),
                }
            }
        }
        410 => SpotifyError::Deprecated {
            endpoint: deprecated_label(endpoint),
        },
        s @ 500..=599 => SpotifyError::Api {
            status: s,
            endpoint: endpoint.to_string(),
            message: spotify_error_message(body),
            body: body.to_string(),
        },
        s => SpotifyError::Api {
            status: s,
            endpoint: endpoint.to_string(),
            message: spotify_error_message(body),
            body: body.to_string(),
        },
    }
}

/// Extract Spotify's machine-readable `error.message` from the body when
/// present (Spotify API error shape: `{"error":{"status":403,"message":"…"}}`).
/// Falls back to a stable empty string so error displays don't leak body bytes.
pub fn spotify_error_message(body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(msg) = value
            .get("error")
            .and_then(|err| err.get("message"))
            .and_then(serde_json::Value::as_str)
        {
            return msg.to_string();
        }
    }
    String::new()
}

/// Try to read the missing OAuth scope from a 403 response body. Spotify's
/// shape: `{"error":{"status":403,"message":"Insufficient client scope"}}`.
/// When the message names the scope, return it; otherwise None.
fn parse_required_scope(body: &str) -> Option<String> {
    let msg = spotify_error_message(body);
    // Spotify error messages don't include a structured "required_scope"
    // field. Best-effort heuristic: look for "scope" in the message; if the
    // host is calling a known scope-gated endpoint they'll know which.
    if msg.to_lowercase().contains("scope") {
        Some(msg)
    } else {
        None
    }
}

/// The set of endpoints Spotify retired in November 2024. New apps get 410
/// on these regardless of OAuth scope.
fn deprecated_label(endpoint: &str) -> &'static str {
    if endpoint.contains("audio-features") {
        "audio-features"
    } else if endpoint.contains("audio-analysis") {
        "audio-analysis"
    } else if endpoint.contains("recommendations") {
        "recommendations"
    } else if endpoint.contains("related-artists") {
        "related-artists"
    } else if endpoint.contains("featured-playlists") {
        "featured-playlists"
    } else {
        "unknown-deprecated"
    }
}
