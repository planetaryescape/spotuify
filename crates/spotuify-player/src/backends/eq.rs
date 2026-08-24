//! 10-band parametric equalizer for the embedded sink chain.
//!
//! librespot has no EQ, so — like the time-stretch in [`super::tempo`] — the
//! filters live in our own `Sink` wrapper, the one place every decoded sample
//! passes through. Each band is a peaking-EQ biquad (Audio EQ Cookbook, via
//! the `biquad` crate) running per channel in Direct Form 2 Transposed.
//!
//! A flat curve is a true bypass: `process` returns without touching the
//! buffer, so listeners who never open the EQ pay one atomic load per packet.
//!
//! Work is split so the audio thread never does anything unbounded: the
//! writer computes the headroom (a frequency sweep) and publishes it with the
//! curve, leaving the reader only ten `Coefficients::from_params` calls.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use biquad::{Biquad, Coefficients, DirectForm2Transposed, ToHertz, Type};
use parking_lot::Mutex;
use spotuify_core::{eq_headroom_db, EQ_BAND_COUNT, EQ_FREQUENCIES_HZ, EQ_Q};

/// A published curve: the band gains plus the pre-attenuation they need.
///
/// The two travel together because they must never disagree — a reader that
/// saw new bands with an old pre-gain would clip for one packet.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EqCurve {
    bands: [i16; EQ_BAND_COUNT],
    /// Linear pre-attenuation, `10^(headroom_db / 20)`.
    pre_gain: f64,
}

impl EqCurve {
    pub fn is_flat(&self) -> bool {
        self.bands.iter().all(|tenths| *tenths == 0)
    }

    pub fn pre_gain(&self) -> f64 {
        self.pre_gain
    }
}

/// EQ curve shared between the daemon-facing backend (writer) and the sink's
/// audio thread (reader). The generation counter is the fast path: the audio
/// thread loads it per packet and only takes the lock when the curve moved.
#[derive(Clone, Debug)]
pub struct SharedEq(Arc<EqShared>);

#[derive(Debug)]
struct EqShared {
    curve: Mutex<EqCurve>,
    generation: AtomicU64,
}

impl Default for SharedEq {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedEq {
    pub fn new() -> Self {
        Self(Arc::new(EqShared {
            curve: Mutex::new(EqCurve {
                bands: [0; EQ_BAND_COUNT],
                pre_gain: 1.0,
            }),
            generation: AtomicU64::new(0),
        }))
    }

    /// Publish a new curve. No-op (and no generation bump) when the curve is
    /// unchanged, so repeated `eq rock` calls never rebuild coefficients.
    ///
    /// The headroom sweep runs HERE, on whichever task set the curve, not on
    /// the audio thread: it is ~2500 evaluations of a transcendental
    /// expression plus a golden-section refinement, which has no business
    /// between two buffers of PCM. The lock is held for the length of a
    /// struct copy; the sweep happens before it is taken.
    pub fn set_bands(&self, bands: [i16; EQ_BAND_COUNT]) {
        let gains = spotuify_core::EqBands::from_tenths(bands).db();
        let pre_gain = 10.0_f64.powf(eq_headroom_db(&gains) / 20.0);
        {
            let mut current = self.0.curve.lock();
            if current.bands == bands {
                return;
            }
            *current = EqCurve { bands, pre_gain };
        }
        // Release: the curve write above must be visible to any thread that
        // observes this generation.
        self.0.generation.fetch_add(1, Ordering::Release);
    }

    pub fn curve(&self) -> EqCurve {
        *self.0.curve.lock()
    }

