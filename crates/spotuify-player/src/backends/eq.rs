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
//! Boosts are kept inside full scale by the peak limiter in
//! [`super::limiter`], not by attenuating the curve up front — see D036.
//! Nothing unbounded runs on the audio thread: the reader only builds ten
//! `Coefficients::from_params` when the generation counter moves.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use biquad::{Biquad, Coefficients, DirectForm2Transposed, ToHertz, Type};
use parking_lot::Mutex;
use spotuify_core::{EqLimiting, EQ_BAND_COUNT, EQ_FREQUENCIES_HZ, EQ_Q};

use crate::backends::limiter::{reduction_db, Limiter};

/// A published curve: ten band gains in tenths of a dB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EqCurve {
    bands: [i16; EQ_BAND_COUNT],
}

impl EqCurve {
    pub fn is_flat(&self) -> bool {
        self.bands.iter().all(|tenths| *tenths == 0)
    }
}

/// EQ curve shared between the daemon-facing backend (writer) and the sink's
/// audio thread (reader). The generation counter is the fast path: the audio
/// thread loads it per packet and only takes the lock when the curve moved.
///
/// It carries the limiter's meter back the other way, so a diagnostic read
/// is one relaxed atomic load on a handle the backend already holds.
#[derive(Clone, Debug)]
pub struct SharedEq(Arc<EqShared>);

#[derive(Debug)]
struct EqShared {
    curve: Mutex<EqCurve>,
    generation: AtomicU64,
    /// The limiter meter: the curve generation a reading was taken under in
    /// the high 48 bits, the gain reduction in tenths of a dB in the low 16.
    ///
    /// The two share a word because they have to move together. A packet
    /// that loaded the old curve can finish *after* the curve changed, and
    /// an untagged store would let it overwrite the fresh idle with a
    /// reading of a curve nobody is listening to any more.
    limiting: AtomicU64,
}

/// Bits the gain reduction occupies in [`EqShared::limiting`]; the
/// generation gets the remaining 48, i.e. 2.8e14 curve changes.
const LIMITING_TENTHS_BITS: u32 = 16;

fn pack_limiting(generation: u64, tenths: u16) -> u64 {
    (generation << LIMITING_TENTHS_BITS) | u64::from(tenths)
}

fn unpack_generation(word: u64) -> u64 {
    word >> LIMITING_TENTHS_BITS
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
            }),
            generation: AtomicU64::new(0),
            limiting: AtomicU64::new(0),
        }))
    }

    /// Publish a new curve. No-op (and no generation bump) when the curve is
    /// unchanged, so repeated `eq rock` calls never rebuild coefficients.
    ///
    /// The curve lock is held across the generation bump so two writers
    /// cannot pick the same next generation. It is a handful of atomics and
    /// an array copy long, and the audio thread already contends for this
    /// lock in [`Self::curve`].
    pub fn set_bands(&self, bands: [i16; EQ_BAND_COUNT]) {
        let mut current = self.0.curve.lock();
        if current.bands == bands {
            return;
        }
        *current = EqCurve { bands };
        let generation = self.0.generation.load(Ordering::Relaxed) + 1;
        // A reading taken under the previous curve is stale the moment the
        // curve moves. The sink normally corrects it on the next packet, but
        // there may not be one — a curve set while playback is paused would
        // otherwise leave the old curve's reduction on `spotuify eq`.
        //
        // Published BEFORE the generation is visible, so a packet still
        // running on the old curve loses the compare, and no packet can yet
        // be running on the new one.
        self.publish_limiting(generation, EqLimiting::IDLE);
        // Release: the curve write above must be visible to any thread that
        // observes this generation.
        self.0.generation.store(generation, Ordering::Release);
    }

    pub fn curve(&self) -> EqCurve {
        *self.0.curve.lock()
    }

    pub fn generation(&self) -> u64 {
        self.0.generation.load(Ordering::Acquire)
    }

    /// Read-only handle on the limiter's gain reduction, for clients that
    /// should not be able to set the curve.
    pub fn meter(&self) -> EqLimiterMeter {
        EqLimiterMeter(self.0.clone())
    }

    /// Reset the meter to idle under whatever curve is current. Used when a
    /// stage starts, stops, or is dropped — all points where the stream of
    /// packets that would otherwise correct the meter has ended.
    pub fn clear_limiting(&self) {
        self.publish_limiting(self.generation(), EqLimiting::IDLE);
    }

    /// Record a reading taken under `generation`. Readings from a curve that
    /// has since been replaced are dropped rather than applied.
    fn publish_limiting(&self, generation: u64, limiting: EqLimiting) {
        let next = pack_limiting(generation, limiting.into_tenths());
        let mut current = self.0.limiting.load(Ordering::Relaxed);
        while generation >= unpack_generation(current) {
            match self.0.limiting.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }
}

