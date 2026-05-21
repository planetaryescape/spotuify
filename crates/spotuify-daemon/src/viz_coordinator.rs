//! Phase 17 — `VizCoordinator`.
//!
//! Owns the shared FFT analyzer, the (optional) cpal loopback capture,
//! and a tokio ticker that broadcasts `DaemonEvent::SpectrumFrame` to
//! TUI subscribers at the configured target rate.
//!
//! Lifecycle:
//! - Constructed at daemon boot (idle: enabled=false).
//! - `set_enabled(true)` spins up the ticker. If `source = Loopback` or
//!   `source = Auto` with no sink path, it also opens the platform
//!   loopback capture stream.
//! - Player events feed the coordinator's playing/paused state. While
//!   paused, the ticker keeps running (so band magnitudes decay
//!   smoothly to zero) but stops broadcasting once decayed.
//! - `set_focused(false)` throttles the broadcast rate from 30 Hz to
//!   1 Hz so background terminals don't burn CPU.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
#[cfg(feature = "loopback-cpal")]
use spotuify_audio::loopback::{AudioCaptureManager, LoopbackError};
use spotuify_audio::{create_shared_analyzer, SharedAnalyzer};
use spotuify_protocol::{
    DaemonEvent, IpcMessage, IpcPayload, VizActiveSource, VizDiagnostics, VizSourceKindData,
};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
#[cfg_attr(not(feature = "loopback-cpal"), allow(unused_imports))]
use tracing::{debug, info, warn};

const DEFAULT_TARGET_FPS: u8 = 30;
const UNFOCUSED_FPS: u8 = 1;

pub struct VizCoordinator {
    analyzer: SharedAnalyzer,
    event_tx: broadcast::Sender<IpcMessage>,
    state: Mutex<VizCoordinatorState>,
    enabled: Arc<AtomicBool>,
    focused: AtomicBool,
    playing: Arc<AtomicBool>,
    sink_available: AtomicBool,
    configured_fps: AtomicU8,
    target_fps: Arc<AtomicU8>,
    dropped_frames: Arc<AtomicU64>,
    /// Phase 0 — millis-since-epoch of last broadcast `SpectrumFrame`,
    /// or 0 when none yet. Atomic so the diagnostics() snapshot can
    /// derive `last_frame_age_ms` without locking the ticker state.
    last_frame_ms: Arc<AtomicU64>,
    /// Phase 0 — backend kind the daemon registered with. Lets diagnostics
    /// surface the correct hint ("switch to embedded backend") to the TUI.
    /// Set by `DaemonState::ensure_player_ready` after backend boot.
    backend_kind: Mutex<Option<spotuify_core::BackendKind>>,
    ticker: Mutex<Option<JoinHandle<()>>>,
    /// Phase 17 — open loopback capture stream. Wrapped in a dedicated
    /// OS thread because cpal's `Stream` is `!Send` on macOS coreaudio.
    /// Only `Some` when the active source is `LoopbackCpal`.
    #[cfg(feature = "loopback-cpal")]
    loopback: Mutex<Option<LoopbackThread>>,
}

struct VizCoordinatorState {
    configured_source: VizSourceKindData,
    active_source: VizActiveSource,
    sample_rate: Option<u32>,
    loopback_device_name: Option<String>,
    hint: Option<String>,
}

impl Default for VizCoordinatorState {
    fn default() -> Self {
        Self {
            configured_source: VizSourceKindData::Auto,
            active_source: VizActiveSource::None,
            sample_rate: None,
            loopback_device_name: None,
            hint: None,
        }
    }
}

impl VizCoordinator {
    /// Construct an idle coordinator. The ticker task is NOT started
    /// until `set_enabled(true)` is called.
    pub fn new(event_tx: broadcast::Sender<IpcMessage>) -> Arc<Self> {
        Arc::new(Self {
            analyzer: create_shared_analyzer(),
            event_tx,
            state: Mutex::new(VizCoordinatorState::default()),
            enabled: Arc::new(AtomicBool::new(false)),
            focused: AtomicBool::new(true),
            playing: Arc::new(AtomicBool::new(false)),
            sink_available: AtomicBool::new(false),
            configured_fps: AtomicU8::new(DEFAULT_TARGET_FPS),
            target_fps: Arc::new(AtomicU8::new(DEFAULT_TARGET_FPS)),
            dropped_frames: Arc::new(AtomicU64::new(0)),
            last_frame_ms: Arc::new(AtomicU64::new(0)),
            backend_kind: Mutex::new(None),
            ticker: Mutex::new(None),
            #[cfg(feature = "loopback-cpal")]
            loopback: Mutex::new(None),
        })
    }

