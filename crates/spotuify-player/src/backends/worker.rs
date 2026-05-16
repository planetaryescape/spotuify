//! Phase 9.3c — embedded backend worker loop.
//!
//! The worker fans in commands, librespot events, and a periodic
//! tick into a single `tokio::select!`. ncspot's pattern is followed:
//! the tick only fires when playing, saving CPU when paused.
//!
//! Generic over the command + librespot-event type so tests can
//! drive it with synthetic enums.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::{interval, Interval, MissedTickBehavior};

use crate::backends::clock::{Clock, PlaybackPhase};

pub const POSITION_TICK_INTERVAL: Duration = Duration::from_millis(400);

/// Worker output events. The daemon translates these into wire-level
/// `DaemonEvent`s through the same path used by ConnectOnly /
/// Spotifyd.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerOut {
    Tick { position_ms: u32 },
    Shutdown,
}

/// Worker input commands. Each command mutates `WorkerState` and may
/// emit a `WorkerOut`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerIn {
    Play { position_ms: u32 },
    Pause,
    Seek { position_ms: u32 },
    Stop,
    Shutdown,
}

pub struct WorkerState {
    pub phase: PlaybackPhase,
    pub clock: Box<dyn Clock>,
}

impl WorkerState {
    pub fn new(clock: Box<dyn Clock>) -> Self {
        Self {
            phase: PlaybackPhase::Stopped,
            clock,
        }
    }

    pub fn is_playing(&self) -> bool {
        matches!(self.phase, PlaybackPhase::Playing { .. })
    }

    pub fn handle(&mut self, cmd: &WorkerIn) {
        match cmd {
            WorkerIn::Play { position_ms } => {
                self.phase = PlaybackPhase::playing_from(self.clock.as_ref(), *position_ms);
            }
            WorkerIn::Pause => {
                if let PlaybackPhase::Playing { .. } = self.phase {
                    let now = crate::backends::clock::derived_position_ms(
                        self.clock.as_ref(),
                        self.phase,
                    );
                    self.phase = PlaybackPhase::paused_at(now);
                }
            }
            WorkerIn::Seek { position_ms } => match self.phase {
                PlaybackPhase::Playing { .. } => {
                    self.phase = PlaybackPhase::playing_from(self.clock.as_ref(), *position_ms);
                }
                PlaybackPhase::Paused { .. } | PlaybackPhase::Stopped => {
                    self.phase = PlaybackPhase::paused_at(*position_ms);
                }
            },
            WorkerIn::Stop => {
                self.phase = PlaybackPhase::Stopped;
            }
            WorkerIn::Shutdown => {}
        }
    }
}

/// Spawn the worker loop. Returns the join handle so callers can
/// await teardown.
pub async fn run_worker(
    mut state: WorkerState,
    mut commands: mpsc::UnboundedReceiver<WorkerIn>,
    out: mpsc::UnboundedSender<WorkerOut>,
) {
    let mut tick = build_tick();
    loop {
        tokio::select! {
            cmd = commands.recv() => match cmd {
                Some(WorkerIn::Shutdown) | None => {
                    let _ = out.send(WorkerOut::Shutdown);
                    break;
                }
                Some(cmd) => {
                    state.handle(&cmd);
                }
            },
            _ = tick.tick(), if state.is_playing() => {
                let pos = crate::backends::clock::derived_position_ms(state.clock.as_ref(), state.phase);
                if out.send(WorkerOut::Tick { position_ms: pos }).is_err() {
                    break;
                }
            }
        }
    }
}

