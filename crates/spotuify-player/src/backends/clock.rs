//! Phase 9.3b — `Clock` trait + position derivation.
//!
//! Position is *derived* from a baseline + elapsed wall-clock time,
//! not ticked. ncspot's `spotify.rs:307-313` pattern: instead of
//! incrementing a counter on every event, store `(playback_start,
//! position_at_start)` and compute `now - start + position_at_start`
//! on read.
//!
//! Pros:
//! - No off-by-one bugs from missed/duplicate ticks.
//! - Resilient to interval skew under load.
//! - Easy to seek: just rewrite the baseline.
//!
//! The `Clock` trait makes time-dependent tests deterministic; the
//! NTP-step test (clock goes backwards 1ms) catches the
//! `Duration::since` panic.

use std::time::{Duration, Instant};

/// Abstracts the monotonic clock so tests can inject a fake.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

/// Production impl — wraps `std::time::Instant::now()`.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackPhase {
    Stopped,
    Playing {
        started_at: Instant,
        baseline_ms: u32,
    },
    Paused {
        position_ms: u32,
    },
}

impl PlaybackPhase {
    pub fn stopped() -> Self {
        Self::Stopped
    }

    pub fn paused_at(position_ms: u32) -> Self {
        Self::Paused { position_ms }
    }

    pub fn playing_from(clock: &dyn Clock, position_ms: u32) -> Self {
        Self::Playing {
            started_at: clock.now(),
            baseline_ms: position_ms,
        }
    }
}

/// Derive current playback position. Returns 0 when stopped, the
/// frozen value when paused, baseline + elapsed when playing.
///
/// Clamps a backwards-running clock (NTP step) to the baseline so we
/// never return `u32::MAX` from `Duration::since` underflow.
pub fn derived_position_ms(clock: &dyn Clock, phase: PlaybackPhase) -> u32 {
    match phase {
        PlaybackPhase::Stopped => 0,
        PlaybackPhase::Paused { position_ms } => position_ms,
        PlaybackPhase::Playing {
            started_at,
            baseline_ms,
        } => {
            let now = clock.now();
            let elapsed = if now >= started_at {
                now.duration_since(started_at)
            } else {
                // NTP step backwards or test-clock weirdness — clamp.
                Duration::ZERO
            };
            let elapsed_ms = u32::try_from(elapsed.as_millis()).unwrap_or(u32::MAX);
            baseline_ms.saturating_add(elapsed_ms)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    struct FakeClock {
        now: Mutex<Instant>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                now: Mutex::new(Instant::now()),
            }
        }

        fn advance(&self, by: Duration) {
            let mut now = self.now.lock();
            *now = now.checked_add(by).unwrap_or(*now);
        }

        fn rewind(&self, by: Duration) {
            let mut now = self.now.lock();
            *now = now.checked_sub(by).unwrap_or(*now);
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            *self.now.lock()
        }
    }

    #[test]
    fn stopped_returns_zero() {
        let clock = FakeClock::new();
        assert_eq!(derived_position_ms(&clock, PlaybackPhase::Stopped), 0);
    }

    #[test]
    fn paused_freezes_position_through_clock_advance() {
        let clock = FakeClock::new();
        clock.advance(Duration::from_secs(5));
        assert_eq!(
            derived_position_ms(&clock, PlaybackPhase::paused_at(12_000)),
            12_000
        );
    }

    #[test]
    fn playing_advances_position_by_elapsed_wall_clock() {
        let clock = FakeClock::new();
        let phase = PlaybackPhase::playing_from(&clock, 12_000);
        clock.advance(Duration::from_millis(2_500));
        let pos = derived_position_ms(&clock, phase);
        // Tolerance: 1ms either way. FakeClock is deterministic so
        // this should land exactly on 14_500 but a robust assertion
        // also catches integer-cast rounding bugs.
        assert!((14_499..=14_501).contains(&pos), "got {pos}");
    }

    #[test]
    fn seek_while_playing_re_baselines_immediately() {
        // Adversarial: a seek while playing must update the baseline.
        // A bug where seek only stored "new position" without
        // resetting `started_at` would let position drift back to
        // pre-seek values.
        let clock = FakeClock::new();
        clock.advance(Duration::from_secs(10));
        let after_seek = PlaybackPhase::playing_from(&clock, 30_000);
        assert_eq!(derived_position_ms(&clock, after_seek), 30_000);
    }

    #[test]
    fn ntp_backward_step_clamps_to_baseline_no_panic_no_underflow() {
        // Adversarial: this is the bug that would surface as a
        // `Duration::since` panic on a non-monotonic clock. Without
        // the saturating arithmetic we'd return u32::MAX.
        let clock = FakeClock::new();
        let phase = PlaybackPhase::playing_from(&clock, 5_000);
        clock.rewind(Duration::from_millis(1));
        let pos = derived_position_ms(&clock, phase);
        assert_eq!(pos, 5_000, "NTP step must clamp to baseline, got {pos}");
    }

    #[test]
    fn long_playback_does_not_overflow_u32() {
        // Adversarial: 90 minutes of playback = 5.4M ms, well under
        // u32::MAX (4.29B). Just confirm the saturating cast keeps
        // pathological clocks finite.
        let clock = FakeClock::new();
        let phase = PlaybackPhase::playing_from(&clock, 0);
        clock.advance(Duration::from_secs(60 * 60 * 24)); // 24 hours
        let pos = derived_position_ms(&clock, phase);
        assert!(pos > 0 && pos != u32::MAX);
    }
}