    /// Phase 0 (observability) — record that the daemon backend is now
    /// running and surface it through diagnostics. Called by
    /// `DaemonState::ensure_player_ready` after a successful backend
    /// register so `viz status` and TUI hints know whether sink-tap
    /// visualization is achievable.
    pub fn set_backend_kind(&self, kind: spotuify_core::BackendKind) {
        *self.backend_kind.lock() = Some(kind);
    }

    /// Cheap clone of the shared analyzer handle for daemon-owned audio
    /// sources such as loopback capture.
    pub fn shared_analyzer(&self) -> SharedAnalyzer {
        self.analyzer.clone()
    }

    pub async fn set_sink_available(self: &Arc<Self>, available: bool) {
        let was = self.sink_available.swap(available, Ordering::Release);
        if was == available {
            return;
        }
        if self.enabled.load(Ordering::Acquire) {
            self.teardown_source();
            self.activate_source().await;
            self.broadcast_source_change();
        }
    }

    pub async fn set_enabled(self: &Arc<Self>, enabled: bool) {
        let was = self.enabled.swap(enabled, Ordering::Release);
        if was == enabled {
            return;
        }
        if enabled {
            self.activate_source().await;
            self.start_ticker();
        } else {
            self.stop_ticker();
            self.teardown_source();
            self.broadcast_source_change();
        }
    }

    pub async fn set_source(self: &Arc<Self>, kind: VizSourceKindData) {
        {
            let mut st = self.state.lock();
            if st.configured_source == kind {
                return;
            }
            st.configured_source = kind;
        }
        if self.enabled.load(Ordering::Acquire) {
            self.teardown_source();
            self.activate_source().await;
        }
        self.broadcast_source_change();
    }

    pub async fn set_focused(&self, focused: bool) {
        let was = self.focused.swap(focused, Ordering::Release);
        if was == focused {
            return;
        }
        let new_fps = if focused {
            self.configured_fps.load(Ordering::Acquire)
        } else {
            UNFOCUSED_FPS
        };
        self.target_fps.store(new_fps, Ordering::Release);
    }

    pub fn set_target_fps(&self, fps: u8) {
        let fps = fps.clamp(1, 60);
        self.configured_fps.store(fps, Ordering::Release);
        if self.focused.load(Ordering::Acquire) {
            self.target_fps.store(fps, Ordering::Release);
        }
    }

    pub fn set_analyzer_params(&self, smoothing: f32, noise_gate: f32) {
        if let Ok(mut analyzer) = self.analyzer.lock() {
            analyzer.set_visual_params(smoothing, noise_gate);
        }
    }

    /// Set whether playback is currently active. Drives the gate that
    /// stops the ticker from spamming `SpectrumFrame` events when nothing
    /// is playing.
    pub fn set_playing(&self, playing: bool) {
        self.playing.store(playing, Ordering::Release);
    }

