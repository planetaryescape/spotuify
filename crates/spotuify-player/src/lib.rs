//! Player backend abstraction for spotuify.
//!
//! Defines the `PlayerBackend` trait the daemon uses to register a
//! Spotify Connect device and dispatch playback commands. The shipped
//! runtime backend is `EmbeddedBackend`: an in-process librespot Player
//! + Spirc with mercury bus and gapless preload. The mock backend is
//!   for tests only.
//!
//! See `docs/implementation/12-phase-9-librespot-embed.md` for the
//! full design; `docs/blueprint/07-player.md` for the non-negotiable
//! action set.

pub mod backends;
pub mod config;
pub mod events;

pub use config::PlayerSettings;
pub use events::PlayerEvent;
pub use spotuify_core::BackendKind;

/// Names of the local audio output devices the embedded player can render
/// to, for the output-device picker. Returns an empty list when the active
/// audio backend doesn't support enumeration.
#[cfg(feature = "audio-device-enumeration")]
pub fn list_audio_outputs() -> Vec<String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let mut names: Vec<String> = cpal::default_host()
        .output_devices()
        .map(|devices| devices.filter_map(|device| device.name().ok()).collect())
        .unwrap_or_default();
    names.sort();
    names.dedup();
    names
}

#[cfg(not(feature = "audio-device-enumeration"))]
pub fn list_audio_outputs() -> Vec<String> {
    Vec::new()
}

/// The current system default output device name (cpal), or `None` if there is
/// no default / enumeration isn't supported. Used by the daemon's "follow the
/// system default output" watcher to detect when the user switches outputs.
#[cfg(feature = "audio-device-enumeration")]
pub fn current_default_output_name() -> Option<String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    cpal::default_host()
        .default_output_device()
        .and_then(|device| device.name().ok())
}

#[cfg(not(feature = "audio-device-enumeration"))]
pub fn current_default_output_name() -> Option<String> {
    None
}

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Newtype wrapper so the daemon doesn't confuse device IDs with
/// arbitrary strings in command receipts and event payloads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceId(pub String);

impl DeviceId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Repeat mode. Names follow `docs/blueprint/07-player.md` — these are
/// the user-facing labels, not Spotify's raw API strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    Off,
    Context,
    Track,
}

impl RepeatMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Context => "context",
            Self::Track => "track",
        }
    }

    pub fn parse(value: &str) -> Result<Self, PlayerError> {
        match value {
            "off" => Ok(Self::Off),
            "context" => Ok(Self::Context),
            "track" => Ok(Self::Track),
            other => Err(PlayerError::InvalidArg(format!(
                "repeat mode `{other}` invalid (expected off, context, track)"
            ))),
        }
    }
}

