//! Phase 9.3a — RecoveringSink contract tests.
//!
//! Wraps an audio backend Sink in `catch_unwind` so a panic (AirPods
//! disconnect on macOS, PipeWire restart on Linux) doesn't take the
//! daemon down. Adversarial focus:
//! - Each of `start`, `stop`, `write` is guarded independently — a
//!   regression that drops the wrapper from one of those call sites
//!   shows up immediately as an escaped panic.
//! - Reconstruction is real (instantiation counter increments).
//! - The panic budget bail-out fires after the documented limit so
//!   a stuck audio backend can't spin the daemon forever.
//! - Non-panic `Err` results propagate unchanged — catch_unwind must
//!   not swallow them or downgrade them to `Ok`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use spotuify_player::backends::recovering_sink::{RecoveringSink, Sink, SinkBudget, SinkError};

/// Sink double whose behavior is parameterised. Lets each test pick
/// which call panics on which invocation.
struct ScriptedSink {
    instantiations: Arc<AtomicUsize>,
    panic_on: PanicPlan,
    write_calls: usize,
    start_calls: usize,
    stop_calls: usize,
    return_err_on_write: Option<SinkError>,
}

#[derive(Debug, Clone, Copy)]
enum PanicPlan {
    Never,
    WriteEvery,
    WriteOnce,
    StartOnce,
    StopOnce,
}

impl ScriptedSink {
    fn new(plan: PanicPlan, counter: Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self {
            instantiations: counter,
            panic_on: plan,
            write_calls: 0,
            start_calls: 0,
            stop_calls: 0,
            return_err_on_write: None,
        }
    }
}

