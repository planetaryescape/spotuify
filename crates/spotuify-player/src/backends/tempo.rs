//! Pitch-preserving playback-rate stage for the embedded sink chain.
//!
//! librespot cannot change playback speed: its sample rate is a compile-time
//! 44.1 kHz constant, `PlayerConfig`/`Spirc` expose no rate control, and the
//! Connect protocol's `playback_speed` is only mirrored back as 0/1. The one
//! place every decoded sample passes through is our own `Sink` wrapper, so the
//! time-stretch lives there: for each input buffer at rate `r` we ask
//! Signalsmith Stretch for `len / r` output samples, pitch unchanged.
//!
//! Because librespot's player thread is paced by the sink (writes block on
//! the physical backend's buffer), the decoder naturally runs `r`× faster and
//! its own position timestamps stay content-accurate — no bookkeeping here.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// The workspace denies `unsafe_code`; this module is the single, audited
/// exception: a C ABI over the vendored header-only Signalsmith Stretch
/// (`vendor/signalsmith-stretch/shim.cpp`, built by `build.rs`). Every
/// pointer handed across is owned by [`Stretch`] or borrowed from slices
/// whose lengths are checked before the call.
#[allow(unsafe_code)]
mod stretch_ffi {
    use std::ptr::NonNull;

    mod ffi {
        #[repr(C)]
        pub struct Stretch {
            _private: [u8; 0],
        }

        extern "C" {
            pub fn spotuify_stretch_new(channels: i32, sample_rate: f32) -> *mut Stretch;
            pub fn spotuify_stretch_free(stretch: *mut Stretch);
            pub fn spotuify_stretch_reset(stretch: *mut Stretch);
            pub fn spotuify_stretch_process(
                stretch: *mut Stretch,
                inputs: *const *const f32,
                input_samples: i32,
                outputs: *const *mut f32,
                output_samples: i32,
            );
        }
    }

    /// Owning handle to one Signalsmith Stretch engine (planar f32 in/out).
    pub(super) struct Stretch {
        raw: NonNull<ffi::Stretch>,
        channels: usize,
    }

    // SAFETY: the engine has no thread affinity or global state; it is only
    // ever driven from the sink's audio thread and moves with it.
    unsafe impl Send for Stretch {}

    impl Stretch {
        pub(super) fn new(channels: usize, sample_rate: u32) -> Option<Self> {
            // SAFETY: plain constructor; a null return means allocation failed.
            let raw = unsafe { ffi::spotuify_stretch_new(channels as i32, sample_rate as f32) };
            NonNull::new(raw).map(|raw| Self { raw, channels })
        }

        pub(super) fn reset(&mut self) {
            // SAFETY: `raw` is a live engine owned by this handle.
            unsafe { ffi::spotuify_stretch_reset(self.raw.as_ptr()) }
        }

        /// The ratio `in_frames / out_frames` is the stretch factor.
        pub(super) fn process(
            &mut self,
            inputs: &[Vec<f32>],
            in_frames: usize,
            outputs: &mut [Vec<f32>],
            out_frames: usize,
        ) {
            assert_eq!(inputs.len(), self.channels, "input channel count");
            assert_eq!(outputs.len(), self.channels, "output channel count");
            assert!(
                inputs.iter().all(|ch| ch.len() >= in_frames),
                "input buffers shorter than in_frames"
            );
            assert!(
                outputs.iter().all(|ch| ch.len() >= out_frames),
                "output buffers shorter than out_frames"
            );
            let input_ptrs: Vec<*const f32> = inputs.iter().map(|ch| ch.as_ptr()).collect();
            let output_ptrs: Vec<*mut f32> = outputs.iter_mut().map(|ch| ch.as_mut_ptr()).collect();
            // SAFETY: both pointer arrays hold exactly `channels` entries and
            // every buffer is at least as long as the sample count passed
            // (asserted above); the engine only reads/writes within those.
            unsafe {
                ffi::spotuify_stretch_process(
                    self.raw.as_ptr(),
                    input_ptrs.as_ptr(),
                    in_frames as i32,
                    output_ptrs.as_ptr(),
                    out_frames as i32,
                );
            }
        }
    }

    impl Drop for Stretch {
        fn drop(&mut self) {
            // SAFETY: `raw` came from `spotuify_stretch_new` and is freed once.
            unsafe { ffi::spotuify_stretch_free(self.raw.as_ptr()) }
        }
    }
}

use stretch_ffi::Stretch;

/// Spotify's podcast speed range.
pub const MIN_PLAYBACK_SPEED: f32 = 0.5;
pub const MAX_PLAYBACK_SPEED: f32 = 3.5;

/// Clamp to the supported range and snap to 0.05 steps so `1.2500001`
/// from a slider never produces a distinct, un-displayable rate.
pub fn normalize_playback_speed(speed: f32) -> f32 {
    let clamped = speed.clamp(MIN_PLAYBACK_SPEED, MAX_PLAYBACK_SPEED);
    (clamped * 20.0).round() / 20.0
}

/// A rate shared lock-free between the backend (writer) and the sink's
/// audio thread (reader). Stored as `f32` bits.
#[derive(Clone, Debug)]
pub struct SharedRate(Arc<AtomicU32>);

impl Default for SharedRate {
    fn default() -> Self {
        Self::new(1.0)
    }
}