fn build_tick() -> Interval {
    let mut t = interval(POSITION_TICK_INTERVAL);
    t.set_missed_tick_behavior(MissedTickBehavior::Skip);
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::clock::Clock;
    use parking_lot::Mutex;
    use std::time::Instant;

    struct FakeClock {
        now: Mutex<Instant>,
    }

    impl FakeClock {
        fn boxed() -> Box<Self> {
            Box::new(Self {
                now: Mutex::new(Instant::now()),
            })
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            *self.now.lock()
        }
    }

    #[test]
    fn pause_freezes_position_via_state_machine() {
        let mut state = WorkerState::new(FakeClock::boxed());
        state.handle(&WorkerIn::Play { position_ms: 1_000 });
        assert!(state.is_playing());
        state.handle(&WorkerIn::Pause);
        assert!(!state.is_playing());
        // After Pause, a follow-up Pause is a no-op.
        state.handle(&WorkerIn::Pause);
        assert!(!state.is_playing());
    }

    #[test]
    fn seek_while_playing_keeps_playing_with_new_baseline() {
        let mut state = WorkerState::new(FakeClock::boxed());
        state.handle(&WorkerIn::Play { position_ms: 0 });
        state.handle(&WorkerIn::Seek {
            position_ms: 30_000,
        });
        assert!(state.is_playing());
        let pos = crate::backends::clock::derived_position_ms(state.clock.as_ref(), state.phase);
        assert_eq!(pos, 30_000);
    }

    #[test]
    fn seek_while_paused_stays_paused_at_new_position() {
        // Adversarial: a seek must NOT auto-resume playback. Catches
        // the bug where Seek always promotes to Playing.
        let mut state = WorkerState::new(FakeClock::boxed());
        state.handle(&WorkerIn::Pause);
        state.handle(&WorkerIn::Seek {
            position_ms: 12_345,
        });
        assert!(!state.is_playing());
        let pos = crate::backends::clock::derived_position_ms(state.clock.as_ref(), state.phase);
        assert_eq!(pos, 12_345);
    }

    #[test]
    fn stop_clears_phase_to_stopped() {
        let mut state = WorkerState::new(FakeClock::boxed());
        state.handle(&WorkerIn::Play { position_ms: 5_000 });
        state.handle(&WorkerIn::Stop);
        assert!(matches!(state.phase, PlaybackPhase::Stopped));
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_command_exits_loop_and_emits_shutdown_event() {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let state = WorkerState::new(FakeClock::boxed());

        let handle = tokio::spawn(run_worker(state, cmd_rx, out_tx));
        cmd_tx
            .send(WorkerIn::Shutdown)
            .expect("shutdown command should send");

        let observed = out_rx
            .recv()
            .await
            .expect("worker should emit shutdown event");
        assert_eq!(observed, WorkerOut::Shutdown);
        handle.await.expect("worker task should join");
    }

    #[tokio::test(start_paused = true)]
    async fn ticks_only_fire_while_playing() {
        // Adversarial: this is the test that catches the bug where
        // someone drops `if state.is_playing()` from the select arm.
        // Without the guard, paused playback would still emit ticks
        // and drain CPU.
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let state = WorkerState::new(FakeClock::boxed());
        let handle = tokio::spawn(run_worker(state, cmd_rx, out_tx));

        // Stay paused. Advance virtual time well past two intervals.
        tokio::time::advance(Duration::from_millis(2_500)).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(10), out_rx.recv())
                .await
                .is_err(),
            "tick must not fire while paused"
        );

        // Start playing → ticks resume.
        cmd_tx
            .send(WorkerIn::Play { position_ms: 0 })
            .expect("play command should send");
        tokio::time::advance(Duration::from_millis(500)).await;
        let tick = tokio::time::timeout(Duration::from_secs(1), out_rx.recv())
            .await
            .expect("expected tick after Play")
            .expect("worker channel should stay open");
        assert!(
            matches!(tick, WorkerOut::Tick { .. }),
            "expected Tick, got {tick:?}"
        );

        cmd_tx
            .send(WorkerIn::Shutdown)
            .expect("shutdown command should send");
        let _ = out_rx.recv().await;
        handle.await.expect("worker task should join");
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_command_sender_exits_loop_cleanly() {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let state = WorkerState::new(FakeClock::boxed());
        let handle = tokio::spawn(run_worker(state, cmd_rx, out_tx));

        // Adversarial: a parent task that simply drops the command
        // sender must NOT leak the worker. Catches the regression
        // where the loop only exits on an explicit Shutdown command.
        drop(cmd_tx);
        let observed = out_rx
            .recv()
            .await
            .expect("worker should emit shutdown when sender drops");
        assert_eq!(observed, WorkerOut::Shutdown);
        handle.await.expect("worker task should join");
    }
}
