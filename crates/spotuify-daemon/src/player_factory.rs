//! Phase 9.1 — backend factory.
//!
//! Builds the `Box<dyn PlayerBackend>` the daemon owns, keyed on
//! `[player] backend` from config. Each backend hands back its event
//! stream; the daemon drains it and translates `PlayerEvent`s into
//! wire-level `DaemonEvent`s in a spawned task.

use std::sync::Arc;

use anyhow::Result;
use parking_lot::RwLock;
use spotuify_audio::SharedAnalyzer;
use spotuify_core::BackendKind;
use spotuify_player::backends::connect_only::{ConnectOnlyBackend, TokenProvider};
use spotuify_player::backends::spotifyd::{SpotifydBackend, SpotifydSettings};
use spotuify_player::{PlayerBackend, PlayerEvent};
use spotuify_spotify::config::Config;
use tokio_stream::wrappers::UnboundedReceiverStream;

const SPOTIFY_API_BASE: &str = "https://api.spotify.com";

/// Synchronous token provider backed by an `Arc<RwLock<_>>` slot the
/// daemon refreshes from the keyring-cached token. ConnectOnly +
/// Spotifyd backends read it on every Web API call; Embedded
/// (Phase 9.4) routes the librespot-derived token here too.
pub(crate) struct DaemonTokenProvider {
    inner: Arc<RwLock<Option<String>>>,
}

impl DaemonTokenProvider {
    pub(crate) fn new(slot: Arc<RwLock<Option<String>>>) -> Self {
        Self { inner: slot }
    }
}

impl TokenProvider for DaemonTokenProvider {
    fn current_token(&self) -> Option<String> {
        self.inner.read().clone()
    }
}

/// Construct the backend + event stream pair for the configured kind.
///
/// Phase 9.1 shipped ConnectOnly + Spotifyd. With `embedded-playback`
/// enabled, Embedded constructs the in-process librespot backend;
/// otherwise it falls back to Spotifyd with a tracing warning.
pub(crate) fn build_player(
    config: &Config,
    token_slot: Arc<RwLock<Option<String>>>,
    viz_analyzer: Option<SharedAnalyzer>,
) -> Result<(Box<dyn PlayerBackend>, UnboundedReceiverStream<PlayerEvent>)> {
    let kind = config.player.backend;
    match kind {
        BackendKind::Connect => {
            let token = Arc::new(DaemonTokenProvider::new(token_slot));
            let (backend, stream) =
                ConnectOnlyBackend::with_base_url(SPOTIFY_API_BASE.to_string(), token);
            Ok((Box::new(backend), stream))
        }
        BackendKind::Spotifyd => {
            let token = Arc::new(DaemonTokenProvider::new(token_slot));
            let (backend, stream) = SpotifydBackend::with_settings(
                SPOTIFY_API_BASE.to_string(),
                token,
                SpotifydSettings {
                    autostart: config.spotifyd_autostart,
                    spotifyd_config_path: config.spotifyd_config_path.clone(),
                },
            );
            Ok((Box::new(backend), stream))
        }
        BackendKind::Embedded => build_embedded(config, token_slot, viz_analyzer),
    }
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
    let token = Arc::new(DaemonTokenProvider::new(token_slot.clone()));
    match EmbeddedBackend::new_with_analyzer(paths, token, viz_analyzer) {
        Ok((backend, stream)) => {
            // Arc<EmbeddedBackend> -> Box<dyn PlayerBackend> would
            // require Arc impls — easier: wrap in a thin shim that
            // takes &mut self by leaning on Arc::get_mut. Concretely
            // we hold a Mutex<EmbeddedBackend> inside the daemon, so
            // returning Box<dyn> here means we must own a single
            // instance. We can't Box<Arc<...>>; rebuild as Box<dyn>
            // by unwrapping the Arc when refcount == 1 (it is, here).
            let backend = Arc::try_unwrap(backend).map_err(|_| {
                anyhow::anyhow!("internal: EmbeddedBackend Arc had unexpected sharing")
            })?;
            Ok((Box::new(backend), stream))
        }
        Err(err) => {
            tracing::warn!(
                error = %err,
                "EmbeddedBackend init failed; falling back to spotifyd"
            );
            let token = Arc::new(DaemonTokenProvider::new(token_slot));
            let (backend, stream) = SpotifydBackend::with_settings(
                SPOTIFY_API_BASE.to_string(),
                token,
                SpotifydSettings {
                    autostart: config.spotifyd_autostart,
                    spotifyd_config_path: config.spotifyd_config_path.clone(),
                },
            );
            Ok((Box::new(backend), stream))
        }
    }
}

#[cfg(not(feature = "embedded-playback"))]
fn build_embedded(
    config: &Config,
    token_slot: Arc<RwLock<Option<String>>>,
    _viz_analyzer: Option<SharedAnalyzer>,
) -> Result<(Box<dyn PlayerBackend>, UnboundedReceiverStream<PlayerEvent>)> {
    tracing::warn!(
        "config player.backend = `embedded` but spotuify was built without \
         --features embedded-playback. Falling back to spotifyd."
    );
    let token = Arc::new(DaemonTokenProvider::new(token_slot));
    let (backend, stream) = SpotifydBackend::with_settings(
        SPOTIFY_API_BASE.to_string(),
        token,
        SpotifydSettings {
            autostart: config.spotifyd_autostart,
            spotifyd_config_path: config.spotifyd_config_path.clone(),
        },
    );
    Ok((Box::new(backend), stream))
}