/// Reader-side view of the limiter's live gain reduction.
///
/// Handed to the daemon at player install, the same way the audio counter
/// is: reading a meter must not cost a round trip through the player actor.
#[derive(Clone, Debug)]
pub struct EqLimiterMeter(Arc<EqShared>);

impl EqLimiterMeter {
    /// Gain reduction published for the most recent packet.
    pub fn limiting(&self) -> EqLimiting {
        let word = self.0.limiting.load(Ordering::Relaxed);
        EqLimiting::from_tenths(word as u16)
    }
}

/// Stateful filter bank for one interleaved stream: `EQ_BAND_COUNT` peaking
/// biquads per channel, followed by the shared peak limiter.
pub struct EqStage {
    /// The curve this stage filters with, and the meter it publishes to.
    ///
    /// Owned rather than passed per call: the stage has to reach the meter
    /// from `new` and from `Drop`, and a stage that could be handed a
    /// *different* `SharedEq` than the one it is tagged against would be a
    /// bug with no way to catch it.
    eq: SharedEq,
    channels: usize,
    sample_rate: f64,
    /// `filters[band][channel]`.
    filters: Vec<Vec<DirectForm2Transposed<f64>>>,
    /// Last curve generation whose coefficients are loaded into `filters`.
    /// `u64::MAX` forces a rebuild on the first packet.
    generation: u64,
    limiter: Limiter,
    /// Last value this stage published to [`SharedEq`], so a bypassed stage
    /// stores at most once instead of on every packet.
    ///
    /// `None` until the first packet. The cache is per stage but the meter
    /// is shared, and a reconnect builds a fresh stage on the same
    /// [`SharedEq`]: if the previous stage left -8 dB on the meter, a cache
    /// that started out claiming idle would suppress the store that
    /// corrects it, and `eq-get` would report the old reduction forever.
    published: Option<EqLimiting>,
    /// False while the curve is flat — `process` is then a no-op.
    active: bool,
    rebuilds: u64,
}

impl EqStage {
    pub fn new(channels: usize, sample_rate: u32, eq: SharedEq) -> Self {
        // `process` walks the buffer in frames of `channels`, so a zero would
        // produce empty chunks forever. Callers pass a constant 2; clamp
        // rather than carry a panic path down to the audio thread.
        let channels = channels.max(1);
        let flat = passthrough_coefficients();
        let sample_rate = f64::from(sample_rate);
        // A replacement stage inherits whatever the last one left behind —
        // librespot can drop a running chain without calling `stop` — and
        // would otherwise not touch the meter until audio flows again.
        eq.clear_limiting();
        Self {
            eq,
            channels,
            sample_rate,
            filters: (0..EQ_BAND_COUNT)
                .map(|_| {
                    (0..channels)
                        .map(|_| DirectForm2Transposed::<f64>::new(flat))
                        .collect()
                })
                .collect(),
            generation: u64::MAX,
            limiter: Limiter::new(sample_rate),
            published: None,
            active: false,
            rebuilds: 0,
        }
    }

    /// Drop filter memory (seek / stop / track change) so audio from before
    /// the discontinuity cannot ring into the new position, release the
    /// limiter — there is no transient left to ride out — and clear the
    /// shared meter.
    ///
    /// The meter is cleared here and not only on the next packet because
    /// there may not be a next packet: the sink stops on pause, and a
    /// reading frozen at the last loud packet before the user hit space is
    /// not "current gain reduction", it is a leftover.
    pub fn reset(&mut self) {
        self.reset_state();
        self.published = None;
        self.eq.clear_limiting();
    }

