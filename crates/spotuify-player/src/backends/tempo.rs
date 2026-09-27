//! Pitch-preserving playback-rate stage for the embedded sink chain.
//!
//! librespot cannot change playback speed: its sample rate is a compile-time
//! 44.1 kHz constant, `PlayerConfig`/`Spirc` expose no rate control, and the
//! Connect protocol's `playback_speed` is only mirrored back as 0/1. The one
//! place every decoded sample passes through is our own `Sink` wrapper, so the
//! speed change lives there.
//!
//! Speed only ever applies to podcasts (music always plays at 1x), so the
//! engine is Sonic, a time-domain speed-up built for speech: it drops or
//! repeats whole pitch periods, so a voice stays one clean voice. The phase
//! vocoder used before (Signalsmith Stretch) is tuned for music and smeared
//! speech into an echoey, robotic sound at 1.5x-2x.
//!
//! Because librespot's player thread is paced by the sink (writes block on
//! the physical backend's buffer), the decoder naturally runs `r`x faster and
//! its own position timestamps stay content-accurate - no bookkeeping here.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// The workspace denies `unsafe_code`; this module is the single, audited
/// exception: a C ABI over the vendored Sonic library (`vendor/sonic`, built
/// by `build.rs`). Every pointer handed across is owned by [`Sonic`] or
/// borrowed from slices whose lengths are checked before the call.
#[allow(unsafe_code)]
mod sonic_ffi {
    use std::os::raw::c_int;
    use std::ptr::NonNull;

    mod ffi {
        use std::os::raw::c_int;

        #[repr(C)]
        pub struct SonicStream {
            _private: [u8; 0],
        }

        extern "C" {
            pub fn sonicCreateStream(sample_rate: c_int, num_channels: c_int) -> *mut SonicStream;
            pub fn sonicDestroyStream(stream: *mut SonicStream);
            pub fn sonicSetSpeed(stream: *mut SonicStream, speed: f32);
            pub fn sonicWriteFloatToStream(
                stream: *mut SonicStream,
                samples: *const f32,
                num_frames: c_int,
            ) -> c_int;
            pub fn sonicReadFloatFromStream(
                stream: *mut SonicStream,
                samples: *mut f32,
                max_frames: c_int,
            ) -> c_int;
            pub fn sonicFlushStream(stream: *mut SonicStream) -> c_int;
        }
    }

    /// Owning handle to one Sonic stream (interleaved f32 in/out).
    pub(super) struct Sonic {
        raw: NonNull<ffi::SonicStream>,
        channels: usize,
    }

    // SAFETY: a Sonic stream has no thread affinity or global state; it is
    // only ever driven from the sink's audio thread and moves with it.
    unsafe impl Send for Sonic {}

    impl Sonic {
        pub(super) fn new(channels: usize, sample_rate: u32) -> Option<Self> {
            // SAFETY: plain constructor; a null return means allocation failed.
            let raw = unsafe { ffi::sonicCreateStream(sample_rate as c_int, channels as c_int) };
            NonNull::new(raw).map(|raw| Self { raw, channels })
        }

        pub(super) fn set_speed(&mut self, speed: f32) {
            // SAFETY: `raw` is a live stream owned by this handle.
            unsafe { ffi::sonicSetSpeed(self.raw.as_ptr(), speed) }
        }

        /// Queue interleaved frames. False means Sonic could not grow its
        /// buffer (allocation failure).
        pub(super) fn write(&mut self, interleaved: &[f32]) -> bool {
            assert_eq!(interleaved.len() % self.channels, 0, "partial frame");
            let frames = interleaved.len() / self.channels;
            if frames == 0 {
                return true;
            }
            // SAFETY: the buffer holds exactly `frames * channels` samples,
            // which is all Sonic reads.
            unsafe {
                ffi::sonicWriteFloatToStream(
                    self.raw.as_ptr(),
                    interleaved.as_ptr(),
                    frames as c_int,
                ) != 0
            }
        }