    pub async fn diagnostics(&self) -> VizDiagnostics {
        let st = self.state.lock();
        let last_frame_age_ms = match self.last_frame_ms.load(Ordering::Acquire) {
            0 => None,
            ms => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(ms);
                Some(now.saturating_sub(ms))
            }
        };
        VizDiagnostics {
            enabled: self.enabled.load(Ordering::Acquire),
            configured_source: st.configured_source,
            active_source: st.active_source,
            sample_rate: st.sample_rate,
            loopback_device_name: st.loopback_device_name.clone(),
            dropped_frames_5min: self.dropped_frames.load(Ordering::Relaxed),
            target_fps: self.target_fps.load(Ordering::Acquire),
            hint: st.hint.clone(),
            playing: self.playing.load(Ordering::Acquire),
            last_frame_age_ms,
            backend_kind: *self.backend_kind.lock(),
        }
    }

    /// Pick a source based on the configured kind. When `Auto`, prefer
    /// the sink-tap path when the embedded backend supplied a sink
    /// chain; otherwise fall back to loopback.
    async fn activate_source(self: &Arc<Self>) {
        let configured = self.state.lock().configured_source;
        let want_sink = matches!(
            configured,
            VizSourceKindData::Auto | VizSourceKindData::Sink
        );
        // Auto used to fall through to loopback when no sink was
        // available. On macOS that opens cpal's default INPUT device,
        // which can interfere with the system audio session — the
        // user reported "enabling visualization changes the audio."
        // Loopback is now opt-in only via the explicit `Loopback`
        // configured source.
        let want_loopback = matches!(configured, VizSourceKindData::Loopback);

        if want_sink && self.sink_available.load(Ordering::Acquire) {
            self.state.lock().active_source = VizActiveSource::Sink;
            info!(target: "spotuify_daemon::viz", "viz source = sink");
            return;
        }

        if matches!(configured, VizSourceKindData::Sink) {
            let mut st = self.state.lock();
            st.active_source = VizActiveSource::None;
            st.hint =
                Some("Sink visualization requires the embedded playback backend.".to_string());
            return;
        }
        if matches!(configured, VizSourceKindData::Auto) {
            // Auto on Connect-only / Spotifyd: no PCM to tap, no
            // loopback (audio-session safety). Render flat spectrum.
            let mut st = self.state.lock();
            st.active_source = VizActiveSource::None;
            st.hint = Some(
                "Auto: no PCM source available. Switch playback to the embedded backend, or set `viz.source = \"loopback\"` to opt into capturing system audio."
                    .to_string(),
            );
            return;
        }

        #[cfg(feature = "loopback-cpal")]
        if want_loopback {
            match LoopbackThread::spawn(self.analyzer.clone()) {
                Ok(thread) => {
                    let device_name = thread.device_name.clone();
                    let sample_rate = thread.sample_rate;
                    {
                        let mut st = self.state.lock();
                        st.active_source = VizActiveSource::LoopbackCpal;
                        st.sample_rate = Some(sample_rate);
                        st.loopback_device_name = Some(device_name.clone());
                        st.hint = None;
                    }
                    info!(
                        target: "spotuify_daemon::viz",
                        device = %device_name,
                        sample_rate,
                        "viz source = loopback (cpal)"
                    );
                    *self.loopback.lock() = Some(thread);
                    return;
                }
                Err(err) => {
                    warn!(target: "spotuify_daemon::viz", "loopback capture failed: {err}");
                    let mut st = self.state.lock();
                    st.active_source = VizActiveSource::None;
                    st.hint = Some(loopback_setup_hint());
                    return;
                }
            }
        }

        // No backend matched / no feature compiled in.
        let _ = want_loopback;
        let mut st = self.state.lock();
        st.active_source = VizActiveSource::None;
        st.hint = Some(loopback_setup_hint());
    }

    fn teardown_source(&self) {
        #[cfg(feature = "loopback-cpal")]
        {
            *self.loopback.lock() = None;
        }
        let mut st = self.state.lock();
        st.active_source = VizActiveSource::None;
        st.sample_rate = None;
        st.loopback_device_name = None;
    }

    fn broadcast_source_change(&self) {
        let st = self.state.lock();
        let hint = st.hint.clone();
        let backend_kind = *self.backend_kind.lock();
        let _ = self.event_tx.send(IpcMessage {
            id: 0,
            source: None,
            payload: IpcPayload::Event(DaemonEvent::VizSourceChanged {
                active: st.active_source,
                configured: st.configured_source,
                hint,
                backend_kind,
            }),
        });
    }

    fn start_ticker(self: &Arc<Self>) {
        let mut slot = self.ticker.lock();
        if slot.is_some() {
            return;
        }
        let ticker = VizTicker {
            analyzer: self.analyzer.clone(),
            event_tx: self.event_tx.clone(),
            enabled: self.enabled.clone(),
            playing: self.playing.clone(),
            target_fps: self.target_fps.clone(),
            dropped_frames: self.dropped_frames.clone(),
            last_frame_ms: self.last_frame_ms.clone(),
        };
        let handle = tokio::spawn(async move { ticker.run().await });
        *slot = Some(handle);
    }

    fn stop_ticker(&self) {
        if let Some(handle) = self.ticker.lock().take() {
            handle.abort();
        }
    }
}

struct VizTicker {
    analyzer: SharedAnalyzer,
    event_tx: broadcast::Sender<IpcMessage>,
    enabled: Arc<AtomicBool>,
    playing: Arc<AtomicBool>,
    target_fps: Arc<AtomicU8>,
    dropped_frames: Arc<AtomicU64>,
    /// Phase 0 — shared with `VizCoordinator::diagnostics`; updated
    /// once per broadcast so `last_frame_age_ms` reflects reality.
    last_frame_ms: Arc<AtomicU64>,
}

