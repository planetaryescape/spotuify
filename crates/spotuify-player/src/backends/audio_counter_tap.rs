//! Phase 10 (F10) — `AudioCounterTap` sink wrapper.
//!
//! Sits between librespot's audio output and `RecoveringSink`, counting
//! every PCM frame that lands in the inner sink. The daemon's
//! `SessionTracker` reads the counter at track-finalisation time to
//! compute `audible_ms` accurately — buffer drops, AirPods-disconnect
//! gaps, and pause intervals all show up as fewer frames written even
//! when the wall clock kept ticking.
//!
//! Chain order in `EmbeddedBackend`:
//!
//! ```text
//! librespot Sink → AudioCounterTap → RecoveringSink → physical backend
//! ```
//!
//! `AudioCounterTap` is **inside** the recovering wrapper so a panic
//! in the underlying physical sink doesn't lose the counter `Arc` —
//! the handle outlives any sink reconstruction.
//!
//! Backends that can't expose a tap (spotifyd, ConnectOnly — they don't
//! own the PCM stream) return `None` from `PlayerBackend::audio_counter`
//! and the SessionTracker transparently falls back to wall-clock
//! derivation.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use crate::backends::recovering_sink::{Sink, SinkError};

/// Thread-safe handle into the running counter. Cloneable; the
/// `SessionTracker` keeps one and the sink keeps another.
///
/// `samples` counts individual PCM **samples** written (NOT stereo
/// frame pairs). Audio is stereo so a stereo frame = 2 samples.
/// `audible_ms()` divides by `sample_rate` to convert.
#[derive(Debug, Default)]
pub struct AudioCounterHandle {
    samples: AtomicU64,
    sample_rate: AtomicU32,
    /// Channel count. Defaults to 2 (stereo). Backends emitting mono
    /// can update it at start time.
    channels: AtomicU32,
}

impl AudioCounterHandle {
    /// Construct with default sample rate (44.1kHz) and channel count
    /// (stereo). The embedded backend writes the real values on
    /// `start()` via `set_format`.
    pub fn new() -> Arc<Self> {
        let handle = Self::default();
        handle.sample_rate.store(44_100, Ordering::Relaxed);
        handle.channels.store(2, Ordering::Relaxed);
        Arc::new(handle)
    }

    /// Reset the counter. Called at every `start()` so each playback
    /// session has its own zero baseline.
    pub fn reset(&self) {
        self.samples.store(0, Ordering::Relaxed);
    }

    /// Update the format hint. No-op if the backend never calls it —
    /// the defaults (44.1kHz stereo) are usually correct.
    pub fn set_format(&self, sample_rate: u32, channels: u32) {
        self.sample_rate.store(sample_rate, Ordering::Relaxed);
        self.channels.store(channels.max(1), Ordering::Relaxed);
    }

    /// Samples written so far (since the most recent `reset`).
    pub fn samples(&self) -> u64 {
        self.samples.load(Ordering::Relaxed)
    }

    /// Audible time, in milliseconds, derived from the sample count
    /// and the current format. Stereo frames are reduced to per-channel
    /// duration by dividing by `channels`.
    pub fn audible_ms(&self) -> u64 {
        let samples = self.samples.load(Ordering::Relaxed);
        let rate = self.sample_rate.load(Ordering::Relaxed) as u64;
        let channels = self.channels.load(Ordering::Relaxed).max(1) as u64;
        if rate == 0 {
            return 0;
        }
        (samples * 1_000) / (rate * channels)
    }

    /// Internal: called by the tap on every `write()`.
    pub(crate) fn add_samples(&self, count: u64) {
        self.samples.fetch_add(count, Ordering::Relaxed);
    }
}

/// Sink wrapper that counts PCM samples then delegates to the inner
/// sink. Generic over the inner `Sink` so tests can plug a recording
/// double in without real audio hardware.
pub struct AudioCounterTap<S: Sink> {
    inner: S,
    handle: Arc<AudioCounterHandle>,
}

impl<S: Sink> AudioCounterTap<S> {
    pub fn new(inner: S, handle: Arc<AudioCounterHandle>) -> Self {
        Self { inner, handle }
    }

    /// Shared handle for the SessionTracker to read.
    pub fn handle(&self) -> Arc<AudioCounterHandle> {
        self.handle.clone()
    }
}

