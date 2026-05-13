//! Phase 6.1: typed Spotify Web API error model.
//!
//! Replaces `anyhow::Result<T>` in the Spotify client surface with a typed
//! enum so callers can classify rate-limit, auth, network, and decode
//! failures without string-matching.

/// Auth error categories per Phase 6 spec.
///
/// Wired into `DaemonEvent::AuthError { kind }`. Carries no payload because
/// the recovery action depends on the kind, not on a message string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthErrorKind {
    ExpiredRefresh,
    InvalidGrant,
    Forbidden,
}

/// Phase 6 typed error for Spotify Web API operations.
///
/// Stub variant set during Phase 6.1 buildup; the full enum lands once the
/// adversarial test suite (`tests/error_classification.rs`) is green.
#[derive(Debug, thiserror::Error)]
pub enum SpotifyError {
    #[error("rate limited (scope {scope}); retry after {retry_after:?}")]
    RateLimited {
        retry_after: std::time::Duration,
        scope: String,
    },
    #[error("auth expired; refresh required")]
    AuthExpired,
    #[error("auth revoked; re-login required")]
    AuthRevoked,
    #[error("forbidden: scope {scope} required")]
    Forbidden { scope: String },
    #[error("not found")]
    NotFound,
    #[error("Spotify deprecated endpoint {endpoint} after 2024-11")]
    Deprecated { endpoint: &'static str },
    #[error("network failure: {0}")]
    Network(String),
    #[error("decode failure on {endpoint}: {message}")]
    Decode { endpoint: String, message: String },
    #[error("Spotify API {status} on {endpoint}: {message}")]
    Api {
        status: u16,
        endpoint: String,
        message: String,
        body: String,
    },
}

impl SpotifyError {
    /// Whether the operation should be retried after the daemon's normal
    /// backoff. RateLimited respects `Retry-After` upstream; 5xx is retried
    /// with jittered exponential backoff; 4xx is fatal except 401/429.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimited { .. } | Self::AuthExpired | Self::Network(_) => true,
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
            Self::AuthExpired | Self::AuthRevoked => K::Auth,
            Self::Forbidden { .. } => K::Auth,
            Self::NotFound | Self::Deprecated { .. } => K::Provider,
            Self::Network(_) => K::Network,
            Self::Decode { .. } | Self::Api { .. } => K::Provider,
        }
    }
}