    pub fn generation(&self) -> u64 {
        self.0.generation.load(Ordering::Acquire)
    }
}

/// Stateful filter bank for one interleaved stream: `EQ_BAND_COUNT` peaking
/// biquads per channel, plus the pre-gain that buys headroom for boosts.
pub struct EqStage {
    channels: usize,
    sample_rate: f64,
    /// `filters[band][channel]`.
    filters: Vec<Vec<DirectForm2Transposed<f64>>>,
    /// Last curve generation whose coefficients are loaded into `filters`.
    /// `u64::MAX` forces a rebuild on the first packet.
    generation: u64,
    /// Pre-attenuation actually applied to the sample being processed. Walks
    /// towards `target_pre_gain` over [`RAMP_MS`] rather than stepping.
    pre_gain: f64,
    target_pre_gain: f64,
    /// Frames left in the current pre-gain ramp, and the per-frame step.
    ramp_frames: u32,
    ramp_step: f64,
    /// False while the curve is flat — `process` is then a no-op, unless a
    /// ramp is still running it out.
    active: bool,
    rebuilds: u64,
}

/// How long the pre-gain takes to walk to a new value.
///
/// Flat -> Rock moves the pre-gain from 1.0 to 0.29; applied to one sample
/// that is a step discontinuity, i.e. a click, and holding `k` in the TUI
/// editor turns it into zipper noise. 10 ms is long enough to be inaudible
/// and short enough that the curve still feels immediate.
const RAMP_MS: f64 = 10.0;

impl EqStage {
    pub fn new(channels: usize, sample_rate: u32) -> Self {
        // `process` maps each sample to a channel with `index % channels`, so
        // a zero would divide by zero on the audio thread. Callers pass a
        // constant 2; clamp rather than carry a panic path down there.
        let channels = channels.max(1);
        let flat = passthrough_coefficients();
        Self {
            channels,
            sample_rate: f64::from(sample_rate),
            filters: (0..EQ_BAND_COUNT)
                .map(|_| {
                    (0..channels)
                        .map(|_| DirectForm2Transposed::<f64>::new(flat))
                        .collect()
                })
                .collect(),
            generation: u64::MAX,
            pre_gain: 1.0,
            target_pre_gain: 1.0,
            ramp_frames: 0,
            ramp_step: 0.0,
            active: false,
            rebuilds: 0,
        }
    }

    /// Drop filter memory (seek / stop / track change) so audio from before
    /// the discontinuity cannot ring into the new position. Also lands any
    /// in-flight pre-gain ramp: there is no continuity left to protect.
    pub fn reset(&mut self) {
        self.reset_filters();
        self.pre_gain = self.target_pre_gain;
        self.ramp_frames = 0;
        self.ramp_step = 0.0;
    }

    fn reset_filters(&mut self) {
        for band in &mut self.filters {
            for channel in band {
                channel.reset_state();
            }
        }
    }

    /// Pre-attenuation currently being applied. Exposed for tests that watch
    /// the ramp rather than the audio it smooths.
    pub fn pre_gain(&self) -> f64 {
        self.pre_gain
    }

    /// Number of coefficient rebuilds so far. Only the generation counter
    /// should drive this — a rebuild per packet would be a real cost.
    pub fn rebuilds(&self) -> u64 {
        self.rebuilds
    }

    /// Filter one interleaved buffer in place. Returns `false` when the
    /// buffer was left untouched (flat curve, nothing ramping).
    pub fn process(&mut self, eq: &SharedEq, interleaved: &mut [f64]) -> bool {
        let generation = eq.generation();
        if generation != self.generation {
            let first = self.generation == u64::MAX;
            self.rebuild(eq.curve(), first);
            self.generation = generation;
        }
        // A curve that just went flat keeps running until its ramp finishes,
        // so the filters bleed out instead of being cut mid-tail. Their
        // coefficients are already unity by then (a 0 dB peaking section has
        // numerator == denominator), so this only rings the old state out.
        if !self.active && self.ramp_frames == 0 {
            return false;
        }
        for (index, sample) in interleaved.iter_mut().enumerate() {
            let channel = index % self.channels;
            // One gain per frame, so the two channels never drift apart.
            if channel == 0 {
                self.advance_ramp();
            }
            let mut value = *sample * self.pre_gain;
            for band in &mut self.filters {
                value = band[channel].run(value);
            }
            if value.is_finite() {
                *sample = value;
            } else {
                // An IIR that has gone non-finite stays that way: its own
                // state feeds the next output. Flush it rather than emit
                // NaN for the rest of the track.
                *sample = 0.0;
                self.reset();
            }
        }
        true
    }

    fn advance_ramp(&mut self) {
        if self.ramp_frames == 0 {
            return;
        }
        self.ramp_frames -= 1;
        if self.ramp_frames == 0 {
            self.pre_gain = self.target_pre_gain;
        } else {
            self.pre_gain += self.ramp_step;
        }
    }

