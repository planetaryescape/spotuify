//! EmbeddedBackend — Phase 9.2+ skeleton.
//!
//! Hosts an in-process librespot Session + Player + Spirc so a single
//! `spotuify` binary registers as a Spotify Connect device. Phase 9.2
//! provides the skeleton + cache wiring; Phase 9.3 fills in Player +
//! Spirc + worker loop + RecoveringSink; Phase 9.4 adds the two-token
//! bridge and mercury bus; Phase 9.5 ships the audio-backend matrix.
//!
//! When the `embedded-playback` feature is off the whole module is
//! `#[cfg]`'d out and the daemon's player factory falls back to
//! spotifyd or connect (Phase 9.1 behaviour).

// Phase 9.5 guard: `embedded-playback` without a concrete audio
// backend is misconfigured — librespot would compile but have no
// way to emit audio. Force a build break with a useful message.
#[cfg(not(any(
    feature = "alsa-backend",
    feature = "pipewire-backend",
    feature = "rodio-backend",
    feature = "portaudio-backend",
)))]
compile_error!(
    "feature `embedded-playback` requires exactly one audio backend feature: \
     `alsa-backend`, `pipewire-backend`, `rodio-backend`, or `portaudio-backend`. \
     See docs/implementation/12-phase-9-librespot-embed.md (audio backend matrix) \
     for the recommended per-platform default."
);

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use librespot_core::cache::Cache;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::warn;

use crate::{
    BackendKind, DeviceId, PlayerBackend, PlayerError, PlayerEvent, PlayerResult, RepeatMode,
};

/// Cache layout under `~/.cache/spotuify/librespot/`. The three
/// subdirs match librespot's `Cache::new(creds, volume, audio, size)`
/// argument layout.
pub struct EmbeddedCachePaths {
    pub creds: PathBuf,
    pub volume: PathBuf,
    pub audio: Option<PathBuf>,
    pub audio_size_mib: Option<u64>,
}

impl EmbeddedCachePaths {
    pub fn under(base: PathBuf, audio_cache_mib: u32) -> Self {
        let root = base.join("librespot");
        Self {
            creds: root.join("creds"),
            volume: root.join("volume"),
            audio: (audio_cache_mib > 0).then(|| root.join("audio")),
            audio_size_mib: (audio_cache_mib > 0).then_some(audio_cache_mib as u64),
        }
    }
}

/// EmbeddedBackend — Phase 9.2 skeleton.
///
/// Holds the librespot cache + a placeholder for the Session/Player/
/// Spirc trio that 9.3 wires. Today most methods short-circuit to
/// `Unsupported` or `NotInitialised`; the daemon factory still
/// prefers spotifyd until 9.3 lands the real wiring.
pub struct EmbeddedBackend {
    cache: Cache,
    events_tx: mpsc::UnboundedSender<PlayerEvent>,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    device_name: Option<String>,
}

impl EmbeddedBackend {
    /// Construct from the configured cache root. Returns the backend
    /// plus the receiving end of its event stream so the daemon can
    /// drain it through the same translator as ConnectOnly/Spotifyd.
    pub fn new(
        paths: EmbeddedCachePaths,
    ) -> PlayerResult<(Arc<Self>, UnboundedReceiverStream<PlayerEvent>)> {
        // Phase 9.5 — make spotuify show up nicely in pavucontrol on
        // Linux. The env vars are inherited by librespot's audio
        // backend when it spawns the PulseAudio stream.
        #[cfg(target_os = "linux")]
        {
            // SAFETY: this runs once during backend construction at
            // daemon boot; no other thread reads these vars at that
            // point.
            std::env::set_var("PULSE_PROP_application.name", "spotuify");
            std::env::set_var("PULSE_PROP_application.icon_name", "spotuify");
            std::env::set_var("PULSE_PROP_stream.description", "Spotify (spotuify)");
        }

        let cache = Cache::new(
            Some(paths.creds.as_path()),
            Some(paths.volume.as_path()),
            paths.audio.as_deref(),
            paths.audio_size_mib,
        )
        .map_err(|err| PlayerError::Other(anyhow::anyhow!("librespot cache init: {err}")))?;
        let (tx, rx) = mpsc::unbounded_channel();
        let backend = Arc::new(Self {
            cache,
            events_tx: tx,
            state: Mutex::new(State::default()),
        });
        Ok((backend, UnboundedReceiverStream::new(rx)))
    }

