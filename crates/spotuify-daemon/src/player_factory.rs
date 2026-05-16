//! Player backend factory — librespot-only since Phase 0 cleanup
//! (2026-05-16). Builds the `Box<dyn PlayerBackend>` the daemon owns.
//! With `--features embedded-playback` we construct the in-process
//! librespot backend; without it the daemon errors at startup (there
//! is no longer a Spotifyd or ConnectOnly fallback).

use std::sync::Arc;

use anyhow::Result;
use parking_lot::RwLock;
use spotuify_audio::SharedAnalyzer;
use spotuify_player::{PlayerBackend, PlayerEvent};
use spotuify_spotify::config::Config;
use tokio_stream::wrappers::UnboundedReceiverStream;

/// Synchronous token provider backed by an `Arc<RwLock<_>>` slot the
/// daemon refreshes from the keyring-cached token. Embedded
/// librespot's TokenProvider reads it on every Web API call.
pub(crate) struct DaemonTokenProvider {
    inner: Arc<RwLock<Option<String>>>,
}

impl DaemonTokenProvider {
    pub(crate) fn new(slot: Arc<RwLock<Option<String>>>) -> Self {
        Self { inner: slot }
    }
}

impl spotuify_player::backends::token_bridge::TokenProvider for DaemonTokenProvider {
    fn current_token(&self) -> Option<String> {
        self.inner.read().clone()
    }
}

/// Build the embedded librespot backend + event stream. Returns an
/// error on init failure — there is no fallback chain.
pub(crate) fn build_player(
    config: &Config,
    token_slot: Arc<RwLock<Option<String>>>,
    viz_analyzer: Option<SharedAnalyzer>,
) -> Result<(Box<dyn PlayerBackend>, UnboundedReceiverStream<PlayerEvent>)> {
    build_embedded(config, token_slot, viz_analyzer)
}

#[cfg(feature = "embedded-playback")]
fn build_embedded(
    config: &Config,
    token_slot: Arc<RwLock<Option<String>>>,
    viz_analyzer: Option<SharedAnalyzer>,
) -> Result<(Box<dyn PlayerBackend>, UnboundedReceiverStream<PlayerEvent>)> {
    use spotuify_player::backends::embedded::{EmbeddedBackend, EmbeddedCachePaths};

    let cache_root = dirs::cache_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".cache")))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("spotuify");
    let paths = EmbeddedCachePaths::under(cache_root, config.player.audio_cache_mib);
    let token = Arc::new(DaemonTokenProvider::new(token_slot));
    let (backend, stream) = EmbeddedBackend::new_with_analyzer(paths, token, viz_analyzer)
        .map_err(|err| anyhow::anyhow!("EmbeddedBackend init failed: {err}"))?;
    // Arc<EmbeddedBackend> -> Box<dyn PlayerBackend> requires an owned
    // value. The factory holds the only reference at this point so
    // try_unwrap is infallible in practice.
    let backend = Arc::try_unwrap(backend)
        .map_err(|_| anyhow::anyhow!("internal: EmbeddedBackend Arc had unexpected sharing"))?;
    Ok((Box::new(backend), stream))
}

#[cfg(not(feature = "embedded-playback"))]
fn build_embedded(
    _config: &Config,
    _token_slot: Arc<RwLock<Option<String>>>,
    _viz_analyzer: Option<SharedAnalyzer>,
) -> Result<(Box<dyn PlayerBackend>, UnboundedReceiverStream<PlayerEvent>)> {
    anyhow::bail!(
        "spotuify was built without --features embedded-playback. \
         Rebuild with the feature enabled and a concrete audio backend \
         (alsa-backend / pipewire-backend / rodio-backend / portaudio-backend)."
    )
}