    fn reset_state(&mut self) {
        self.limiter.reset();
        for band in &mut self.filters {
            for channel in band {
                channel.reset_state();
            }
        }
    }

    /// Gain the limiter is applying right now. Exposed for tests that watch
    /// the release rather than the audio it smooths.
    pub fn limiter_gain(&self) -> f64 {
        self.limiter.gain()
    }

    /// Number of coefficient rebuilds so far. Only the generation counter
    /// should drive this — a rebuild per packet would be a real cost.
    pub fn rebuilds(&self) -> u64 {
        self.rebuilds
    }

    /// Filter one interleaved buffer in place. Returns `false` when the
    /// buffer was left untouched (flat curve).
    pub fn process(&mut self, interleaved: &mut [f64]) -> bool {
        let generation = self.eq.generation();
        if generation != self.generation {
            self.rebuild(self.eq.curve());
            self.generation = generation;
            // `set_bands` clears the shared meter, so the dedup cache would
            // otherwise claim a value the meter no longer holds and suppress
            // the store that puts it back.
            self.published = None;
        }
        if !self.active {
            self.publish(generation, EqLimiting::IDLE);
            return false;
        }
        for frame in interleaved.chunks_mut(self.channels) {
            let mut peak = 0.0_f64;
            // Tracked separately because `f64::max` returns the *non*-NaN
            // operand, so a blown-up sample would leave `peak` looking fine.
            let mut finite = true;
            for (channel, sample) in frame.iter_mut().enumerate() {
                let mut value = *sample;
                for band in &mut self.filters {
                    value = band[channel].run(value);
                }
                finite &= value.is_finite();
                *sample = value;
                peak = peak.max(value.abs());
            }
            if !finite {
                // An IIR that has gone non-finite stays that way: its own
                // state feeds the next output. Flush the frame and the
                // filters rather than emit NaN for the rest of the track.
                frame.fill(0.0);
                self.reset_state();
                continue;
            }
            // One gain per frame, so the two channels never drift apart.
            let gain = self.limiter.frame_gain(peak);
            if gain < 1.0 {
                for sample in frame.iter_mut() {
                    *sample *= gain;
                }
            }
        }
        // Where the limiter ended the packet, not the deepest point in it.
        // The release runs at ~90% per 120 ms against a ~46 ms packet, so a
        // transient anywhere in the packet is still visible at its end; the
        // packet's minimum would instead hold a spike for a whole packet
        // after the limiter had already let go of it.
        let limiting = EqLimiting::from_reduction_db(reduction_db(self.limiter.gain()) as f32);
        self.publish(generation, limiting);
        true
    }

    /// Store a reading tagged with the generation the packet was filtered
    /// under, so a curve change that lands mid-packet wins.
    fn publish(&mut self, generation: u64, limiting: EqLimiting) {
        if self.published != Some(limiting) {
            self.eq.publish_limiting(generation, limiting);
            self.published = Some(limiting);
        }
    }

    /// Load a published curve's coefficients.
    fn rebuild(&mut self, curve: EqCurve) {
        self.rebuilds += 1;
        let was_active = self.active;
        let gains = spotuify_core::EqBands::from_tenths(curve.bands).db();
        self.active = !curve.is_flat();
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
        // Coming back from a bypass, the filters still hold samples from
        // whenever the EQ was last on, and the limiter a reduction from a
        // transient that is now minutes old. Start clean; mid-curve tweaks
        // keep their state so a nudge doesn't click.
        if self.active && !was_active {
            self.reset_state();
        }
    }
}

