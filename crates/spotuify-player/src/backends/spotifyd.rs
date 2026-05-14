//! SpotifydBackend — Phase 9.1.
//!
//! Composes `ConnectOnlyBackend` (for Web API command dispatch) with
//! the legacy `spotuify_spotify::spotifyd` subprocess helper. The
//! daemon picks this backend when the user wants crash-isolation via
//! a sibling spotifyd process — today's default during the Phase 9
//! rollout.
//!
//! Design:
//! - All playback commands go through the Web API (same as
//!   ConnectOnly). Spotify routes them to whichever device is active;
//!   spotifyd is just our local Connect device that owns audio output.
//! - `register_device` ensures spotifyd is started (when
//!   `autostart=true`) before announcing readiness. Users who run
//!   spotifyd under launchd/systemd set `autostart=false` and the
//!   backend still works — it just doesn't try to spawn anything.
//! - `is_connected` reflects registration state; richer spotifyd
//!   process health checks live in the daemon's diagnostics module.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::warn;

use super::connect_only::{ConnectOnlyBackend, TokenProvider};
use crate::{BackendKind, DeviceId, PlayerBackend, PlayerEvent, PlayerResult, RepeatMode};

/// Knobs the wrapper needs that aren't part of the Web API path. Kept
/// as a struct so the daemon can build one straight from
/// `spotuify_spotify::config::Config` without dragging the full
/// `Config` type into the player crate.
#[derive(Clone, Debug)]
pub struct SpotifydSettings {
    pub autostart: bool,
    pub spotifyd_config_path: PathBuf,
}

pub struct SpotifydBackend {
    inner: ConnectOnlyBackend,
    settings: SpotifydSettings,
}

impl SpotifydBackend {
    /// Build the backend and the receiving end of its event channel.
    /// Production callers pass the canonical `api_base`; tests reach
    /// for this with a wiremock server URL.
    pub fn with_settings(
        api_base: String,
        token: Arc<dyn TokenProvider>,
        settings: SpotifydSettings,
    ) -> (Self, UnboundedReceiverStream<PlayerEvent>) {
        let (inner, stream) = ConnectOnlyBackend::with_base_url(api_base, token);
        (Self { inner, settings }, stream)
    }

    fn ensure_spotifyd_started(&self) {
        if !self.settings.autostart {
            return;
        }
        let config = spotuify_spotify::config::Config {
            client_id: String::new(),
            client_secret: None,
            redirect_uri: String::new(),
            config_path: PathBuf::new(),
            spotifyd_config_path: self.settings.spotifyd_config_path.clone(),
            spotifyd_device_name: None,
            spotifyd_autostart: self.settings.autostart,
            player: spotuify_spotify::config::PlayerConfig::default(),
            analytics: spotuify_spotify::config::AnalyticsConfig::default(),
        };
        match spotuify_spotify::spotifyd::ensure_started(&config) {
            Ok(_) => {}
            Err(err) => {
                // Don't fail register_device — the user might run
                // spotifyd manually. Surface as a tracing warning so
                // doctor can pick it up via the log.
                warn!(error = %err, "spotifyd autostart failed; continuing");
            }
        }
    }
}

#[async_trait]
impl PlayerBackend for SpotifydBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Spotifyd
    }

    async fn register_device(&mut self, name: &str) -> PlayerResult<DeviceId> {
        self.ensure_spotifyd_started();
        self.inner.register_device(name).await
    }

    async fn play_uri(&mut self, uri: &str, position_ms: u32) -> PlayerResult<()> {
        self.inner.play_uri(uri, position_ms).await
    }

    async fn pause(&mut self) -> PlayerResult<()> {
        self.inner.pause().await
    }

    async fn resume(&mut self) -> PlayerResult<()> {
        self.inner.resume().await
    }

    async fn next(&mut self) -> PlayerResult<()> {
        self.inner.next().await
    }

    async fn previous(&mut self) -> PlayerResult<()> {
        self.inner.previous().await
    }

    async fn seek(&mut self, position_ms: u32) -> PlayerResult<()> {
        self.inner.seek(position_ms).await
    }

    async fn volume(&mut self, percent: u8) -> PlayerResult<()> {
        self.inner.volume(percent).await
    }

    async fn shuffle(&mut self, on: bool) -> PlayerResult<()> {
        self.inner.shuffle(on).await
    }

    async fn repeat(&mut self, mode: RepeatMode) -> PlayerResult<()> {
        self.inner.repeat(mode).await
    }

    async fn is_connected(&self) -> bool {
        self.inner.is_connected().await
    }

    async fn shutdown(&mut self) -> PlayerResult<()> {
        self.inner.shutdown().await
    }
}