    fn emit(&self, event: PlayerEvent) {
        let _ = self.events_tx.send(event);
    }

    /// Reference to the librespot cache, for tests + the OAuth flow
    /// that lives in librespot_oauth.
    pub fn cache(&self) -> &Cache {
        &self.cache
    }
}

#[async_trait]
impl PlayerBackend for EmbeddedBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Embedded
    }

    async fn register_device(&mut self, name: &str) -> PlayerResult<DeviceId> {
        // Phase 9.2 placeholder: cache is wired, OAuth + Spirc come in
        // 9.3. For now register_device emits a synthetic Ready so the
        // daemon's event flow + diagnostics work end-to-end while the
        // real session bring-up matures.
        warn!(
            "EmbeddedBackend::register_device is a Phase 9.2 placeholder; \
             real librespot session bring-up lands in 9.3"
        );
        self.state.lock().device_name = Some(name.to_string());
        let id = DeviceId::new(format!("embedded-pending-{name}"));
        self.emit(PlayerEvent::Ready {
            device_id: id.clone(),
            name: name.to_string(),
        });
        Ok(id)
    }

    async fn play_uri(&mut self, _uri: &str, _position_ms: u32) -> PlayerResult<()> {
        Err(PlayerError::Unsupported(
            "play_uri lands with Phase 9.3".to_string(),
        ))
    }

    async fn pause(&mut self) -> PlayerResult<()> {
        Err(PlayerError::Unsupported(
            "pause lands with Phase 9.3".to_string(),
        ))
    }

    async fn resume(&mut self) -> PlayerResult<()> {
        Err(PlayerError::Unsupported(
            "resume lands with Phase 9.3".to_string(),
        ))
    }

    async fn next(&mut self) -> PlayerResult<()> {
        Err(PlayerError::Unsupported(
            "next lands with Phase 9.3".to_string(),
        ))
    }

    async fn previous(&mut self) -> PlayerResult<()> {
        Err(PlayerError::Unsupported(
            "previous lands with Phase 9.3".to_string(),
        ))
    }

    async fn seek(&mut self, _position_ms: u32) -> PlayerResult<()> {
        Err(PlayerError::Unsupported(
            "seek lands with Phase 9.3".to_string(),
        ))
    }

    async fn volume(&mut self, _percent: u8) -> PlayerResult<()> {
        Err(PlayerError::Unsupported(
            "volume lands with Phase 9.3".to_string(),
        ))
    }

    async fn shuffle(&mut self, _on: bool) -> PlayerResult<()> {
        Err(PlayerError::Unsupported(
            "shuffle lands with Phase 9.3".to_string(),
        ))
    }

    async fn repeat(&mut self, _mode: RepeatMode) -> PlayerResult<()> {
        Err(PlayerError::Unsupported(
            "repeat lands with Phase 9.3".to_string(),
        ))
    }

    async fn is_connected(&self) -> bool {
        self.state.lock().device_name.is_some()
    }

    async fn shutdown(&mut self) -> PlayerResult<()> {
        self.state.lock().device_name = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::EmbeddedCachePaths;
    use std::path::PathBuf;

    #[test]
    fn cache_paths_under_disabled_audio_returns_none() {
        // Adversarial: audio_cache_mib=0 must NOT create an audio
        // cache dir — librespot writes scratch frames there and a
        // user who explicitly opted out shouldn't see surprise GiBs
        // on disk.
        let paths = EmbeddedCachePaths::under(PathBuf::from("/tmp/test"), 0);
        assert!(paths.audio.is_none());
        assert!(paths.audio_size_mib.is_none());
    }

    #[test]
    fn cache_paths_under_enabled_audio_returns_path_and_size() {
        let paths = EmbeddedCachePaths::under(PathBuf::from("/tmp/test"), 256);
        assert_eq!(
            paths.audio,
            Some(PathBuf::from("/tmp/test/librespot/audio"))
        );
        assert_eq!(paths.audio_size_mib, Some(256));
    }

    #[test]
    fn cache_paths_layout_matches_phase_9_doc() {
        let paths = EmbeddedCachePaths::under(PathBuf::from("/u/.cache/spotuify"), 128);
        assert_eq!(
            paths.creds,
            PathBuf::from("/u/.cache/spotuify/librespot/creds")
        );
        assert_eq!(
            paths.volume,
            PathBuf::from("/u/.cache/spotuify/librespot/volume")
        );
    }
}
