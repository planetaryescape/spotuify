//! Player backend abstraction for spotuify.
//!
//! Defines the provider-neutral `PlayerBackend` trait the daemon uses to
//! register a local playback device and dispatch playback commands. A music
//! provider may supply one backend or no backend at all. Provider-native
//! metadata workflows and authentication stay on provider adapter facets.
//!
//! See `docs/blueprint/07-player.md` for the non-negotiable action set.

pub mod backends;
pub mod config;
pub mod events;

pub use config::PlayerSettings;
pub use events::PlayerEvent;
pub use spotuify_core::{PlaySource, ProviderId, RepeatMode, ResourceUri, UriScheme};

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

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::backends::audio_counter_tap::AudioCounterHandle;

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

/// A request to start playback of a collection context at a specific
/// track. Built daemon-side and handed to [`PlayerBackend::play_context`].
///
/// [`PlaySource`] makes single-item, provider-context, and explicit ordered
/// playback mutually exclusive. `start_uri` identifies the item playback
/// begins at inside that source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayContextRequest {
    pub source: PlaySource,
    pub start_uri: ResourceUri,
    pub position_ms: u32,
}

impl PlayContextRequest {
    pub fn single(start_uri: ResourceUri, position_ms: u32) -> Self {
        Self {
            source: PlaySource::Single,
            start_uri,
            position_ms,
        }
    }

    pub fn validate(&self) -> PlayerResult<()> {
        if let PlaySource::Ordered(uris) = &self.source {
            if uris.is_empty() || !uris.contains(&self.start_uri) {
                return Err(PlayerError::InvalidArg(
                    "ordered playback source must contain start_uri".to_string(),
                ));
            }
        }
        Ok(())
    }
}

/// Typed player errors. The daemon translates these into wire-level
/// `DaemonEvent` and `Response::Error` values; the variants here are
/// the seams that matter for the trait contract.
#[derive(Debug, thiserror::Error)]
pub enum PlayerError {
    #[error("backend not initialised — call register_device first")]
    NotInitialised,
    #[error("provider policy prevents playback: {0}")]
    ProviderPolicy(String),
    #[error("no active playback device available")]
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

/// Validate the optional local-player facet for one provider registry entry.
///
/// `None` is valid and is the canonical metadata-only/transportless state.
/// A present backend must claim the same provider identity and URI namespace;
/// otherwise typed URIs could be dispatched to the wrong adapter.
pub fn validate_backend_pairing(
    provider_id: &ProviderId,
    uri_scheme: &UriScheme,
    backend: Option<&dyn PlayerBackend>,
) -> PlayerResult<()> {
    let Some(backend) = backend else {
        return Ok(());
    };
    if backend.provider_id() != provider_id {
        return Err(PlayerError::InvalidArg(format!(
            "player backend provider `{}` does not match registry provider `{provider_id}`",
            backend.provider_id()
        )));
    }
    if backend.uri_scheme() != uri_scheme {
        return Err(PlayerError::InvalidArg(format!(
            "player backend URI scheme `{}` does not match registry scheme `{uri_scheme}`",
            backend.uri_scheme()
        )));
    }
    Ok(())
}

/// Daemon-side abstraction over a provider-supplied local player.
///
/// Backends emit `PlayerEvent`s through a channel injected at
/// construction time (each backend's `new`/`builder`). The daemon
/// subscribes to that channel and translates events into wire-level
/// `DaemonEvent`s for IPC clients.
///
/// Absence of this object is the representation for a metadata-only provider;
/// callers must capability-gate before dispatch instead of installing a null
/// backend that produces an error for every command.
#[async_trait]
pub trait PlayerBackend: Send + Sync {
    fn provider_id(&self) -> &ProviderId;
    fn uri_scheme(&self) -> &UriScheme;

    /// Update the local audio output device used when the sink chain is
    /// (re)built. Takes effect on the next `register_device`, so callers
    /// pair it with a reconnect. `None` follows the system default.
    /// Backends without a local sink ignore it.
    fn set_audio_output_device(&mut self, _device: Option<String>) {}

    /// Register a playback device under `name` and bring the backend
    /// into a ready state. Idempotent for already-running backends.
    async fn register_device(&mut self, name: &str) -> PlayerResult<DeviceId>;

    async fn play_uri(&mut self, uri: &ResourceUri, position_ms: u32) -> PlayerResult<()>;

    /// Load a collection context (album/playlist URI, or an explicit
    /// ordered track list) and start playback at `request.start_uri`.
    ///
    /// A backend with native context support overrides this to load the full
    /// context so "Next" advances through the collection.
    /// Backends without native context support fall back to a lone-track
    /// load — behaviourally the pre-context single-track path.
    async fn play_context(&mut self, request: PlayContextRequest) -> PlayerResult<()> {
        request.validate()?;
        self.play_uri(&request.start_uri, request.position_ms).await
    }

    async fn pause(&mut self) -> PlayerResult<()>;
    async fn resume(&mut self) -> PlayerResult<()>;
    async fn next(&mut self) -> PlayerResult<()>;
    async fn previous(&mut self) -> PlayerResult<()>;
    async fn seek(&mut self, position_ms: u32) -> PlayerResult<()>;
    async fn volume(&mut self, percent: u8) -> PlayerResult<()>;
    async fn shuffle(&mut self, on: bool) -> PlayerResult<()>;
    async fn repeat(&mut self, mode: RepeatMode) -> PlayerResult<()>;

    /// Best-effort audio preload for a playable URI. Backends without a local
    /// audio buffer return `Unsupported`.
    async fn preload_uri(&mut self, _uri: &ResourceUri) -> PlayerResult<()> {
        Err(PlayerError::Unsupported("preload_uri".to_string()))
    }

    /// Append a queueable resource URI to the active device's queue. Callers
    /// validate media-kind and capability support before dispatch.
    async fn queue_add(&mut self, _uri: &ResourceUri) -> PlayerResult<()> {
        Err(PlayerError::Unsupported("queue_add".to_string()))
    }

    /// Whether the backend currently has a healthy playback session.
    async fn is_connected(&self) -> bool;

    /// PCM-audio counter exposed by backends that own decoded samples.
    /// Remote/control-only backends return `None`; analytics falls back
    /// to bounded wall-clock derivation in that case.
    fn audio_counter(&self) -> Option<Arc<AudioCounterHandle>> {
        None
    }

    /// Gracefully tear down. The caller drops the trait object after.
    async fn shutdown(&mut self) -> PlayerResult<()>;
}

#[cfg(test)]
mod tests {
    use super::{DeviceId, PlayContextRequest, PlaySource, PlayerError, RepeatMode, ResourceUri};

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
        assert!(err.value.contains("loop"));
    }

    #[test]
    fn ordered_context_must_contain_typed_start_uri() {
        let request = PlayContextRequest {
            source: PlaySource::Ordered(vec![
                ResourceUri::parse("fake:track:other").expect("valid URI")
            ]),
            start_uri: ResourceUri::parse("fake:track:start").expect("valid URI"),
            position_ms: 0,
        };
        assert!(matches!(
            request.validate(),
            Err(PlayerError::InvalidArg(ref message)) if message.contains("contain start_uri")
        ));
    }

    #[test]
    fn provider_policy_error_does_not_encode_one_service_or_account_tier() {
        let error = PlayerError::ProviderPolicy("region restricted".to_string());
        assert_eq!(
            error.to_string(),
            "provider policy prevents playback: region restricted"
        );
    }
}