impl Drop for EqStage {
    /// librespot can drop a running chain without calling `stop` — a sink
    /// rebuild after a panic, or a reconnect. Nothing else would clear the
    /// meter until the replacement stage sees audio, so `spotuify eq` would
    /// report a reduction from a sink that no longer exists.
    fn drop(&mut self) {
        self.eq.clear_limiting();
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
    use crate::backends::limiter::THRESHOLD_DBFS;
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

    fn peak(samples: &[f64]) -> f64 {
        samples.iter().fold(0.0_f64, |acc, s| acc.max(s.abs()))
    }

    /// Filter a continuous signal and return the second half, by which point
    /// the biquads' startup transient has decayed.
    ///
    /// The two halves come from ONE signal and each is processed once, so
    /// the measured half is phase-continuous with the state the filters
    /// already hold. Re-processing the same buffer twice would filter it
    /// twice and measure the square of the response.
    fn settled(stage: &mut EqStage, mut signal: Vec<f64>) -> Vec<f64> {
        let half = signal.len() / 2 / CHANNELS * CHANNELS;
        let (prime, measure) = signal.split_at_mut(half);
        stage.process(prime);
        stage.process(measure);
        measure.to_vec()
    }

    fn settled_sine(stage: &mut EqStage, hz: f64, amplitude: f64) -> Vec<f64> {
        settled(stage, sine(hz, SAMPLE_RATE as usize / 2, amplitude))
    }

    /// RMS of `hz` after the EQ settles, relative to the same signal in.
    fn steady_state_gain(stage: &mut EqStage, hz: f64) -> f64 {
        let reference = sine(hz, SAMPLE_RATE as usize / 4, 0.5);
        rms(&settled_sine(stage, hz, 0.5)) / rms(&reference)
    }

    /// Equal-amplitude sines at the ten band centres: equal energy per
    /// (roughly log-spaced) band, i.e. pink-ish, and nothing like a single
    /// tone that could sit in one preset's null.
    fn pink_ish(frames: usize, target_rms_db: f64) -> Vec<f64> {
        let raw: Vec<f64> = (0..frames)
            .map(|frame| {
                let t = frame as f64 / f64::from(SAMPLE_RATE);
                EQ_FREQUENCIES_HZ
                    .iter()
                    .enumerate()
                    .map(|(index, hz)| {
                        // Detuned phases so the partials do not all peak on
                        // the same sample and make a click train.
                        let phase = index as f64 * 0.7;
                        (t * f64::from(*hz) * std::f64::consts::TAU + phase).sin()
                    })
                    .sum::<f64>()
            })
            .collect();
        let scale = 10.0_f64.powf(target_rms_db / 20.0) / rms(&raw);
        raw.into_iter()
            .flat_map(|s| [s * scale, s * scale])
            .collect()
    }

    fn stage_and_eq(settings: &EqSettings) -> (EqStage, SharedEq) {
        let eq = SharedEq::new();
        eq.set_bands(settings.bands_tenths());
        let stage = EqStage::new(CHANNELS, SAMPLE_RATE, eq.clone());
        (stage, eq)
    }

    #[test]
    fn a_flat_curve_is_a_byte_identical_passthrough() {
        let (mut stage, eq) = stage_and_eq(&EqSettings::flat());
        let input = sine(1_000.0, 2_048, 0.5);
        let mut buffer = input.clone();
        assert!(!stage.process(&mut buffer), "flat curve must bypass");
        assert_eq!(buffer, input);
        assert!(eq.meter().limiting().is_idle());
    }

    #[test]
    fn a_boosted_band_lifts_its_own_frequency_and_leaves_the_others() {
        let settings = EqSettings::flat().with_band(4, 12.0);
        let (mut stage, _) = stage_and_eq(&settings);

        // 0.2 amplitude keeps +12 dB clear of the ceiling, so this measures
        // the filters rather than the limiter.
        let reference = sine(1_000.0, SAMPLE_RATE as usize / 4, 0.2);
        let at_1k = rms(&settled_sine(&mut stage, 1_000.0, 0.2)) / rms(&reference);
        assert!(
            (at_1k - 4.0).abs() < 0.4,
            "1 kHz gain {at_1k}x, expected ~4x (+12 dB)"
        );

        let reference = sine(100.0, SAMPLE_RATE as usize / 4, 0.2);
        let at_100 = rms(&settled_sine(&mut stage, 100.0, 0.2)) / rms(&reference);
        assert!(
            (at_100 - 1.0).abs() < 0.1,
            "100 Hz gain {at_100}x, expected ~1x (untouched)"
        );
    }

    #[test]
    fn a_cut_band_attenuates_its_own_frequency() {
        let settings = EqSettings::flat().with_band(4, -12.0);
        let (mut stage, _) = stage_and_eq(&settings);
        let at_1k = steady_state_gain(&mut stage, 1_000.0);
        assert!(
            (at_1k - 0.25).abs() < 0.025,
            "1 kHz gain {at_1k}x, expected ~0.25x (-12 dB)"
        );
    }

    #[test]
    fn a_boost_preset_no_longer_quietens_the_bands_it_does_not_touch() {
        // The bug D036 fixes: the static pre-gain traded the WHOLE curve
        // down by the cascade peak, so picking Bass Boost made even the
        // treble — which the preset does not touch — 8.8 dB quieter.
        // 6 kHz is far enough from the +2 dB at 600 Hz to be genuinely
        // untouched (1 kHz still picks up +1.0 dB of its bleed).
        const UNTOUCHED_HZ: f64 = 6_000.0;
        let (mut flat_stage, _) = stage_and_eq(&EqSettings::flat());
        let flat = steady_state_gain(&mut flat_stage, UNTOUCHED_HZ);

        let bass = EqSettings::from_preset("Bass Boost").unwrap();
        let (mut stage, _) = stage_and_eq(&bass);
        let boosted = steady_state_gain(&mut stage, UNTOUCHED_HZ);

        let delta_db = 20.0 * (boosted / flat).log10();
        assert!(
            delta_db.abs() < 0.5,
            "Bass Boost moved untouched {UNTOUCHED_HZ} Hz by {delta_db:.2} dB; \
             the static pre-gain moved it by -8.8"
        );
    }

    #[test]
    fn a_boost_preset_keeps_a_quiet_broadband_signal_at_its_own_level() {
        // -20 dBFS RMS is a normal listening level and well clear of the
        // ceiling, so this measures the curve, not the limiter. A boost
        // preset must ADD energy in the bands it boosts and leave the rest
        // alone; what it must never do is come out quieter than flat.
        let signal = pink_ish(SAMPLE_RATE as usize / 2, -20.0);
        assert!(
            (20.0 * rms(&signal).log10() + 20.0).abs() < 1e-9,
            "the probe should be -20 dBFS RMS"
        );

        let (mut flat_stage, _) = stage_and_eq(&EqSettings::flat());
        let flat = rms(&settled(&mut flat_stage, signal.clone()));

        for preset in ["Bass Boost", "Rock", "Loudness", "Electronic"] {
            let settings = EqSettings::from_preset(preset).unwrap();
            let (mut stage, eq) = stage_and_eq(&settings);
            let out = rms(&settled(&mut stage, signal.clone()));
            let delta_db = 20.0 * (out / flat).log10();
            assert!(
                delta_db > -1.5,
                "{preset} came out {delta_db:.2} dB below flat; \
                 the static pre-gain put Bass Boost at -8.8"
            );
            // A boost preset legitimately ADDS energy — Rock lifts seven of
            // ten bands, so +6 dB on a pink-ish probe is the curve doing its
            // job. The bound is only here to catch a runaway.
            assert!(
                delta_db < 8.0,
                "{preset} came out {delta_db:.2} dB above flat"
            );
            assert!(
                eq.meter().limiting().is_idle(),
                "{preset} tripped the limiter at -20 dBFS ({}); this level \
                 should be pure filter response",
                eq.meter().limiting()
            );
        }
    }

    #[test]
    fn a_full_scale_sine_through_a_boost_preset_stays_inside_full_scale() {
        // Bass Boost reaches +9.5 dB around 76 Hz, so a full-scale sine
        // there would leave the filters at 3.0 without the limiter.
        let ceiling = 10.0_f64.powf(THRESHOLD_DBFS / 20.0);
        for (name, _) in spotuify_core::EQ_PRESETS {
            let settings = EqSettings::from_preset(name).unwrap();
            if settings.is_flat() {
                // A flat curve is a bypass, limiter included: a full-scale
                // input is meant to come out at full scale.
                continue;
            }
            let (mut stage, _) = stage_and_eq(&settings);
            let out = settled_sine(&mut stage, 76.0, 1.0);
            assert!(
                peak(&out) <= ceiling + 1e-9,
                "{name} peaked at {} for a full-scale 76 Hz sine",
                peak(&out)
            );
            assert!(
                out.iter().all(|s| s.is_finite()),
                "{name} produced a non-finite sample"
            );
        }
    }

    #[test]
    fn the_limiter_engages_on_a_loud_boost_and_stays_idle_on_a_quiet_one() {
        let bass = EqSettings::from_preset("Bass Boost").unwrap();

        let (mut stage, eq) = stage_and_eq(&bass);
        settled_sine(&mut stage, 70.0, 1.0);
        let loud = eq.meter().limiting();
        assert!(
            !loud.is_idle() && loud.as_db() < -8.0,
            "a full-scale 70 Hz sine through Bass Boost should be limited hard, got {loud}"
        );

        // 0.3 amplitude through the same +9.5 dB peak lands at ~0.9, under
        // the ceiling: nothing to limit.
        let (mut stage, eq) = stage_and_eq(&bass);
        let out = settled_sine(&mut stage, 70.0, 0.3);
        assert!(peak(&out) > 0.8, "the probe should be near the ceiling");
        assert!(
            eq.meter().limiting().is_idle(),
            "a 0.3-amplitude sine must not trip the limiter; got {}",
            eq.meter().limiting()
        );
    }

    #[test]
    fn the_limiter_releases_to_unity_once_the_transient_passes() {
        let bass = EqSettings::from_preset("Bass Boost").unwrap();
        let (mut stage, eq) = stage_and_eq(&bass);

        let mut transient = sine(70.0, SAMPLE_RATE as usize / 20, 1.0);
        stage.process(&mut transient);
        assert!(
            stage.limiter_gain() < 0.5,
            "the transient should have pulled the gain down, got {}",
            stage.limiter_gain()
        );

        // 200 ms of near-silence afterwards.
        let mut quiet = vec![0.0; SAMPLE_RATE as usize / 5 * CHANNELS];
        stage.process(&mut quiet);
        let recovered = 20.0 * stage.limiter_gain().log10();
        assert!(
            recovered > -0.2,
            "200 ms after the transient the limiter is still at {recovered:.2} dB"
        );
        assert!(
            eq.meter().limiting().as_db() > -0.2,
            "the meter should have let go too, got {}",
            eq.meter().limiting()
        );
    }

    #[test]
    fn the_limiter_applies_one_gain_to_every_channel_of_a_frame() {
        // A transient on one channel only. If the limiter tracked channels
        // separately, the quiet channel would come through untouched and the
        // stereo image would shift on every peak.
        let bass = EqSettings::from_preset("Bass Boost").unwrap();
        let (mut stage, _) = stage_and_eq(&bass);
        let mut buffer: Vec<f64> = (0..4_096)
            .flat_map(|frame| {
                let t = f64::from(frame) / f64::from(SAMPLE_RATE);
                let left = (t * 70.0 * std::f64::consts::TAU).sin();
                [left, 0.25]
            })
            .collect();
        let before: Vec<f64> = buffer.iter().skip(1).step_by(CHANNELS).copied().collect();
        stage.process(&mut buffer);
        let after: Vec<f64> = buffer.iter().skip(1).step_by(CHANNELS).copied().collect();

        let limited = before
            .iter()
            .zip(&after)
            .filter(|(raw, out)| (*raw - *out).abs() > 1e-9)
            .count();
        assert!(
            limited > 100,
            "only {limited} right-channel frames moved; the gain is not shared"
        );
    }

    #[test]
    fn extreme_curves_stay_finite() {
        for gain in [12.0_f32, -12.0] {
            let settings = EqSettings::from_bands(
                spotuify_core::EqBands::from_db(&[gain; EQ_BAND_COUNT]).unwrap(),
            );
            let (mut stage, _) = stage_and_eq(&settings);
            let mut buffer = sine(50.0, 8_192, 1.0);
            // A step to full scale is the worst case for an IIR's overshoot.
            buffer[0] = 1.0;
            buffer[1] = -1.0;
            for _ in 0..8 {
                stage.process(&mut buffer);
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
            stage.process(&mut buffer);
        }
        assert_eq!(stage.rebuilds(), 1, "steady curve must not rebuild");

        // Setting the same curve again is not a change.
        eq.set_bands(EqSettings::from_preset("rock").unwrap().bands_tenths());
        stage.process(&mut buffer);
        assert_eq!(stage.rebuilds(), 1);

        eq.set_bands(EqSettings::from_preset("Jazz").unwrap().bands_tenths());
        stage.process(&mut buffer);
        stage.process(&mut buffer);
        assert_eq!(stage.rebuilds(), 2);
    }

    #[test]
    fn a_packet_that_finishes_after_a_curve_change_cannot_overwrite_the_fresh_meter() {
        // The race the generation tag exists for. `process` loads the
        // generation once at the top; the curve can move while the packet is
        // still being filtered. Without the tag that packet's reduction
        // lands on a meter `set_bands` had already cleared, and only the
        // NEXT packet repairs it — so if playback stopped there, `eq-get`
        // stayed stale.
        let bass = EqSettings::from_preset("Bass Boost").unwrap();
        let eq = SharedEq::new();
        eq.set_bands(bass.bands_tenths());
        let stale_generation = eq.generation();

        // The curve moves under the in-flight packet.
        eq.set_bands([0; EQ_BAND_COUNT]);
        assert!(eq.meter().limiting().is_idle());
        assert!(eq.generation() > stale_generation);

        // ...and only now does that packet publish what it measured.
        eq.publish_limiting(stale_generation, EqLimiting::from_reduction_db(1.0));
        assert!(
            eq.meter().limiting().is_idle(),
            "a reading from a replaced curve must not land, got {}",
            eq.meter().limiting()
        );

        // A reading under the current curve still lands.
        eq.publish_limiting(eq.generation(), EqLimiting::from_reduction_db(1.0));
        assert_eq!(eq.meter().limiting().as_db(), -1.0);
    }

    #[test]
    fn dropping_a_stage_clears_the_meter() {
        // librespot can drop a running chain without calling `stop` — a sink
        // rebuild after a panic, or a reconnect. Nothing else clears the
        // meter until audio flows again through whatever replaces it.
        let bass = EqSettings::from_preset("Bass Boost").unwrap();
        let (mut stage, eq) = stage_and_eq(&bass);
        settled_sine(&mut stage, 70.0, 1.0);
        assert!(!eq.meter().limiting().is_idle());

        drop(stage);
        assert!(
            eq.meter().limiting().is_idle(),
            "a dropped stage must not leave a reduction behind, got {}",
            eq.meter().limiting()
        );
    }

    #[test]
    fn a_replacement_stage_clears_the_meter_before_any_audio_flows() {
        // The replacement for a dropped stage must not wait for its first
        // packet: between the rebuild and the next buffer, `spotuify eq`
        // would otherwise report the dead sink's reduction.
        let bass = EqSettings::from_preset("Bass Boost").unwrap();
        let eq = SharedEq::new();
        eq.set_bands(bass.bands_tenths());
        eq.publish_limiting(eq.generation(), EqLimiting::from_reduction_db(8.0));
        assert!(!eq.meter().limiting().is_idle());

        let _replacement = EqStage::new(CHANNELS, SAMPLE_RATE, eq.clone());
        assert!(
            eq.meter().limiting().is_idle(),
            "a new stage must clear the meter it inherits, got {}",
            eq.meter().limiting()
        );
    }

    #[test]
    fn a_fresh_stage_corrects_a_meter_the_previous_one_left_limiting() {
        // A reconnect builds a new `EqStage` on the same `SharedEq`. The
        // publish-dedup cache is per stage; the meter is shared. If a new
        // stage trusted its own cache's opening claim of idle, it would
        // suppress the store that corrects the meter, and `eq-get` would
        // report the previous stage's reduction forever.
        let bass = EqSettings::from_preset("Bass Boost").unwrap();
        let (mut old_stage, eq) = stage_and_eq(&bass);
        settled_sine(&mut old_stage, 70.0, 1.0);
        assert!(
            !eq.meter().limiting().is_idle(),
            "the first stage should have left a reduction on the meter"
        );
        drop(old_stage);

        let mut fresh = EqStage::new(CHANNELS, SAMPLE_RATE, eq.clone());
        let mut quiet = sine(1_000.0, 4_096, 0.1);
        fresh.process(&mut quiet);
        assert!(
            eq.meter().limiting().is_idle(),
            "a fresh stage on quiet audio must correct the meter, got {}",
            eq.meter().limiting()
        );
    }

    #[test]
    fn re_enabling_a_curve_releases_the_limiter_instead_of_inheriting_its_gain() {
        // Bypass freezes the limiter wherever the last loud packet left it.
        // Coming back from flat has to start at unity, or the first ~100 ms
        // of the re-enabled curve are attenuated by a stale reduction.
        let bass = EqSettings::from_preset("Bass Boost").unwrap();
        let (mut stage, eq) = stage_and_eq(&bass);
        settled_sine(&mut stage, 70.0, 1.0);
        assert!(stage.limiter_gain() < 0.5, "expected a deep cut");

        eq.set_bands([0; EQ_BAND_COUNT]);
        let mut bypassed = sine(1_000.0, 512, 0.5);
        assert!(!stage.process(&mut bypassed));

        eq.set_bands(bass.bands_tenths());
        let mut quiet = sine(1_000.0, 4_096, 0.1);
        stage.process(&mut quiet);
        assert_eq!(
            stage.limiter_gain(),
            1.0,
            "quiet audio after a re-enable must not be attenuated"
        );
    }

    #[test]
    fn changing_the_curve_clears_the_meter_without_waiting_for_a_packet() {
        // A reading belongs to the curve that produced it. The sink normally
        // corrects the meter on its next packet, but a curve set while
        // playback is paused has no next packet, and the old curve's
        // reduction would sit on `spotuify eq` until playback resumed.
        let bass = EqSettings::from_preset("Bass Boost").unwrap();
        let (mut stage, eq) = stage_and_eq(&bass);
        settled_sine(&mut stage, 70.0, 1.0);
        assert!(!eq.meter().limiting().is_idle());

        eq.set_bands([0; EQ_BAND_COUNT]);
        assert!(
            eq.meter().limiting().is_idle(),
            "the meter should clear on the curve change itself, got {}",
            eq.meter().limiting()
        );

        // ...and the now-bypassed stage keeps it that way.
        let mut buffer = sine(1_000.0, 512, 0.5);
        assert!(!stage.process(&mut buffer));
        assert!(eq.meter().limiting().is_idle());
    }

    #[test]
    fn a_curve_change_lets_the_stage_republish_a_reduction_it_had_already_sent() {
        // `set_bands` clears the shared meter behind the stage's dedup
        // cache. If the cache survived the change it would claim the meter
        // still held the value it had just lost, and suppress the store that
        // puts it back.
        let bass = EqSettings::from_preset("Bass Boost").unwrap();
        let (mut stage, eq) = stage_and_eq(&bass);
        settled_sine(&mut stage, 70.0, 1.0);
        let before = eq.meter().limiting();
        assert!(!before.is_idle());

        // Same curve under a new generation: the stage's limiter carries on
        // from where it was, so it recomputes the same neighbourhood.
        eq.set_bands(EqSettings::from_preset("Hip-Hop").unwrap().bands_tenths());
        eq.set_bands(bass.bands_tenths());
        assert!(eq.meter().limiting().is_idle());

        settled_sine(&mut stage, 70.0, 1.0);
        assert!(
            !eq.meter().limiting().is_idle(),
            "the stage must republish after a curve change cleared the meter"
        );
    }

    #[test]
    fn every_preset_is_realisable_at_44_1_khz() {
        for (name, bands) in spotuify_core::EQ_PRESETS {
            let eq = SharedEq::new();
            eq.set_bands(bands);
            let mut stage = EqStage::new(CHANNELS, SAMPLE_RATE, eq.clone());
            let mut buffer = sine(440.0, 4_096, 0.8);
            stage.process(&mut buffer);
            assert!(
                buffer.iter().all(|s| s.is_finite() && s.abs() <= 1.0),
                "preset {name} produced an out-of-range sample"
            );
        }
    }
}