impl Sink for ScriptedSink {
    fn start(&mut self) -> Result<(), SinkError> {
        self.start_calls += 1;
        if matches!(self.panic_on, PanicPlan::StartOnce) && self.start_calls == 1 {
            panic!("scripted start panic");
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<(), SinkError> {
        self.stop_calls += 1;
        if matches!(self.panic_on, PanicPlan::StopOnce) && self.stop_calls == 1 {
            panic!("scripted stop panic");
        }
        Ok(())
    }

    fn write(&mut self, _frames: &[i16]) -> Result<(), SinkError> {
        self.write_calls += 1;
        if let Some(err) = self.return_err_on_write.take() {
            return Err(err);
        }
        match self.panic_on {
            PanicPlan::WriteEvery => panic!("scripted write panic (every)"),
            PanicPlan::WriteOnce if self.write_calls == 1 => {
                panic!("scripted write panic (once)")
            }
            _ => Ok(()),
        }
    }
}

/// Factory that produces a panicking sink ONCE, then non-panicking
/// replacements for every reconstruction. Models the real failure
/// mode (a one-off device disconnect that the next sink instance
/// can recover from).
fn one_shot_factory(plan: PanicPlan, counter: Arc<AtomicUsize>) -> impl FnMut() -> ScriptedSink {
    let mut produced = 0;
    move || {
        let this_plan = if produced == 0 {
            plan
        } else {
            PanicPlan::Never
        };
        produced += 1;
        ScriptedSink::new(this_plan, counter.clone())
    }
}

/// Factory that produces a sink that panics on EVERY write across
/// every reconstruction. Used to drive budget exhaustion.
fn always_factory(plan: PanicPlan, counter: Arc<AtomicUsize>) -> impl FnMut() -> ScriptedSink {
    move || ScriptedSink::new(plan, counter.clone())
}

#[test]
fn happy_path_no_panic_no_reconstruction() {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut sink = RecoveringSink::new(
        one_shot_factory(PanicPlan::Never, counter.clone()),
        SinkBudget::default(),
    );
    sink.start().unwrap();
    sink.write(&[0, 1, 2]).unwrap();
    sink.write(&[3, 4]).unwrap();
    sink.stop().unwrap();
    assert_eq!(counter.load(Ordering::SeqCst), 1, "no reconstruction");
}

#[test]
fn panic_on_write_reconstructs_inner_and_subsequent_write_succeeds() {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut sink = RecoveringSink::new(
        one_shot_factory(PanicPlan::WriteOnce, counter.clone()),
        SinkBudget::default(),
    );

    // First write panics; RecoveringSink absorbs and surfaces as Err.
    let first = sink.write(&[0; 64]);
    assert!(
        matches!(first, Err(SinkError::Recovered(_))),
        "got {first:?}"
    );

    // Second write hits a fresh sink and succeeds.
    let second = sink.write(&[0; 64]);
    assert!(second.is_ok(), "got {second:?}");
    assert!(
        counter.load(Ordering::SeqCst) >= 2,
        "expected reconstruction, counter = {}",
        counter.load(Ordering::SeqCst)
    );
}

#[test]
fn panic_on_start_is_recovered() {
    // Adversarial: if RecoveringSink only wraps write(), a start()
    // panic on first call would escape and crash the daemon.
    let counter = Arc::new(AtomicUsize::new(0));
    let mut sink = RecoveringSink::new(
        one_shot_factory(PanicPlan::StartOnce, counter.clone()),
        SinkBudget::default(),
    );

    let first = sink.start();
    assert!(
        matches!(first, Err(SinkError::Recovered(_))),
        "start panic should surface as Recovered, got {first:?}"
    );

    // After reconstruction, start succeeds.
    sink.start().unwrap();
    assert!(counter.load(Ordering::SeqCst) >= 2);
}

#[test]
fn panic_on_stop_is_recovered() {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut sink = RecoveringSink::new(
        one_shot_factory(PanicPlan::StopOnce, counter.clone()),
        SinkBudget::default(),
    );

    let first = sink.stop();
    assert!(
        matches!(first, Err(SinkError::Recovered(_))),
        "stop panic should surface as Recovered, got {first:?}"
    );

    sink.stop().unwrap();
    assert!(counter.load(Ordering::SeqCst) >= 2);
}

#[test]
fn repeated_panics_exhaust_budget_and_return_degraded() {
    // Adversarial: a stuck audio backend that panics every write
    // shouldn't loop forever. After N consecutive panics in a window
    // the wrapper gives up and surfaces Degraded — daemon converts
    // this to PlayerDegraded.
    let counter = Arc::new(AtomicUsize::new(0));
    let budget = SinkBudget {
        max_panics: 3,
        ..SinkBudget::default()
    };
    let mut sink = RecoveringSink::new(always_factory(PanicPlan::WriteEvery, counter), budget);

    let mut errors = Vec::new();
    for _ in 0..5 {
        errors.push(sink.write(&[0; 16]));
    }

    // The first `max_panics` writes are Recovered, after that all are Degraded.
    assert!(
        errors
            .iter()
            .any(|r| matches!(r, Err(SinkError::Degraded { .. }))),
        "expected at least one Degraded after budget exhaustion, got {errors:?}"
    );
}

#[test]
fn non_panic_error_propagates_unchanged_without_reconstruction() {
    // Adversarial: catch_unwind must NOT swallow normal errors. If
    // the inner sink returns Err(Underflow) the wrapper should hand
    // it back as Err(Underflow) — not "Recovered" — and must not
    // tear down the sink.
    let counter = Arc::new(AtomicUsize::new(0));
    let factory = {
        let counter = counter.clone();
        move || {
            let mut sink = ScriptedSink::new(PanicPlan::Never, counter.clone());
            sink.return_err_on_write = Some(SinkError::Backend("underflow".to_string()));
            sink
        }
    };
    let mut sink = RecoveringSink::new(factory, SinkBudget::default());
    let result = sink.write(&[0; 8]);

    assert!(
        matches!(result, Err(SinkError::Backend(ref msg)) if msg == "underflow"),
        "non-panic error should propagate unchanged, got {result:?}"
    );
    // No reconstruction triggered.
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}