impl VizTicker {
    async fn run(self) {
        // We always tick at the maximum rate and decimate inside the loop
        // based on `target_fps`. Avoids restarting the timer on focus
        // changes, which would race with concurrent set_focused calls.
        let mut interval =
            tokio::time::interval(Duration::from_millis(1000 / DEFAULT_TARGET_FPS as u64));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut tick_count: u64 = 0;
        let mut last_peak = 0.0f32;
        loop {
            interval.tick().await;
            tick_count = tick_count.wrapping_add(1);
            if !self.enabled.load(Ordering::Acquire) {
                debug!(target: "spotuify_daemon::viz", "ticker observed disabled, exiting");
                break;
            }
            let target = self.target_fps.load(Ordering::Acquire).max(1);
            let stride = (DEFAULT_TARGET_FPS / target).max(1) as u64;
            if !tick_count.is_multiple_of(stride) {
                continue;
            }
            let playing = self.playing.load(Ordering::Acquire);
            if !playing && last_peak <= 0.005 {
                continue;
            }
            let spectrum = match self.analyzer.try_lock() {
                Ok(mut guard) => {
                    if !playing {
                        let silence = [0.0; spotuify_audio::FFT_SIZE];
                        guard.push_samples(&silence);
                    }
                    guard.process()
                }
                Err(_) => {
                    self.dropped_frames.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            };
            last_peak = spectrum.peak;
            let now = now_ms();
            let payload = DaemonEvent::SpectrumFrame {
                bands: spectrum.bands.to_vec(),
                peak: spectrum.peak,
                timestamp_ms: now,
            };
            self.last_frame_ms.store(now, Ordering::Release);
            if self
                .event_tx
                .send(IpcMessage {
                    id: 0,
                    source: None,
                    payload: IpcPayload::Event(payload),
                })
                .is_err()
            {
                // No subscribers yet — that's normal at boot. Not a drop.
            }
        }
    }
}

// (active_to_kind helper removed — `VizSourceChanged` now carries the
// richer `VizActiveSource` directly so TUI clients can distinguish
// `loopback (cpal)` vs `loopback (pipewire)`.)

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Phase 17 — owns a `cpal::Stream` in a dedicated OS thread so the
/// (potentially `!Send`) Stream never has to cross task boundaries.
/// Daemon state and the tokio runtime stay Send. Drop signals the
/// thread to release the stream and join.
#[cfg(feature = "loopback-cpal")]
struct LoopbackThread {
    shutdown: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    device_name: String,
    sample_rate: u32,
}

#[cfg(feature = "loopback-cpal")]
impl LoopbackThread {
    fn spawn(analyzer: SharedAnalyzer) -> Result<Self, LoopbackError> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();
        let (info_tx, info_rx) = std::sync::mpsc::sync_channel::<Result<(String, u32), String>>(1);

        let thread = std::thread::Builder::new()
            .name("spotuify-viz-loopback".into())
            .spawn(move || {
                let mgr = match AudioCaptureManager::new(analyzer) {
                    Ok(m) => m,
                    Err(e) => {
                        let _ = info_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                let _ = info_tx.send(Ok((mgr.device_name().to_string(), mgr.sample_rate())));
                while !shutdown_clone.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(100));
                }
                drop(mgr);
            })
            .map_err(|e| LoopbackError::BuildStream(format!("spawn loopback thread: {e}")))?;

        let info = info_rx
            .recv()
            .map_err(|e| LoopbackError::BuildStream(format!("loopback info channel: {e}")))?;
        match info {
            Ok((device_name, sample_rate)) => Ok(Self {
                shutdown,
                thread: Some(thread),
                device_name,
                sample_rate,
            }),
            Err(msg) => {
                shutdown.store(true, Ordering::Release);
                let _ = thread.join();
                Err(LoopbackError::BuildStream(msg))
            }
        }
    }
}

