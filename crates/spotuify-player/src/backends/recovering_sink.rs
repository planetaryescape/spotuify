//! Phase 9.3a — RecoveringSink.
//!
//! Wraps an audio backend Sink in `catch_unwind` so a panic doesn't
//! crash the daemon. The classic failure modes we're guarding:
//! - macOS PortAudio + AirPods disconnect mid-track → SIGSEGV inside
//!   the sink's `write()`.
//! - Linux PipeWire restart → the underlying sink errors then
//!   panics.
//!
//! Design choices:
//! - Generic over a small local `Sink` trait so test doubles can
//!   inject panics without dragging in real audio hardware.
//! - Each of `start`, `stop`, `write` is wrapped independently.
//! - A panic budget bounds the recovery: after N panics within a
//!   sliding window we surface `Degraded` instead of looping forever.
//! - Non-panic `Err` is propagated verbatim; only panics trigger the
//!   reconstruction path.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::{Duration, Instant};

use thiserror::Error;
use tracing::warn;

/// Minimal Sink trait — production audio backends in 9.5 will adapt
/// their concrete sink types to this interface so RecoveringSink can
/// wrap them uniformly.
pub trait Sink: Send + 'static {
    fn start(&mut self) -> Result<(), SinkError>;
    fn stop(&mut self) -> Result<(), SinkError>;
    fn write(&mut self, frames: &[i16]) -> Result<(), SinkError>;
}

#[derive(Debug, Error)]
pub enum SinkError {
    /// Inner sink returned an error normally (e.g. underflow, EOF).
    /// Propagated verbatim — RecoveringSink does NOT reconstruct in
    /// this case.
    #[error("audio backend error: {0}")]
    Backend(String),

    /// Inner sink panicked. RecoveringSink swallowed it, dropped the
    /// inner, and will lazily reconstruct on the next call.
    #[error("audio backend panicked (recovered): {0}")]
    Recovered(String),

    /// Panic budget exhausted. The daemon should surface
    /// PlayerDegraded and stop hitting the sink.
    #[error("audio backend degraded: {panics} panics in {window:?}")]
    Degraded { panics: u32, window: Duration },
}

/// Recovery budget. Defaults match the recommendation in the phase 9
/// implementation doc: 5 panics in 30 seconds before bail-out.
#[derive(Debug, Clone, Copy)]
pub struct SinkBudget {
    pub max_panics: u32,
    pub window: Duration,
}

impl Default for SinkBudget {
    fn default() -> Self {
        Self {
            max_panics: 5,
            window: Duration::from_secs(30),
        }
    }
}

pub struct RecoveringSink<S: Sink, F: FnMut() -> S> {
    factory: F,
    inner: Option<S>,
    budget: SinkBudget,
    panic_marks: Vec<Instant>,
    degraded: bool,
}

impl<S: Sink, F: FnMut() -> S> RecoveringSink<S, F> {
    pub fn new(mut factory: F, budget: SinkBudget) -> Self {
        let inner = factory();
        Self {
            factory,
            inner: Some(inner),
            budget,
            panic_marks: Vec::new(),
            degraded: false,
        }
    }

    fn record_panic_and_check_budget(&mut self) -> bool {
        let now = Instant::now();
        // Drop marks outside the window so the rolling count stays
        // accurate (catches the "non-rolling counter" bug).
        self.panic_marks
            .retain(|t| now.duration_since(*t) <= self.budget.window);
        self.panic_marks.push(now);
        if self.panic_marks.len() as u32 >= self.budget.max_panics {
            self.degraded = true;
            return true;
        }
        false
    }

    fn try_recover(&mut self) -> Option<SinkError> {
        if self.record_panic_and_check_budget() {
            return Some(SinkError::Degraded {
                panics: self.panic_marks.len() as u32,
                window: self.budget.window,
            });
        }
        // Build a fresh sink so the next call has something to talk
        // to. If the factory itself panics there's nothing we can do
        // — propagate as Degraded.
        let next = catch_unwind(AssertUnwindSafe(|| (self.factory)()));
        match next {
            Ok(sink) => {
                self.inner = Some(sink);
                None
            }
            Err(_) => {
                self.degraded = true;
                Some(SinkError::Degraded {
                    panics: self.panic_marks.len() as u32,
                    window: self.budget.window,
                })
            }
        }
    }

    fn guarded<R>(
        &mut self,
        op_name: &'static str,
        op: impl FnOnce(&mut S) -> Result<R, SinkError>,
    ) -> Result<R, SinkError> {
        if self.degraded {
            return Err(SinkError::Degraded {
                panics: self.panic_marks.len() as u32,
                window: self.budget.window,
            });
        }
        // We need the inner sink. If it's missing the wrapper was
        // already torn down once — rebuild before continuing.
        if self.inner.is_none() {
            if let Some(degraded) = self.try_recover() {
                return Err(degraded);
            }
        }
        let mut inner = self.inner.take().expect("inner sink restored above");
        let result = catch_unwind(AssertUnwindSafe(|| op(&mut inner)));
        match result {
            Ok(Ok(value)) => {
                self.inner = Some(inner);
                Ok(value)
            }
            Ok(Err(err)) => {
                // Normal error path: put the sink back, propagate.
                self.inner = Some(inner);
                Err(err)
            }
            Err(payload) => {
                let message = panic_message(payload);
                warn!(op = op_name, error = %message, "audio sink panicked; reconstructing");
                // Drop the panicked inner; rebuild lazily for next call.
                drop(inner);
                if let Some(degraded) = self.try_recover() {
                    Err(degraded)
                } else {
                    Err(SinkError::Recovered(message))
                }
            }
        }
    }

    pub fn start(&mut self) -> Result<(), SinkError> {
        self.guarded("start", |inner| inner.start())
    }

    pub fn stop(&mut self) -> Result<(), SinkError> {
        self.guarded("stop", |inner| inner.stop())
    }

    pub fn write(&mut self, frames: &[i16]) -> Result<(), SinkError> {
        self.guarded("write", |inner| inner.write(frames))
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic with non-string payload".to_string()
    }
}
