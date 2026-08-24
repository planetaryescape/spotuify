//! 10-band parametric equalizer for the embedded sink chain.
//!
//! librespot has no EQ, so — like the time-stretch in [`super::tempo`] — the
//! filters live in our own `Sink` wrapper, the one place every decoded sample
//! passes through. Each band is a peaking-EQ biquad (Audio EQ Cookbook, via
//! the `biquad` crate) running per channel in Direct Form 2 Transposed.
//!
//! A flat curve is a true bypass: `process` returns without touching the
//! buffer, so listeners who never open the EQ pay one atomic load per packet.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use biquad::{Biquad, Coefficients, DirectForm2Transposed, ToHertz, Type};
use parking_lot::Mutex;
use spotuify_core::{eq_headroom_db, EQ_BAND_COUNT, EQ_FREQUENCIES_HZ, EQ_Q};

/// EQ curve shared between the daemon-facing backend (writer) and the sink's
/// audio thread (reader). The generation counter is the fast path: the audio
/// thread loads it per packet and only takes the lock when the curve moved.
#[derive(Clone, Debug)]
pub struct SharedEq(Arc<EqShared>);

#[derive(Debug)]
struct EqShared {
    /// Band gains in tenths of a dB (see `spotuify_core::EqSettings`).
    bands: Mutex<[i16; EQ_BAND_COUNT]>,
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
            bands: Mutex::new([0; EQ_BAND_COUNT]),
            generation: AtomicU64::new(0),
        }))
    }

    /// Publish a new curve. No-op (and no generation bump) when the curve is
    /// unchanged, so repeated `eq rock` calls never rebuild coefficients.
    pub fn set_bands(&self, bands: [i16; EQ_BAND_COUNT]) {
        {
            let mut current = self.0.bands.lock();
            if *current == bands {
                return;
            }
            *current = bands;
        }
        // Release: the band write above must be visible to any thread that
        // observes this generation.
        self.0.generation.fetch_add(1, Ordering::Release);
    }

    pub fn bands(&self) -> [i16; EQ_BAND_COUNT] {
        *self.0.bands.lock()
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
    /// Linear pre-attenuation, `10^(headroom_db / 20)`.
    pre_gain: f64,
    /// False while the curve is flat — `process` is then a no-op.
    active: bool,
    rebuilds: u64,
}

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
            active: false,
            rebuilds: 0,
        }
    }

    /// Drop filter memory (seek / stop / track change) so audio from before
    /// the discontinuity cannot ring into the new position.
    pub fn reset(&mut self) {
        for band in &mut self.filters {
            for channel in band {
                channel.reset_state();
            }
        }
    }

    /// Number of coefficient rebuilds so far. Only the generation counter
    /// should drive this — a rebuild per packet would be a real cost.
    pub fn rebuilds(&self) -> u64 {
        self.rebuilds
    }

    /// Filter one interleaved buffer in place. Returns `false` when the curve
    /// is flat and the buffer was left untouched.
    pub fn process(&mut self, eq: &SharedEq, interleaved: &mut [f64]) -> bool {
        let generation = eq.generation();
        if generation != self.generation {
            self.rebuild(eq.bands());
            self.generation = generation;
        }
        if !self.active {
            return false;
        }
        for (index, sample) in interleaved.iter_mut().enumerate() {
            let channel = index % self.channels;
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

    fn rebuild(&mut self, bands: [i16; EQ_BAND_COUNT]) {
        self.rebuilds += 1;
        let was_active = self.active;
        let gains = spotuify_core::EqBands::from_tenths(bands).db();
        self.active = bands.iter().any(|tenths| *tenths != 0);
        self.pre_gain = 10.0_f64.powf(eq_headroom_db(&gains) / 20.0);
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
        // Coming back from bypass, the filters still hold samples from
        // whenever the EQ was last on. Start clean; mid-curve tweaks keep
        // their state so a nudge doesn't click.
        if self.active && !was_active {
            self.reset();
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

    /// RMS of `hz` after the EQ settles, relative to the same signal in.
    /// The first buffer carries the filters' startup transient, so prime
    /// with one buffer and measure the second.
    fn steady_state_gain(stage: &mut EqStage, eq: &SharedEq, hz: f64) -> f64 {
        let input = sine(hz, SAMPLE_RATE as usize / 4, 0.5);
        let mut primed = input.clone();
        stage.process(eq, &mut primed);
        let mut measured = input.clone();
        stage.process(eq, &mut measured);
        rms(&measured) / rms(&input)
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
    fn pre_gain_keeps_bass_boost_inside_full_scale() {
        let (mut stage, eq) = stage_and_eq(&EqSettings::from_preset("Bass Boost").unwrap());
        // Full-scale 1 kHz sine: the band at 1 kHz is 0 dB in this preset,
        // so without the pre-gain the +8 dB bass lift would still leave the
        // peak at 1.0 — check the boosted end too.
        for hz in [70.0, 180.0, 1_000.0] {
            let mut buffer = sine(hz, SAMPLE_RATE as usize / 4, 1.0);
            stage.process(&eq, &mut buffer);
            stage.process(&eq, &mut buffer);
            let peak = buffer.iter().fold(0.0_f64, |acc, s| acc.max(s.abs()));
            assert!(peak <= 1.0, "{hz} Hz peaked at {peak}, clipping");
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
