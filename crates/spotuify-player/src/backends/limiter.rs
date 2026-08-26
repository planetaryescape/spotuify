//! Peak limiter that sits after the [`super::eq`] filter bank.
//!
//! A boosting EQ curve can push a full-scale input past 1.0. D033 bought
//! that headroom up front, attenuating the whole signal by the cascade's
//! peak response — which made "Bass Boost" 8.8 dB quieter than flat all the
//! time, to prevent clipping that only happens on the loudest bass
//! transients. D036 swaps the constant cost for a conditional one: run at
//! full level and pull the gain down only for the samples that would
//! actually overshoot.
//!
//! Hard ceiling, no lookahead. The gain drops on the *same* sample that
//! exceeds the threshold, so nothing gets through above it; only the
//! recovery is smoothed. A lookahead limiter would round the attack instead
//! of stepping it, at the cost of a delay buffer and latency in a chain that
//! is already fighting for its packet deadline — and the step is on the gain
//! of an already-over-threshold sample, which is the least audible place to
//! put one.

/// Ceiling, in dBFS. Below 0 so the samples that survive the limiter still
/// have room for the integer conversion and any downstream gain.
pub const THRESHOLD_DBFS: f64 = -0.3;

/// Linear form of [`THRESHOLD_DBFS`].
const THRESHOLD: f64 = 0.966_050_878_989_813_3;

/// Time to give back ~90% of the gain reduction once the signal drops back
/// under the ceiling.
///
/// Not a 1/e time constant: the coefficient below is solved for 90%
/// recovery over this window, so "release" reads as the time a listener
/// would say the level came back. Faster than ~50 ms would modulate audibly
/// within one cycle of the bass the limiter mostly catches (70 Hz is a
/// 14 ms period); much slower and one transient ducks the following bar.
pub const RELEASE_MS: f64 = 120.0;

/// Fraction of the reduction still outstanding after [`RELEASE_MS`].
const RELEASE_REMAINDER: f64 = 0.1;

/// Per-sample peak limiter for one interleaved stream.
///
/// One gain for the whole frame, so a transient on the left channel cannot
/// shift the stereo image by ducking one side only.
pub struct Limiter {
    /// Gain applied to the frame being processed. 1.0 when idle.
    gain: f64,
    /// Per-sample release coefficient: `gain += (target - gain) * (1 - a)`.
    release: f64,
}

impl Limiter {
    pub fn new(sample_rate: f64) -> Self {
        // sample_rate arrives from librespot's constant 44_100; guard anyway
        // so a zero can never make the coefficient non-finite on the audio
        // thread.
        let frames = (sample_rate.max(1.0) * RELEASE_MS / 1_000.0).max(1.0);
        Self {
            gain: 1.0,
            release: RELEASE_REMAINDER.powf(1.0 / frames),
        }
    }

    /// Forget the current reduction. Used on the same discontinuities that
    /// reset the filters (seek, stop, track change): there is no transient
    /// left to ride out.
    pub fn reset(&mut self) {
        self.gain = 1.0;
    }

    /// Gain currently applied. 1.0 when the limiter is idle.
    pub fn gain(&self) -> f64 {
        self.gain
    }

    /// Gain to apply to a frame whose loudest channel is `peak` (already
    /// filtered, absolute value). Instantaneous attack, exponential release.
    pub fn frame_gain(&mut self, peak: f64) -> f64 {
        // A non-finite sample means the filters blew up; the caller flushes
        // them. Don't let the NaN into the gain, where it would persist.
        let target = if peak.is_finite() && peak > THRESHOLD {
            THRESHOLD / peak
        } else {
            1.0
        };
        if target <= self.gain {
            self.gain = target;
        } else {
            self.gain += (target - self.gain) * (1.0 - self.release);
        }
        self.gain
    }
}

/// Gain reduction, in dB, that `gain` represents. `0.0` for unity or above.
pub fn reduction_db(gain: f64) -> f64 {
    if gain >= 1.0 || gain <= 0.0 {
        0.0
    } else {
        -20.0 * gain.log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: f64 = 44_100.0;

    #[test]
    fn the_threshold_constant_matches_its_decibel_value() {
        let expected = 10.0_f64.powf(THRESHOLD_DBFS / 20.0);
        assert!(
            (THRESHOLD - expected).abs() < 1e-8,
            "THRESHOLD {THRESHOLD} is not {THRESHOLD_DBFS} dBFS ({expected})"
        );
    }

    #[test]
    fn a_signal_under_the_ceiling_is_left_alone() {
        let mut limiter = Limiter::new(SAMPLE_RATE);
        for _ in 0..1_000 {
            assert_eq!(limiter.frame_gain(0.5), 1.0);
        }
        assert_eq!(reduction_db(limiter.gain()), 0.0);
    }

    #[test]
    fn the_first_over_threshold_sample_is_already_pulled_to_the_ceiling() {
        // No lookahead means the attack has to be instantaneous, or the
        // sample that triggered the limiter escapes above the ceiling.
        let mut limiter = Limiter::new(SAMPLE_RATE);
        let gain = limiter.frame_gain(2.0);
        assert!(
            (2.0 * gain - THRESHOLD).abs() < 1e-12,
            "peak 2.0 left at {} after one sample",
            2.0 * gain
        );
    }

    #[test]
    fn release_gives_back_most_of_the_reduction_within_its_window() {
        let mut limiter = Limiter::new(SAMPLE_RATE);
        limiter.frame_gain(4.0);
        let reduced = limiter.gain();
        assert!(reduced < 0.3, "expected a deep cut, got {reduced}");

        let frames = (SAMPLE_RATE * RELEASE_MS / 1_000.0) as usize;
        for _ in 0..frames {
            limiter.frame_gain(0.1);
        }
        let outstanding = 1.0 - limiter.gain();
        assert!(
            outstanding <= (1.0 - reduced) * RELEASE_REMAINDER * 1.05,
            "after {RELEASE_MS} ms the gain is {}, still {outstanding} short",
            limiter.gain()
        );
    }

    #[test]
    fn release_never_overshoots_unity() {
        let mut limiter = Limiter::new(SAMPLE_RATE);
        limiter.frame_gain(4.0);
        for _ in 0..SAMPLE_RATE as usize {
            let gain = limiter.frame_gain(0.0);
            assert!((0.0..=1.0).contains(&gain), "gain {gain} left [0, 1]");
        }
    }

    #[test]
    fn a_non_finite_peak_does_not_poison_the_gain() {
        let mut limiter = Limiter::new(SAMPLE_RATE);
        for peak in [f64::NAN, f64::INFINITY] {
            limiter.reset();
            assert_eq!(limiter.frame_gain(peak), 1.0);
            assert!(limiter.gain().is_finite());
        }
    }

    #[test]
    fn reduction_db_reports_the_cut_as_a_positive_magnitude() {
        assert_eq!(reduction_db(1.0), 0.0);
        assert_eq!(reduction_db(2.0), 0.0);
        assert_eq!(reduction_db(0.0), 0.0);
        assert!((reduction_db(0.5) - 6.0206).abs() < 1e-3);
    }
}
