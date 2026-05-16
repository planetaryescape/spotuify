//! Phase 9.2 — premium gate.
//!
//! Wraps the Web API `GET /me` check the daemon does before letting
//! `EmbeddedBackend` initialise librespot. Lives in the backends
//! module so ConnectOnly can borrow the same `HttpWebApiClient` when
//! Phase 9.4 wires token refresh.
//!
//! The gate isolates four user-visible outcomes:
//! - `Allowed` — premium account, librespot init can proceed.
//! - `Denied { product }` — not premium; daemon emits PremiumRequired,
//!   does not call the init closure.
//! - `Auth` — token expired/missing; surfaces as a typed error so the
//!   daemon can prompt re-login instead of "upgrade your account".
//! - `Timeout` — bounded 5s so a hung Spotify doesn't wedge startup.

use std::future::Future;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use spotuify_spotify::client::user_agent_string;
use thiserror::Error;

const GATE_TIMEOUT: Duration = Duration::from_secs(5);

/// What `GET /me` decided. Carries the raw `product` field on
/// denial so the daemon can include it in the PremiumRequired
/// banner (debuggers love seeing "open" vs "free" verbatim).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PremiumDecision {
    Allowed,
    Denied { product: String },
}

#[derive(Debug, Error)]
pub enum GateError {
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("premium gate timed out after {0:?}")]
    Timeout(Duration),
    #[error("premium gate network error: {0}")]
    Network(String),
    #[error("premium gate decoding error: {0}")]
    Decode(String),
}

/// Trait so future backends (the embedded librespot path, the
/// `connect_only` path) can plug in different token sources without
/// re-implementing the HTTP shape.
#[async_trait]
pub trait WebApiClient: Send + Sync {
    async fn get_me(&self) -> Result<MeResponse, GateError>;
}

#[derive(Debug, Clone, Deserialize)]
pub struct MeResponse {
    pub product: String,
}

/// Real Web API impl. Tests reach for `with_base_url` so they can
/// point at a wiremock server.
pub struct HttpWebApiClient {
    http: reqwest::Client,
    base_url: String,
    token: String,
}

impl HttpWebApiClient {
    pub fn new(token: String) -> Self {
        Self::with_base_url("https://api.spotify.com".to_string(), token)
    }

    pub fn with_base_url(base_url: String, token: String) -> Self {
        Self {
            http: reqwest::Client::builder()
                .user_agent(user_agent_string())
                .timeout(GATE_TIMEOUT)
                .build()
                .expect("reqwest client builds with default settings"),
            base_url,
            token,
        }
    }
}

#[async_trait]
impl WebApiClient for HttpWebApiClient {
    async fn get_me(&self) -> Result<MeResponse, GateError> {
        let url = format!("{}/v1/me", self.base_url.trim_end_matches('/'));
        let resp = match self.http.get(&url).bearer_auth(&self.token).send().await {
            Ok(resp) => resp,
            Err(err) if err.is_timeout() => return Err(GateError::Timeout(GATE_TIMEOUT)),
            Err(err) => return Err(GateError::Network(err.to_string())),
        };
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            let body = resp.text().await.unwrap_or_default();
            return Err(GateError::Auth(format!("401: {body}")));
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(GateError::Network(format!("{status}: {body}")));
        }
        resp.json::<MeResponse>()
            .await
            .map_err(|err| GateError::Decode(err.to_string()))
    }
}

/// Run the gate, then call `init` only when the account is premium.
///
/// The init closure is what would brings up librespot in Phase 9.3 —
/// keeping it behind this gate function is the seam tests use to
/// assert "init never runs on Free".
pub async fn check_premium_then_init<F, Fut, E>(
    client: &dyn WebApiClient,
    init: F,
) -> Result<PremiumDecision, GateError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), E>>,
    E: std::fmt::Display,
{
    let me = client.get_me().await?;
    if me.product == "premium" {
        if let Err(err) = init().await {
            // We surface init errors as a generic "network" failure
            // so the daemon's main loop can decide policy. The exact
            // error type leaks through tracing.
            tracing::warn!(error = %err, "premium gate init closure failed");
            return Err(GateError::Network(err.to_string()));
        }
        Ok(PremiumDecision::Allowed)
    } else {
        Ok(PremiumDecision::Denied {
            product: me.product,
        })
    }
}