        /// Push any remaining input through, so the next read drains it.
        pub(super) fn flush(&mut self) -> bool {
            // SAFETY: `raw` is a live stream owned by this handle.
            unsafe { ffi::sonicFlushStream(self.raw.as_ptr()) != 0 }
        }

        /// Append every processed frame currently available to `out`.
        pub(super) fn drain_into(&mut self, out: &mut Vec<f32>) {
            const CHUNK_FRAMES: usize = 4096;
            loop {
                let start = out.len();
                out.resize(start + CHUNK_FRAMES * self.channels, 0.0);
                // SAFETY: `out[start..]` has room for exactly CHUNK_FRAMES
                // frames, and Sonic writes at most `max_frames` frames.
                let got = unsafe {
                    ffi::sonicReadFloatFromStream(
                        self.raw.as_ptr(),
                        out[start..].as_mut_ptr(),
                        CHUNK_FRAMES as c_int,
                    )
                };
                let got = got.max(0) as usize;
                out.truncate(start + got * self.channels);
                if got < CHUNK_FRAMES {
                    return;
                }
            }
        }
    }

    impl Drop for Sonic {
        fn drop(&mut self) {
            // SAFETY: `raw` came from `sonicCreateStream` and is freed once.
            unsafe { ffi::sonicDestroyStream(self.raw.as_ptr()) }
        }
    }
}

use sonic_ffi::Sonic;

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

/// Whether a rate is close enough to 1.0 that speed processing is pure cost.
pub fn is_unity(rate: f32) -> bool {
    (rate - 1.0).abs() < 0.005
}

/// Stateful speed engine for one stereo stream. Created lazily on the first
/// non-unity buffer and dropped when the rate returns to 1.0, so the common
/// music case carries no DSP at all.
pub struct TempoStage {
    channels: usize,
    sample_rate: u32,
    engine: Option<Sonic>,
    /// The speed the engine is currently set to, to skip redundant updates.
    engine_speed: f32,
    /// Scratch buffers reused across calls.
    input: Vec<f32>,
    output: Vec<f32>,
}

impl TempoStage {
    pub fn new(channels: usize, sample_rate: u32) -> Self {
        Self {
            channels,
            sample_rate,
            engine: None,
            engine_speed: 1.0,
            input: Vec::new(),
            output: Vec::new(),
        }
    }

    /// Drop engine state (seek / stop / track change). Buffered audio from
    /// before the discontinuity is discarded; the next non-unity buffer
    /// starts a fresh stream.
    pub fn reset(&mut self) {
        self.engine = None;
    }

