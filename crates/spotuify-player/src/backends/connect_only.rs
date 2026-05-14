//! ConnectOnlyBackend — Phase 9.0e.
//!
//! Remote-controls an existing Spotify Connect device via the Web API.
//! No local audio output, so this backend works for Free accounts and
//! headless servers. The user's "active device" is whoever owns the
//! current Spotify session at the moment a command is issued.
//!
//! Design:
//! - Holds a `TokenProvider` so the daemon can hand over a keyring or
//!   bridge-sourced token at construction time. The trait keeps the
//!   backend testable with `StaticTokenProvider`.
//! - `register_device` does NOT call the Web API. ConnectOnly does not
//!   own a Connect device; commands fall through to whichever device
//!   the user already activated in another Spotify client. Calling
//!   the API here would 401 on first launch.
//! - Each command applies a bounded 5s timeout so a hung Spotify can't
//!   block the daemon.
//! - Error mapping is locked to the user-visible categories: 403 →
//!   PremiumRequired, 404 → NoActiveDevice, 401 → Auth, 5xx → Network,
//!   timeout → Timeout. Generic 4xx falls to Playback so the message
//!   isn't lost.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use reqwest::{Method, StatusCode};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::{
    BackendKind, DeviceId, PlayerBackend, PlayerError, PlayerEvent, PlayerResult, RepeatMode,
};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

/// Source of the Web API bearer token. The keyring-backed
/// implementation lives in the daemon wiring; tests use
/// `StaticTokenProvider`.
pub trait TokenProvider: Send + Sync {
    fn current_token(&self) -> Option<String>;
}

/// Test/utility provider that returns a fixed token (or none).
pub struct StaticTokenProvider {
    inner: Option<String>,
}

impl StaticTokenProvider {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            inner: Some(token.into()),
        }
    }

    pub fn missing() -> Self {
        Self { inner: None }
    }
}

impl TokenProvider for StaticTokenProvider {
    fn current_token(&self) -> Option<String> {
        self.inner.clone()
    }
}

pub struct ConnectOnlyBackend {
    http: reqwest::Client,
    api_base: String,
    token: Arc<dyn TokenProvider>,
    events_tx: mpsc::UnboundedSender<PlayerEvent>,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    registered: bool,
    device_id: Option<DeviceId>,
}

impl ConnectOnlyBackend {
    /// Construct with the canonical Spotify Web API base. Production
    /// wiring uses this; tests reach for `with_base_url`.
    pub fn new(token: Arc<dyn TokenProvider>) -> (Self, UnboundedReceiverStream<PlayerEvent>) {
        Self::with_base_url("https://api.spotify.com".to_string(), token)
    }

    pub fn with_base_url(
        api_base: String,
        token: Arc<dyn TokenProvider>,
    ) -> (Self, UnboundedReceiverStream<PlayerEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let backend = Self {
            http: reqwest::Client::builder()
                .timeout(COMMAND_TIMEOUT)
                .build()
                .expect("reqwest client builds with default settings"),
            api_base,
            token,
            events_tx: tx,
            state: Mutex::new(State::default()),
        };
        (backend, UnboundedReceiverStream::new(rx))
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.api_base.trim_end_matches('/'))
    }

    fn require_token(&self) -> PlayerResult<String> {
        self.token
            .current_token()
            .ok_or_else(|| PlayerError::Auth("no Spotify token available".to_string()))
    }

    fn require_registered(&self) -> PlayerResult<()> {
        if self.state.lock().registered {
            Ok(())
        } else {
            Err(PlayerError::NotInitialised)
        }
    }

    async fn send_command(&self, builder: reqwest::RequestBuilder) -> PlayerResult<()> {
        let resp = match builder.send().await {
            Ok(resp) => resp,
            Err(err) if err.is_timeout() => return Err(PlayerError::Timeout(COMMAND_TIMEOUT)),
            Err(err) => return Err(PlayerError::Network(err.to_string())),
        };
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        Err(map_status(status, resp).await)
    }

    fn emit(&self, event: PlayerEvent) {
        let _ = self.events_tx.send(event);
    }
}

async fn map_status(status: StatusCode, resp: reqwest::Response) -> PlayerError {
    // Adversarial classification: each category drives a different
    // user-visible code path in the daemon, so we lock them by HTTP
    // status rather than trying to parse Spotify error JSON (which
    // changes shape across endpoints).
    let body = resp.text().await.unwrap_or_default();
    match status {
        StatusCode::FORBIDDEN => PlayerError::PremiumRequired,
        StatusCode::NOT_FOUND => PlayerError::NoActiveDevice,
        StatusCode::UNAUTHORIZED => PlayerError::Auth(format!("Spotify rejected token: {body}")),
        s if s.is_server_error() => PlayerError::Network(format!("Spotify {s}: {body}")),
        s => PlayerError::Playback(format!("Spotify {s}: {body}")),
    }
}

