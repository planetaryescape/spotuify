//! Player backend factory — librespot-only since Phase 0 cleanup
//! (2026-05-16). Builds the `Box<dyn PlayerBackend>` the daemon owns.
//! With `--features embedded-playback` we construct the in-process
//! librespot backend; without it the daemon errors at startup (there
//! is no longer a Spotifyd or ConnectOnly fallback).

use std::sync::Arc;

use anyhow::Result;
use parking_lot::RwLock;
use spotuify_audio::SharedAnalyzer;
use spotuify_config::PlayerSettings;
use spotuify_core::{ProviderId, UriScheme};
use spotuify_player::{PlayerBackend, PlayerEvent};
use tokio_stream::wrappers::UnboundedReceiverStream;

pub(crate) struct BuiltPlayer {
    pub(crate) backend: Box<dyn PlayerBackend>,
    pub(crate) stream: UnboundedReceiverStream<PlayerEvent>,
    #[cfg(feature = "embedded-playback")]
    pub(crate) session: spotuify_player::backends::embedded::EmbeddedSessionHandle,
}

/// Synchronous token provider backed by an `Arc<RwLock<_>>` slot the
/// daemon refreshes from the auth-file-backed token. Embedded
/// librespot's TokenProvider reads it on every Web API call.
#[cfg(feature = "embedded-playback")]
pub(crate) struct DaemonTokenProvider {
    inner: Arc<RwLock<Option<String>>>,
}

#[cfg(feature = "embedded-playback")]
impl DaemonTokenProvider {
    pub(crate) fn new(slot: Arc<RwLock<Option<String>>>) -> Self {
        Self { inner: slot }
    }
}

#[cfg(feature = "embedded-playback")]
impl spotuify_player::backends::token_bridge::TokenProvider for DaemonTokenProvider {
    fn current_token(&self) -> Option<String> {
        self.inner.read().clone()
    }
}

/// Build the embedded librespot backend + event stream. Returns an
/// error on init failure — there is no fallback chain.
pub(crate) fn build_player(
    provider_id: ProviderId,
    uri_scheme: UriScheme,
    config: &PlayerSettings,
    token_slot: Arc<RwLock<Option<String>>>,
    viz_analyzer: Option<SharedAnalyzer>,
) -> Result<BuiltPlayer> {
    build_embedded(provider_id, uri_scheme, config, token_slot, viz_analyzer)
}

#[cfg(feature = "embedded-playback")]
fn build_embedded(
    provider_id: ProviderId,
    uri_scheme: UriScheme,
    config: &PlayerSettings,
    token_slot: Arc<RwLock<Option<String>>>,
    viz_analyzer: Option<SharedAnalyzer>,
) -> Result<BuiltPlayer> {
    use spotuify_player::backends::embedded::{EmbeddedBackend, EmbeddedCachePaths};

    let cache_root = spotuify_protocol::paths::cache_dir();
    let paths = EmbeddedCachePaths::under(cache_root, config.audio_cache_mib);
    let token = Arc::new(DaemonTokenProvider::new(token_slot));
    let (backend, stream) = EmbeddedBackend::new_with_analyzer_for_provider(
        provider_id.clone(),
        paths,
        token,
        viz_analyzer,
        config.audio_output_device.clone(),
    )
    .map_err(|err| anyhow::anyhow!("EmbeddedBackend init failed: {err}"))?;
    let session = backend.session_handle();
    spotuify_player::validate_backend_pairing(&provider_id, &uri_scheme, Some(backend.as_ref()))
        .map_err(|error| anyhow::anyhow!("embedded backend pairing failed: {error}"))?;
    // Arc<EmbeddedBackend> -> Box<dyn PlayerBackend> requires an owned
    // value. The factory holds the only reference at this point so
    // try_unwrap is infallible in practice.
    let backend = Arc::try_unwrap(backend)
        .map_err(|_| anyhow::anyhow!("internal: EmbeddedBackend Arc had unexpected sharing"))?;
    Ok(BuiltPlayer {
        backend: Box::new(backend),
        stream,
        session,
    })
}

#[cfg(not(feature = "embedded-playback"))]
fn build_embedded(
    _provider_id: ProviderId,
    _uri_scheme: UriScheme,
    _config: &PlayerSettings,
    _token_slot: Arc<RwLock<Option<String>>>,
    _viz_analyzer: Option<SharedAnalyzer>,
) -> Result<BuiltPlayer> {
    anyhow::bail!(
        "spotuify was built without --features embedded-playback. \
         Rebuild with the feature enabled and a concrete audio backend \
         (alsa-backend / pipewire-backend / rodio-backend / portaudio-backend)."
    )
}