    /// Load a published curve's coefficients. `first` skips the ramp: no
    /// audio has been through this stage yet, so there is nothing to click.
    fn rebuild(&mut self, curve: EqCurve, first: bool) {
        self.rebuilds += 1;
        // "Running" covers a curve that went flat but is still ramping out:
        // its filters hold live audio, so they must not be cleared.
        let was_running = self.active || self.ramp_frames > 0;
        let gains = spotuify_core::EqBands::from_tenths(curve.bands).db();
        self.active = !curve.is_flat();
        self.target_pre_gain = curve.pre_gain;
        if first {
            self.pre_gain = curve.pre_gain;
            self.ramp_frames = 0;
            self.ramp_step = 0.0;
        } else {
            // Coefficients switch instantly; only the level is ramped. See
            // D033: crossfading two filter banks costs a second bank and a
            // second pass per sample to fix an artefact the level ramp
            // already covers.
            let frames = ((self.sample_rate * RAMP_MS / 1_000.0).round() as u32).max(1);
            self.ramp_frames = frames;
            self.ramp_step = (self.target_pre_gain - self.pre_gain) / f64::from(frames);
        }
        for (index, gain) in gains.iter().enumerate() {
            let coefficients = Coefficients::<f64>::from_params(
                Type::PeakingEQ(*gain),
                self.sample_rate.hz(),
                f64::from(EQ_FREQUENCIES_HZ[index]).hz(),
                EQ_Q,
            )
            .unwrap_or_else(|_| {
                // Only reachable if a centre frequency ever lands above
                // Nyquist for the stream's rate; pass that band through.
                tracing::warn!(
                    band = index,
                    hz = EQ_FREQUENCIES_HZ[index],
                    "eq band is outside Nyquist for this stream; bypassing it"
                );
                passthrough_coefficients()
            });
            for channel in &mut self.filters[index] {
                channel.update_coefficients(coefficients);
            }
        }
        // Coming back from a full bypass (not merely the tail of a ramp),
        // the filters still hold samples from whenever the EQ was last on.
        // Start clean; mid-curve tweaks keep their state so a nudge doesn't
        // click. Only the biquads are cleared — the ramp just started.
        if self.active && !was_running {
            self.reset_filters();
        }
    }
}

/// Unity-gain biquad: `y[n] = x[n]`.
fn passthrough_coefficients() -> Coefficients<f64> {
    Coefficients {
        a1: 0.0,
        a2: 0.0,
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use spotuify_core::EqSettings;

    const SAMPLE_RATE: u32 = 44_100;
    const CHANNELS: usize = 2;

    fn sine(hz: f64, frames: usize, amplitude: f64) -> Vec<f64> {
        (0..frames)
            .flat_map(|frame| {
                let t = frame as f64 / f64::from(SAMPLE_RATE);
                let sample = (t * hz * std::f64::consts::TAU).sin() * amplitude;
                [sample, sample]
            })
            .collect()
    }

    fn rms(samples: &[f64]) -> f64 {
        let sum: f64 = samples.iter().map(|s| s * s).sum();
        (sum / samples.len() as f64).sqrt()
    }

    /// Filter a continuous sine and return the second half, by which point
    /// the biquads' startup transient has decayed.
    ///
    /// The two halves come from ONE signal and each is processed once, so
    /// the measured half is phase-continuous with the state the filters
    /// already hold. Re-processing the same buffer twice would filter it
    /// twice and measure the square of the response.
    fn settled(stage: &mut EqStage, eq: &SharedEq, hz: f64, amplitude: f64) -> Vec<f64> {
        let half = SAMPLE_RATE as usize / 4 * CHANNELS;
        let mut signal = sine(hz, SAMPLE_RATE as usize / 2, amplitude);
        let (prime, measure) = signal.split_at_mut(half);
        stage.process(eq, prime);
        stage.process(eq, measure);
        measure.to_vec()
    }

    /// RMS of `hz` after the EQ settles, relative to the same signal in.
    fn steady_state_gain(stage: &mut EqStage, eq: &SharedEq, hz: f64) -> f64 {
        let reference = sine(hz, SAMPLE_RATE as usize / 4, 0.5);
        rms(&settled(stage, eq, hz, 0.5)) / rms(&reference)
    }

    fn stage_and_eq(settings: &EqSettings) -> (EqStage, SharedEq) {
        let eq = SharedEq::new();
        eq.set_bands(settings.bands_tenths());
        (EqStage::new(CHANNELS, SAMPLE_RATE), eq)
    }

    #[test]
    fn a_flat_curve_is_a_byte_identical_passthrough() {
        let (mut stage, eq) = stage_and_eq(&EqSettings::flat());
        let input = sine(1_000.0, 2_048, 0.5);
        let mut buffer = input.clone();
        assert!(!stage.process(&eq, &mut buffer), "flat curve must bypass");
        assert_eq!(buffer, input);
    }

    #[test]
    fn a_boosted_band_lifts_its_own_frequency_and_leaves_the_others() {
        // +12 dB at 1 kHz. The pre-gain trades the whole curve down by the
        // same 12 dB for headroom, so measure against it.
        let settings = EqSettings::flat().with_band(4, 12.0);
        let pre_gain = 10.0_f64.powf(settings.headroom_db() / 20.0);
        let (mut stage, eq) = stage_and_eq(&settings);

        let at_1k = steady_state_gain(&mut stage, &eq, 1_000.0) / pre_gain;
        assert!(
            (at_1k - 4.0).abs() < 0.4,
            "1 kHz gain {at_1k}x, expected ~4x (+12 dB)"
        );

        let at_100 = steady_state_gain(&mut stage, &eq, 100.0) / pre_gain;
        assert!(
            (at_100 - 1.0).abs() < 0.1,
            "100 Hz gain {at_100}x, expected ~1x (untouched)"
        );
    }

    #[test]
    fn a_cut_band_attenuates_its_own_frequency() {
        // Cut-only curves need no headroom, so the pre-gain stays 1.0.
        let settings = EqSettings::flat().with_band(4, -12.0);
        assert_eq!(settings.headroom_db(), 0.0);
        let (mut stage, eq) = stage_and_eq(&settings);
        let at_1k = steady_state_gain(&mut stage, &eq, 1_000.0);
        assert!(
            (at_1k - 0.25).abs() < 0.025,
            "1 kHz gain {at_1k}x, expected ~0.25x (-12 dB)"
        );
    }

    #[test]
    fn pre_gain_keeps_every_preset_inside_full_scale_at_its_worst_frequency() {
        // The headroom claims a full-scale sine cannot leave the filters
        // above 1.0. Probe each preset where its own cascade is loudest
        // rather than at a hand-picked band centre — for Bass Boost that is
        // ~76 Hz, between the +8 and +6 bands, not either centre.
        for (name, _) in spotuify_core::EQ_PRESETS {
            let settings = EqSettings::from_preset(name).unwrap();
            let worst = spotuify_core::eq_peak_frequency_hz(&settings.bands_db());
            let (mut stage, eq) = stage_and_eq(&settings);
            let peak = settled(&mut stage, &eq, worst, 1.0)
                .iter()
                .fold(0.0_f64, |acc, sample| acc.max(sample.abs()));
            assert!(
                peak <= 1.0,
                "{name} peaked at {peak} for a full-scale {worst:.0} Hz sine"
            );
            // A curve that boosts at all should still be using most of the
            // range: over-attenuating would also pass a `<= 1.0` check.
            if !settings.is_flat() && settings.headroom_db() < 0.0 {
                assert!(
                    peak > 0.9,
                    "{name} peaked at only {peak}; headroom too deep"
                );
            }
        }
    }

    #[test]
    fn extreme_curves_stay_finite() {
        for gain in [12.0_f32, -12.0] {
            let settings = EqSettings::from_bands(
                spotuify_core::EqBands::from_db(&[gain; EQ_BAND_COUNT]).unwrap(),
            );
            let (mut stage, eq) = stage_and_eq(&settings);
            let mut buffer = sine(50.0, 8_192, 1.0);
            // A step to full scale is the worst case for an IIR's overshoot.
            buffer[0] = 1.0;
            buffer[1] = -1.0;
            for _ in 0..8 {
                stage.process(&eq, &mut buffer);
                assert!(
                    buffer.iter().all(|s| s.is_finite()),
                    "{gain} dB curve produced a non-finite sample"
                );
            }
        }
    }

    #[test]
    fn the_writer_publishes_the_pre_gain_with_the_curve() {
        // The audio thread must never run the headroom sweep. It reads a
        // pre-gain the writer already computed, in the same snapshot as the
        // bands it belongs to.
        let eq = SharedEq::new();
        assert_eq!(eq.curve().pre_gain(), 1.0);
        assert!(eq.curve().is_flat());

        let rock = EqSettings::from_preset("Rock").unwrap();
        eq.set_bands(rock.bands_tenths());
        let curve = eq.curve();
        assert!(!curve.is_flat());
        assert_eq!(
            curve.pre_gain(),
            10.0_f64.powf(rock.headroom_db() / 20.0),
            "published pre-gain must match the curve's headroom"
        );
    }

    #[test]
    fn a_curve_change_ramps_the_level_instead_of_stepping_it() {
        let (mut stage, eq) = stage_and_eq(&EqSettings::flat());
        let frames = SAMPLE_RATE as usize / 100; // 10 ms, one ramp's worth
        let mut warm = vec![1.0_f64; frames * CHANNELS];
        stage.process(&eq, &mut warm);
        assert_eq!(stage.pre_gain(), 1.0);

        let bass = EqSettings::from_preset("Bass Boost").unwrap();
        let target = 10.0_f64.powf(bass.headroom_db() / 20.0);
        assert!(target < 0.4, "Bass Boost should be a big level change");
        eq.set_bands(bass.bands_tenths());

        // Half a ramp in, the level must be part-way there, not already
        // landed and not still at 1.0.
        let mut half = vec![1.0_f64; frames / 2 * CHANNELS];
        stage.process(&eq, &mut half);
        let midway = stage.pre_gain();
        assert!(
            midway < 1.0 && midway > target,
            "pre-gain {midway} should be between 1.0 and {target} mid-ramp"
        );

        // A ramp's worth later it has arrived and stays put.
        let mut rest = vec![1.0_f64; frames * CHANNELS];
        stage.process(&eq, &mut rest);
        assert!((stage.pre_gain() - target).abs() < 1e-12);
    }

    #[test]
    fn switching_curves_mid_signal_produces_no_step_discontinuity() {
        // DC is the cleanest probe: a peaking EQ has unity response at DC,
        // so anything that shows up here is the level change, not the
        // filters. Un-ramped, Flat -> Bass Boost drops 1.0 to ~0.29 between
        // two adjacent samples.
        let (mut stage, eq) = stage_and_eq(&EqSettings::flat());
        let frames = SAMPLE_RATE as usize / 20;
        let mut before = vec![1.0_f64; frames * CHANNELS];
        stage.process(&eq, &mut before);

        let bass = EqSettings::from_preset("Bass Boost").unwrap();
        let target = 10.0_f64.powf(bass.headroom_db() / 20.0);
        eq.set_bands(bass.bands_tenths());

        let mut after = vec![1.0_f64; frames * CHANNELS];
        stage.process(&eq, &mut after);

        // Stitch the boundary back together and look for a jump.
        let mut stream = before;
        stream.extend_from_slice(&after);
        let worst = stream
            .windows(CHANNELS + 1)
            .map(|window| (window[CHANNELS] - window[0]).abs())
            .fold(0.0_f64, f64::max);
        let unramped_step = 1.0 - target;
        assert!(
            unramped_step > 0.5,
            "the un-ramped jump would be {unramped_step}; test is not probing anything"
        );
        assert!(
            worst < 0.05,
            "largest frame-to-frame delta {worst} across the switch; \
             un-ramped this would be about {unramped_step}"
        );
    }

    #[test]
    fn coefficients_rebuild_only_when_the_curve_moves() {
        let (mut stage, eq) = stage_and_eq(&EqSettings::from_preset("Rock").unwrap());
        let mut buffer = sine(1_000.0, 512, 0.5);
        for _ in 0..20 {
            stage.process(&eq, &mut buffer);
        }
        assert_eq!(stage.rebuilds(), 1, "steady curve must not rebuild");

        // Setting the same curve again is not a change.
        eq.set_bands(EqSettings::from_preset("rock").unwrap().bands_tenths());
        stage.process(&eq, &mut buffer);
        assert_eq!(stage.rebuilds(), 1);

        eq.set_bands(EqSettings::from_preset("Jazz").unwrap().bands_tenths());
        stage.process(&eq, &mut buffer);
        stage.process(&eq, &mut buffer);
        assert_eq!(stage.rebuilds(), 2);
    }

    #[test]
    fn every_preset_is_realisable_at_44_1_khz() {
        for (name, bands) in spotuify_core::EQ_PRESETS {
            let eq = SharedEq::new();
            eq.set_bands(bands);
            let mut stage = EqStage::new(CHANNELS, SAMPLE_RATE);
            let mut buffer = sine(440.0, 4_096, 0.8);
            stage.process(&eq, &mut buffer);
            assert!(
                buffer.iter().all(|s| s.is_finite() && s.abs() <= 1.0),
                "preset {name} produced an out-of-range sample"
            );
        }
    }
}