impl<S: Sink> Sink for AudioCounterTap<S> {
    fn start(&mut self) -> Result<(), SinkError> {
        // Reset the counter at every (re)start so per-track audible_ms
        // is correctly bounded — finalize captures the value once at
        // session end.
        self.handle.reset();
        self.inner.start()
    }

    fn stop(&mut self) -> Result<(), SinkError> {
        // Keep the counter intact through stop — SessionTracker may
        // read it after the underlying backend was already stopped.
        self.inner.stop()
    }

    fn write(&mut self, frames: &[i16]) -> Result<(), SinkError> {
        self.handle.add_samples(frames.len() as u64);
        self.inner.write(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Recording double — captures the byte stream + serves errors on
    /// demand without touching real audio hardware.
    struct Recorder {
        written: Mutex<Vec<i16>>,
        start_calls: AtomicU32,
        stop_calls: AtomicU32,
    }

    impl Recorder {
        fn new() -> Self {
            Self {
                written: Mutex::new(Vec::new()),
                start_calls: AtomicU32::new(0),
                stop_calls: AtomicU32::new(0),
            }
        }
    }

    impl Sink for Recorder {
        fn start(&mut self) -> Result<(), SinkError> {
            self.start_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        fn stop(&mut self) -> Result<(), SinkError> {
            self.stop_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        fn write(&mut self, frames: &[i16]) -> Result<(), SinkError> {
            self.written
                .lock()
                .expect("recorder mutex should not be poisoned")
                .extend_from_slice(frames);
            Ok(())
        }
    }

    #[test]
    fn counter_advances_by_exact_sample_count() {
        let handle = AudioCounterHandle::new();
        let mut tap = AudioCounterTap::new(Recorder::new(), handle.clone());
        tap.write(&[0_i16; 2048]).expect("write should succeed");
        assert_eq!(handle.samples(), 2048);
        tap.write(&[0_i16; 512]).expect("write should succeed");
        assert_eq!(handle.samples(), 2560);
    }

    #[test]
    fn audible_ms_uses_44100_stereo_by_default() {
        let handle = AudioCounterHandle::new();
        // One second of stereo 44.1kHz = 44100 frames = 88200 samples.
        let mut tap = AudioCounterTap::new(Recorder::new(), handle.clone());
        tap.write(&vec![0_i16; 88_200])
            .expect("write should succeed");
        assert_eq!(handle.audible_ms(), 1_000);
    }

    #[test]
    fn audible_ms_respects_custom_format() {
        let handle = AudioCounterHandle::new();
        handle.set_format(48_000, 2);
        let mut tap = AudioCounterTap::new(Recorder::new(), handle.clone());
        // One second of stereo 48kHz = 96000 samples.
        tap.write(&vec![0_i16; 96_000])
            .expect("write should succeed");
        assert_eq!(handle.audible_ms(), 1_000);
    }

    #[test]
    fn start_resets_the_counter_but_stop_does_not() {
        let handle = AudioCounterHandle::new();
        let mut tap = AudioCounterTap::new(Recorder::new(), handle.clone());
        tap.write(&[0_i16; 100]).expect("write should succeed");
        assert_eq!(handle.samples(), 100);
        tap.stop().expect("stop should succeed");
        assert_eq!(
            handle.samples(),
            100,
            "stop must preserve the counter for finalize"
        );
        tap.start().expect("start should succeed");
        assert_eq!(handle.samples(), 0, "start must reset the counter");
    }

    #[test]
    fn write_passes_frames_through_to_inner_sink() {
        let handle = AudioCounterHandle::new();
        let recorder = Recorder::new();
        let mut tap = AudioCounterTap::new(recorder, handle.clone());
        tap.write(&[1, 2, 3, 4]).expect("write should succeed");
        let inner_bytes = {
            let writer: &Recorder = &tap.inner;
            writer
                .written
                .lock()
                .expect("recorder mutex should not be poisoned")
                .clone()
        };
        assert_eq!(inner_bytes, vec![1, 2, 3, 4]);
        assert_eq!(handle.samples(), 4);
    }

    #[test]
    fn zero_sample_rate_does_not_panic() {
        let handle = AudioCounterHandle::new();
        handle.set_format(0, 2);
        assert_eq!(handle.audible_ms(), 0);
    }
}