#[async_trait]
impl PlayerBackend for ConnectOnlyBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Connect
    }

    async fn register_device(&mut self, name: &str) -> PlayerResult<DeviceId> {
        // ConnectOnly does not own a Spotify device — playback runs on
        // whichever client the user has already activated. We mint a
        // synthetic id so callers have a stable handle in receipts and
        // event payloads.
        let id = DeviceId::new(format!("connect-only-{name}"));
        {
            let mut state = self.state.lock();
            state.registered = true;
            state.device_id = Some(id.clone());
        }
        self.emit(PlayerEvent::Ready {
            device_id: id.clone(),
            name: name.to_string(),
        });
        Ok(id)
    }

    async fn play_uri(&mut self, uri: &str, position_ms: u32) -> PlayerResult<()> {
        self.require_registered()?;
        let token = self.require_token()?;
        let body = serde_json::json!({
            "uris": [uri],
            "position_ms": position_ms as u64,
        });
        let req = self
            .http
            .request(Method::PUT, self.url("/v1/me/player/play"))
            .bearer_auth(token)
            .json(&body);
        self.send_command(req).await
    }

    async fn pause(&mut self) -> PlayerResult<()> {
        self.require_registered()?;
        let token = self.require_token()?;
        let req = self
            .http
            .request(Method::PUT, self.url("/v1/me/player/pause"))
            .bearer_auth(token);
        self.send_command(req).await
    }

    async fn resume(&mut self) -> PlayerResult<()> {
        // Spotify "resume" is just /play with no body — the existing
        // playback context carries forward.
        self.require_registered()?;
        let token = self.require_token()?;
        let req = self
            .http
            .request(Method::PUT, self.url("/v1/me/player/play"))
            .bearer_auth(token);
        self.send_command(req).await
    }

    async fn next(&mut self) -> PlayerResult<()> {
        self.require_registered()?;
        let token = self.require_token()?;
        let req = self
            .http
            .request(Method::POST, self.url("/v1/me/player/next"))
            .bearer_auth(token);
        self.send_command(req).await
    }

    async fn previous(&mut self) -> PlayerResult<()> {
        self.require_registered()?;
        let token = self.require_token()?;
        let req = self
            .http
            .request(Method::POST, self.url("/v1/me/player/previous"))
            .bearer_auth(token);
        self.send_command(req).await
    }

    async fn seek(&mut self, position_ms: u32) -> PlayerResult<()> {
        self.require_registered()?;
        let token = self.require_token()?;
        let req = self
            .http
            .request(Method::PUT, self.url("/v1/me/player/seek"))
            .bearer_auth(token)
            .query(&[("position_ms", position_ms.to_string())]);
        self.send_command(req).await
    }

    async fn volume(&mut self, percent: u8) -> PlayerResult<()> {
        self.require_registered()?;
        if percent > 100 {
            return Err(PlayerError::InvalidArg(format!(
                "volume_percent must be 0-100 (got {percent})"
            )));
        }
        let token = self.require_token()?;
        let req = self
            .http
            .request(Method::PUT, self.url("/v1/me/player/volume"))
            .bearer_auth(token)
            .query(&[("volume_percent", percent.to_string())]);
        self.send_command(req).await
    }

    async fn shuffle(&mut self, on: bool) -> PlayerResult<()> {
        self.require_registered()?;
        let token = self.require_token()?;
        let req = self
            .http
            .request(Method::PUT, self.url("/v1/me/player/shuffle"))
            .bearer_auth(token)
            .query(&[("state", on.to_string())]);
        self.send_command(req).await
    }

    async fn repeat(&mut self, mode: RepeatMode) -> PlayerResult<()> {
        self.require_registered()?;
        let token = self.require_token()?;
        let req = self
            .http
            .request(Method::PUT, self.url("/v1/me/player/repeat"))
            .bearer_auth(token)
            .query(&[("state", mode.label())]);
        self.send_command(req).await
    }

    async fn is_connected(&self) -> bool {
        self.state.lock().registered
    }

    async fn shutdown(&mut self) -> PlayerResult<()> {
        let mut state = self.state.lock();
        state.registered = false;
        state.device_id = None;
        Ok(())
    }
}