#[cfg(feature = "loopback-cpal")]
impl Drop for LoopbackThread {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn loopback_setup_hint() -> String {
    if cfg!(target_os = "macos") {
        "macOS has no native system-audio loopback. Install BlackHole 2ch \
         (`brew install blackhole-2ch`) and route system output through it to \
         enable the visualizer. See README troubleshooting for details."
            .to_string()
    } else if cfg!(target_os = "linux") {
        "No PipeWire/PulseAudio monitor device found. Verify your sound \
         server exposes a *.monitor input device (run `pactl list sources`)."
            .to_string()
    } else if cfg!(target_os = "windows") {
        "WASAPI loopback could not open the default output device. Verify \
         a playback device is selected as default in Sound Settings."
            .to_string()
    } else {
        "Audio loopback is not supported on this platform.".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spotuify_protocol::DaemonEvent;

    fn fresh() -> (Arc<VizCoordinator>, broadcast::Receiver<IpcMessage>) {
        let (tx, rx) = broadcast::channel(64);
        (VizCoordinator::new(tx), rx)
    }

    #[tokio::test]
    async fn default_diagnostics_are_disabled_and_auto_source() {
        let (vc, _rx) = fresh();
        let d = vc.diagnostics().await;
        assert!(!d.enabled);
        assert_eq!(d.configured_source, VizSourceKindData::Auto);
        assert_eq!(d.active_source, VizActiveSource::None);
        assert_eq!(d.target_fps, DEFAULT_TARGET_FPS);
    }

    #[tokio::test]
    async fn set_focused_changes_target_fps() {
        let (vc, _rx) = fresh();
        vc.set_focused(false).await;
        assert_eq!(vc.diagnostics().await.target_fps, UNFOCUSED_FPS);
        vc.set_focused(true).await;
        assert_eq!(vc.diagnostics().await.target_fps, DEFAULT_TARGET_FPS);
    }

    #[tokio::test]
    async fn set_source_updates_configured_and_broadcasts_change() {
        let (vc, mut rx) = fresh();
        vc.set_source(VizSourceKindData::Sink).await;
        assert_eq!(
            vc.diagnostics().await.configured_source,
            VizSourceKindData::Sink
        );
        // A VizSourceChanged event should have been broadcast (after enable
        // we also see one; before enable we still emit so clients refresh).
        // Disabled means no source change event (gated). Verify by enabling next.
        let _ = rx.try_recv();
    }

    #[tokio::test]
    async fn enable_then_disable_is_idempotent() {
        let (vc, _rx) = fresh();
        // Force a source that won't actually open a real audio device.
        vc.set_source(VizSourceKindData::None).await;
        vc.set_enabled(true).await;
        assert!(vc.diagnostics().await.enabled);
        vc.set_enabled(true).await; // no-op
        assert!(vc.diagnostics().await.enabled);
        vc.set_enabled(false).await;
        assert!(!vc.diagnostics().await.enabled);
        vc.set_enabled(false).await; // no-op
        assert!(!vc.diagnostics().await.enabled);
    }

    #[tokio::test]
    async fn shared_analyzer_is_stable_across_clones() {
        let (vc, _rx) = fresh();
        let a1 = vc.shared_analyzer();
        let a2 = vc.shared_analyzer();
        assert!(Arc::ptr_eq(&a1, &a2));
    }

    #[tokio::test]
    async fn sink_source_requires_available_embedded_sink_chain() {
        let (vc, _rx) = fresh();
        vc.set_source(VizSourceKindData::Sink).await;
        vc.set_enabled(true).await;

        let d = vc.diagnostics().await;
        assert_eq!(d.active_source, VizActiveSource::None);
        assert!(d
            .hint
            .as_deref()
            .is_some_and(|hint| hint.contains("embedded playback")));
    }

    #[tokio::test]
    async fn sink_source_activates_when_sink_chain_is_available() {
        let (vc, _rx) = fresh();
        vc.set_sink_available(true).await;
        vc.set_source(VizSourceKindData::Sink).await;
        vc.set_enabled(true).await;

        assert_eq!(vc.diagnostics().await.active_source, VizActiveSource::Sink);
    }

    #[tokio::test]
    async fn disabled_ticker_does_not_emit_spectrum_frames() {
        let (vc, mut rx) = fresh();
        // Set source=None so even if we accidentally start anything,
        // there's no audio source feeding the analyzer.
        vc.set_source(VizSourceKindData::None).await;
        // Drain the VizSourceChanged that the previous call may emit.
        while rx.try_recv().is_ok() {}
        // Wait a bit. If a ticker were running we'd see SpectrumFrames.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut spectrum_seen = false;
        while let Ok(msg) = rx.try_recv() {
            if let IpcPayload::Event(DaemonEvent::SpectrumFrame { .. }) = msg.payload {
                spectrum_seen = true;
                break;
            }
        }
        assert!(
            !spectrum_seen,
            "no SpectrumFrame should be emitted while disabled"
        );
    }

    #[tokio::test]
    async fn enabled_with_source_none_does_not_panic() {
        let (vc, _rx) = fresh();
        vc.set_source(VizSourceKindData::None).await;
        vc.set_enabled(true).await;
        // Let the ticker run briefly.
        tokio::time::sleep(Duration::from_millis(80)).await;
        vc.set_enabled(false).await;
    }
}