/// Typed player errors. The daemon translates these into wire-level
/// `DaemonEvent` and `Response::Error` values; the variants here are
/// the seams that matter for the trait contract.
#[derive(Debug, thiserror::Error)]
pub enum PlayerError {
    #[error("backend not initialised — call register_device first")]
    NotInitialised,
    #[error("streaming requires Spotify Premium; current account is not premium")]
    PremiumRequired,
    #[error("no active Spotify Connect device available")]
    NoActiveDevice,
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("playback failed: {0}")]
    Playback(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("operation timed out after {0:?}")]
    Timeout(std::time::Duration),
    #[error("invalid argument: {0}")]
    InvalidArg(String),
    #[error("backend does not support `{0}`")]
    Unsupported(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type PlayerResult<T> = std::result::Result<T, PlayerError>;

/// PlayerBackend — the daemon-side abstraction over every way we can
/// drive Spotify playback.
///
/// Backends emit `PlayerEvent`s through a channel injected at
/// construction time (each backend's `new`/`builder`). The daemon
/// subscribes to that channel and translates events into wire-level
/// `DaemonEvent`s for IPC clients.
///
/// `is_connected` and `web_api_token` are inspected by `spotuify
/// doctor` and by Phase 9.4's token bridge respectively. Both have
/// safe defaults for backends that don't expose them.
#[async_trait]
pub trait PlayerBackend: Send + Sync {
    /// Which variant this is. Used for diagnostics and doctor output.
    fn kind(&self) -> BackendKind;

    /// Register a Connect device under `name` and bring the backend
    /// into a ready state. Idempotent for already-running backends.
    async fn register_device(&mut self, name: &str) -> PlayerResult<DeviceId>;

    async fn play_uri(&mut self, uri: &str, position_ms: u32) -> PlayerResult<()>;
    async fn pause(&mut self) -> PlayerResult<()>;
    async fn resume(&mut self) -> PlayerResult<()>;
    async fn next(&mut self) -> PlayerResult<()>;
    async fn previous(&mut self) -> PlayerResult<()>;
    async fn seek(&mut self, position_ms: u32) -> PlayerResult<()>;
    async fn volume(&mut self, percent: u8) -> PlayerResult<()>;
    async fn shuffle(&mut self, on: bool) -> PlayerResult<()>;
    async fn repeat(&mut self, mode: RepeatMode) -> PlayerResult<()>;

    /// Best-effort audio preload for a playable URI. Only the embedded
    /// librespot backend owns an audio buffer; remote-control backends
    /// return `Unsupported`.
    async fn preload_uri(&mut self, _uri: &str) -> PlayerResult<()> {
        Err(PlayerError::Unsupported("preload_uri".to_string()))
    }

    /// Append a track, episode, album, or playlist URI to the active
    /// device's queue. librespot 0.8's `Spirc::add_to_queue` is the
    /// fast path on the embedded backend; remote-control backends fall
    /// back to a Web API call. Artist and show URIs are rejected at the
    /// caller layer (see daemon's queueable_uris_for_selection).
    ///
    /// Spirc gotcha: this silently no-ops if the embedded device is
    /// not currently active. Callers should ensure activate-first
    /// (handled by the daemon's activate-first guard).
    async fn queue_add(&mut self, _uri: &str) -> PlayerResult<()> {
        Err(PlayerError::Unsupported("queue_add".to_string()))
    }

    /// Whether the backend currently has a healthy connection to
    /// Spotify (Connect device registered, session valid).
    async fn is_connected(&self) -> bool;

    /// Web API token bridged out of the streaming session. Default
    /// `None` — only the embedded backend exposes a real value in
    /// Phase 9.4.
    async fn web_api_token(&self) -> Option<String> {
        None
    }

    /// Mercury bus fetch (lyrics, radio, related artists). Embedded
    /// backend only; everyone else returns `Unsupported`.
    async fn mercury_get(&self, _uri: &str) -> PlayerResult<bytes::Bytes> {
        Err(PlayerError::Unsupported("mercury_get".to_string()))
    }

    /// Gracefully tear down. The caller drops the trait object after.
    async fn shutdown(&mut self) -> PlayerResult<()>;
}

#[cfg(test)]
mod tests {
    use super::{DeviceId, PlayerError, RepeatMode};

    #[test]
    fn device_id_display_passes_through() {
        assert_eq!(DeviceId::new("abc").to_string(), "abc");
    }

    #[test]
    fn repeat_mode_round_trips_through_label() {
        for mode in [RepeatMode::Off, RepeatMode::Context, RepeatMode::Track] {
            assert_eq!(
                RepeatMode::parse(mode.label()).expect("repeat mode label should parse"),
                mode
            );
        }
    }

    #[test]
    fn repeat_mode_invalid_value_surfaces_input() {
        let err = RepeatMode::parse("loop").expect_err("invalid repeat mode should error");
        // Adversarial: error must echo what the user typed so it's
        // useful in a CLI failure path.
        assert!(matches!(err, PlayerError::InvalidArg(ref msg) if msg.contains("loop")));
    }
}