    /// Speed-adjust one interleaved f64 buffer. Returns `None` when `rate` is
    /// unity and nothing is buffered (caller passes the packet through).
    ///
    /// Sonic holds back a few milliseconds while it looks for pitch periods,
    /// so an individual call can return fewer frames than `len / rate`; the
    /// total over a stream converges on it.
    pub fn process(&mut self, interleaved: &[f64], rate: f32) -> Option<Vec<f64>> {
        if is_unity(rate) {
            // Leaving a faster/slower rate: emit what the engine still holds
            // before this packet, so switching back to 1x has no gap.
            let mut engine = self.engine.take()?;
            self.output.clear();
            if engine.flush() {
                engine.drain_into(&mut self.output);
            }
            let mut out: Vec<f64> = self.output.iter().map(|s| f64::from(*s)).collect();
            out.extend_from_slice(interleaved);
            return Some(out);
        }
        if interleaved.is_empty() {
            return Some(Vec::new());
        }
        if self.engine.is_none() {
            match Sonic::new(self.channels, self.sample_rate) {
                Some(engine) => {
                    self.engine = Some(engine);
                    self.engine_speed = 1.0;
                }
                None => {
                    // Allocation failure: degrade to 1.0x rather than drop audio.
                    tracing::warn!("could not allocate playback-speed engine; playing at 1.0x");
                    return None;
                }
            }
        }
        let engine = self.engine.as_mut().expect("engine allocated above");
        if (self.engine_speed - rate).abs() > f32::EPSILON {
            engine.set_speed(rate);
            self.engine_speed = rate;
        }

        self.input.clear();
        self.input.extend(interleaved.iter().map(|s| *s as f32));
        if !engine.write(&self.input) {
            tracing::warn!("playback-speed engine could not buffer audio; playing at 1.0x");
            self.engine = None;
            return None;
        }
        self.output.clear();
        engine.drain_into(&mut self.output);
        Some(self.output.iter().map(|s| f64::from(*s)).collect())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const RATE: u32 = 44_100;

    fn sine(frames: usize, hz: f64) -> Vec<f64> {
        (0..frames)
            .flat_map(|i| {
                let s = (i as f64 / f64::from(RATE) * hz * std::f64::consts::TAU).sin() * 0.5;
                [s, s]
            })
            .collect()
    }

    /// Stream `input` through the stage in `packet`-frame chunks, the way the
    /// decoder hands packets to the sink.
    fn stream(stage: &mut TempoStage, input: &[f64], packet: usize, rate: f32) -> Vec<f64> {
        let mut out = Vec::new();
        for chunk in input.chunks(packet * 2) {
            out.extend(stage.process(chunk, rate).unwrap());
        }
        out
    }

    #[test]
    fn unity_rate_is_a_passthrough() {
        let mut stage = TempoStage::new(2, RATE);
        assert!(stage.process(&[0.1, 0.2, 0.3, 0.4], 1.0).is_none());
        assert!(stage.process(&[0.1, 0.2], 1.004).is_none());
    }

    #[test]
    fn output_length_tracks_rate() {
        let mut stage = TempoStage::new(2, RATE);
        let input = sine(RATE as usize * 4, 220.0);
        let out = stream(&mut stage, &input, 1152, 1.5);
        assert_eq!(out.len() % 2, 0);
        let got = out.len() / 2;
        let expected = input.len() / 2 * 2 / 3;
        // Only Sonic's look-ahead (well under 100 ms) may be outstanding.
        assert!(
            got <= expected && expected - got < RATE as usize / 10,
            "got {got}, expected ~{expected}"
        );
    }

    #[test]
    fn returning_to_unity_flushes_what_the_engine_held() {
        let mut stage = TempoStage::new(2, RATE);
        let input = sine(RATE as usize * 2, 220.0);
        let fast = stream(&mut stage, &input, 1152, 2.0).len() / 2;
        let packet = sine(1152, 220.0);
        let tail_and_packet = stage.process(&packet, 1.0).unwrap().len() / 2;
        let expected = input.len() / 2 / 2 + packet.len() / 2;
        let total = fast + tail_and_packet;
        // Sonic's flush rounds to whole pitch periods (~4 ms short measured);
        // anything under 10 ms at the switch is inaudible.
        assert!(
            total.abs_diff(expected) <= RATE as usize / 100,
            "got {total}, expected ~{expected}"
        );
        // With the engine gone, the next unity packet passes straight through.
        assert!(stage.process(&packet, 1.0).is_none());
    }

    #[test]
    fn speeding_up_keeps_the_pitch() {
        // A chipmunk effect would double the tone; a pitch-preserving speed-up
        // keeps it at 440 Hz. Count zero crossings on one channel.
        let mut stage = TempoStage::new(2, RATE);
        let out = stream(&mut stage, &sine(RATE as usize * 4, 440.0), 1152, 2.0);
        let left: Vec<f64> = out.iter().step_by(2).copied().collect();
        let steady = &left[left.len() / 4..left.len() * 3 / 4];
        let crossings = steady
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        let hz = crossings as f64 / 2.0 / (steady.len() as f64 / f64::from(RATE));
        assert!((hz - 440.0).abs() < 440.0 * 0.03, "measured {hz:.1} Hz");
    }

    #[test]
    fn speeding_up_keeps_the_level() {
        let mut stage = TempoStage::new(2, RATE);
        let out = stream(&mut stage, &sine(RATE as usize * 2, 440.0), 1152, 2.0);
        let peak = out.iter().fold(0.0_f64, |acc, s| acc.max(s.abs()));
        assert!(peak > 0.4 && peak < 0.6, "peak {peak}");
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