impl SharedRate {
    pub fn new(rate: f32) -> Self {
        Self(Arc::new(AtomicU32::new(rate.to_bits())))
    }

    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }

    pub fn set(&self, rate: f32) {
        self.0.store(rate.to_bits(), Ordering::Relaxed);
    }
}

/// Whether a rate is close enough to 1.0 that stretching is pure cost.
pub fn is_unity(rate: f32) -> bool {
    (rate - 1.0).abs() < 0.005
}

/// Stateful stretch engine for one stereo stream. Created lazily on the
/// first non-unity buffer and dropped when the rate returns to 1.0, so the
/// common music case carries no DSP at all.
pub struct TempoStage {
    channels: usize,
    sample_rate: u32,
    stretch: Option<Stretch>,
    /// Planar scratch buffers (one per channel), reused across calls.
    planar_in: Vec<Vec<f32>>,
    planar_out: Vec<Vec<f32>>,
    /// Fractional output frames carried between buffers so `in / rate`
    /// rounding never accumulates into drift.
    carry: f64,
}

impl TempoStage {
    pub fn new(channels: usize, sample_rate: u32) -> Self {
        Self {
            channels,
            sample_rate,
            stretch: None,
            planar_in: vec![Vec::new(); channels],
            planar_out: vec![Vec::new(); channels],
            carry: 0.0,
        }
    }

    /// Drop engine state (seek / stop / track change). The next non-unity
    /// buffer rebuilds it, which costs one block of latency, not audio.
    pub fn reset(&mut self) {
        if let Some(stretch) = self.stretch.as_mut() {
            stretch.reset();
        }
        self.carry = 0.0;
    }

    /// Stretch one interleaved f64 buffer. Returns `None` when `rate` is
    /// unity (caller passes the original packet through untouched).
    pub fn process(&mut self, interleaved: &[f64], rate: f32) -> Option<Vec<f64>> {
        if is_unity(rate) {
            if self.stretch.is_some() {
                self.reset();
            }
            return None;
        }
        let channels = self.channels;
        let in_frames = interleaved.len() / channels;
        if in_frames == 0 {
            return Some(Vec::new());
        }
        if self.stretch.is_none() {
            match Stretch::new(channels, self.sample_rate) {
                Some(stretch) => self.stretch = Some(stretch),
                None => {
                    // Allocation failure: degrade to 1.0x rather than drop audio.
                    tracing::warn!("could not allocate time-stretch engine; playing at 1.0x");
                    return None;
                }
            }
        }
        let stretch = self.stretch.as_mut().expect("engine allocated above");

        let exact_out = in_frames as f64 / f64::from(rate) + self.carry;
        let out_frames = exact_out.floor().max(0.0) as usize;
        self.carry = exact_out - out_frames as f64;

        for (ch, buf) in self.planar_in.iter_mut().enumerate() {
            buf.clear();
            buf.extend(
                interleaved
                    .iter()
                    .skip(ch)
                    .step_by(channels)
                    .map(|sample| *sample as f32),
            );
        }
        for buf in &mut self.planar_out {
            buf.clear();
            buf.resize(out_frames, 0.0);
        }
        stretch.process(&self.planar_in, in_frames, &mut self.planar_out, out_frames);

        let mut out = Vec::with_capacity(out_frames * channels);
        for frame in 0..out_frames {
            for buf in &self.planar_out {
                out.push(f64::from(buf[frame]));
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn unity_rate_is_a_passthrough() {
        let mut stage = TempoStage::new(2, 44_100);
        assert!(stage.process(&[0.1, 0.2, 0.3, 0.4], 1.0).is_none());
        assert!(stage.process(&[0.1, 0.2], 1.004).is_none());
    }

    #[test]
    fn output_length_tracks_rate_without_drift() {
        let mut stage = TempoStage::new(2, 44_100);
        let input = vec![0.0_f64; 4096 * 2];
        let mut total_out_frames = 0_usize;
        let rounds = 30;
        for _ in 0..rounds {
            let out = stage.process(&input, 1.5).unwrap();
            assert_eq!(out.len() % 2, 0);
            total_out_frames += out.len() / 2;
        }
        let expected = (4096.0 * rounds as f64 / 1.5).floor() as usize;
        assert!(
            total_out_frames.abs_diff(expected) <= 1,
            "got {total_out_frames}, expected ~{expected}"
        );
    }

    #[test]
    fn stretched_audio_keeps_its_level() {
        let mut stage = TempoStage::new(2, 44_100);
        let frames = 44_100;
        let input: Vec<f64> = (0..frames)
            .flat_map(|i| {
                let t = i as f64 / 44_100.0;
                let s = (t * 440.0 * std::f64::consts::TAU).sin() * 0.5;
                [s, s]
            })
            .collect();
        // Prime past the engine's latency, then measure.
        let _ = stage.process(&input, 2.0);
        let out = stage.process(&input, 2.0).unwrap();
        let peak = out.iter().fold(0.0_f64, |acc, s| acc.max(s.abs()));
        assert!(peak > 0.3 && peak < 0.7, "peak {peak}");
    }

    #[test]
    fn speed_normalisation_clamps_and_snaps() {
        assert_eq!(normalize_playback_speed(0.1), 0.5);
        assert_eq!(normalize_playback_speed(9.0), 3.5);
        assert_eq!(normalize_playback_speed(1.2500001), 1.25);
        assert_eq!(normalize_playback_speed(1.26), 1.25);
        assert_eq!(normalize_playback_speed(1.28), 1.3);
    }
}
