//! Embedded librespot sink-chain adapter.
//!
//! `librespot_playback::Player::new` accepts a sink factory returning
//! `Box<dyn librespot_playback::audio_backend::Sink>`. The generic
//! `AudioCounterTap` / `VisualizationTap` wrappers operate on decoded
//! i16 PCM, so this adapter bridges librespot's `AudioPacket` API into
//! those tap handles before delegating the original packet to the real
//! physical backend.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::time::Instant;

use librespot_playback::audio_backend::{Sink, SinkError, SinkResult};
use librespot_playback::config::AudioFormat;
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use spotuify_audio::SharedAnalyzer;
use tracing::warn;

use crate::backends::audio_counter_tap::AudioCounterHandle;
use crate::backends::recovering_sink::SinkBudget;
use crate::backends::visualization_tap::push_i16_samples;

const CHANNELS: usize = 2;

pub fn build_librespot_sink_chain<F>(
    factory: F,
    analyzer: Option<SharedAnalyzer>,
    counter: Arc<AudioCounterHandle>,
    budget: SinkBudget,
) -> Box<dyn Sink>
where
    F: FnMut() -> Box<dyn Sink> + Send + 'static,
{
    Box::new(LibrespotSinkChain::new(factory, analyzer, counter, budget))
}

struct LibrespotSinkChain<F>
where
    F: FnMut() -> Box<dyn Sink>,
{
    factory: F,
    inner: Option<Box<dyn Sink>>,
    analyzer: Option<SharedAnalyzer>,
    counter: Arc<AudioCounterHandle>,
    budget: SinkBudget,
    panic_marks: Vec<Instant>,
    degraded: bool,
    tap_converter: Converter,
}

impl<F> LibrespotSinkChain<F>
where
    F: FnMut() -> Box<dyn Sink>,
{
    fn new(
        mut factory: F,
        analyzer: Option<SharedAnalyzer>,
        counter: Arc<AudioCounterHandle>,
        budget: SinkBudget,
    ) -> Self {
        // The initial build can panic — e.g. PortAudio `could not find device`
        // when the configured output (AirPods) is gone at construction time.
        // librespot calls this on its player thread, so an escaping panic kills
        // it. Catch it: start with `inner: None` and let `guarded()` lazily
        // rebuild (or degrade) on the first op instead of crashing the thread.
        let inner = catch_unwind(AssertUnwindSafe(&mut factory)).ok();
        Self {
            factory,
            inner,
            analyzer,
            counter,
            budget,
            panic_marks: Vec::new(),
            degraded: false,
            tap_converter: Converter::new(None),
        }
    }

    fn record_panic_and_check_budget(&mut self) -> bool {
        let now = Instant::now();
        self.panic_marks
            .retain(|t| now.duration_since(*t) <= self.budget.window);
        self.panic_marks.push(now);
        if self.panic_marks.len() as u32 >= self.budget.max_panics {
            self.degraded = true;
            return true;
        }
        false
    }

    fn degraded_error(&self) -> SinkError {
        SinkError::StateChange(format!(
            "audio backend degraded: {} panics in {:?}",
            self.panic_marks.len(),
            self.budget.window
        ))
    }

    fn try_recover(&mut self) -> Option<SinkError> {
        if self.record_panic_and_check_budget() {
            return Some(self.degraded_error());
        }
        let next = catch_unwind(AssertUnwindSafe(|| (self.factory)()));
        match next {
            Ok(sink) => {
                self.inner = Some(sink);
                None
            }
            Err(_) => {
                self.degraded = true;
                Some(self.degraded_error())
            }
        }
    }

    fn guarded<R>(
        &mut self,
        op_name: &'static str,
        op: impl FnOnce(&mut Box<dyn Sink>) -> SinkResult<R>,
    ) -> SinkResult<R> {
        if self.degraded {
            return Err(self.degraded_error());
        }
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
                self.inner = Some(inner);
                Err(err)
            }
            Err(payload) => {
                let message = panic_message(payload);
                warn!(op = op_name, error = %message, "librespot audio sink panicked; reconstructing");
                // Dropping a panicked PortAudio sink can ITSELF panic
                // (`portaudio_rs::terminate().unwrap()` on a bad-state device,
                // e.g. AirPods mid-disconnect). Swallow that drop-panic so it
                // can't unwind into librespot's player thread and kill it.
                let _ = catch_unwind(AssertUnwindSafe(move || drop(inner)));
                if let Some(degraded) = self.try_recover() {
                    Err(degraded)
                } else {
                    Err(SinkError::OnWrite(format!(
                        "audio backend panicked and was reconstructed: {message}"
                    )))
                }
            }
        }
    }

    fn tap_packet(&mut self, packet: &AudioPacket) {
        if let AudioPacket::Samples(samples) = packet {
            let pcm = self.tap_converter.f64_to_s16(samples);
            self.counter.add_samples(pcm.len() as u64);
            if let Some(analyzer) = &self.analyzer {
                push_i16_samples(analyzer, &pcm, CHANNELS);
            }
        }
    }
}

impl<F> Sink for LibrespotSinkChain<F>
where
    F: FnMut() -> Box<dyn Sink> + Send + 'static,
{
    fn start(&mut self) -> SinkResult<()> {
        self.counter.reset();
        self.counter
            .set_format(librespot_playback::SAMPLE_RATE, CHANNELS as u32);
        self.guarded("start", |inner| inner.start())
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.guarded("stop", |inner| inner.stop())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        self.tap_packet(&packet);
        self.guarded("write", |inner| inner.write(packet, converter))
    }
}

impl<F> Drop for LibrespotSinkChain<F>
where
    F: FnMut() -> Box<dyn Sink>,
{
    /// librespot drops the whole `Box<dyn Sink>` during player teardown
    /// (clean stop, track end, session drop). The inner PortAudio sink's
    /// own `Drop` can panic (`terminate().unwrap()` on a bad-state device —
    /// AirPods disconnect/route change), and that would unwind straight into
    /// librespot's player thread and kill it ("Player thread Error: Any {..}").
    /// Drop the inner sink inside `catch_unwind` so the panic dies here. This
    /// `Drop` must never panic itself (a panic while already unwinding aborts
    /// the process), and `catch_unwind` guarantees that.
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            let _ = catch_unwind(AssertUnwindSafe(move || drop(inner)));
        }
    }
}

pub fn default_librespot_sink_factory(
    output_device: Option<String>,
    analyzer: Option<SharedAnalyzer>,
    counter: Arc<AudioCounterHandle>,
) -> Option<impl FnOnce() -> Box<dyn Sink> + Send + 'static> {
    let builder = librespot_playback::audio_backend::find(None)?;
    Some(move || {
        build_librespot_sink_chain(
            // `output_device` is the local audio output device (a cpal
            // output name for the rodio backend); `None` = system default.
            move || builder(output_device.clone(), AudioFormat::default()),
            analyzer,
            counter,
            SinkBudget::default(),
        )
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use librespot_playback::convert::Converter;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct RecordingSink {
        samples_seen: Arc<AtomicUsize>,
        starts: Arc<AtomicUsize>,
        panic_on_write: bool,
    }

    impl Sink for RecordingSink {
        fn start(&mut self) -> SinkResult<()> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        #[expect(clippy::panic, reason = "test double intentionally panics")]
        fn write(&mut self, packet: AudioPacket, _converter: &mut Converter) -> SinkResult<()> {
            if self.panic_on_write {
                panic!("scripted librespot sink panic");
            }
            if let AudioPacket::Samples(samples) = packet {
                self.samples_seen.fetch_add(samples.len(), Ordering::SeqCst);
            }
            Ok(())
        }
    }

    /// Test double whose `Drop` always panics (models PortAudioSink's
    /// `terminate().unwrap()` panicking on a bad-state device). Optionally
    /// also panics on `write`.
    struct DropPanicSink {
        panic_on_write: bool,
    }

    impl Sink for DropPanicSink {
        fn start(&mut self) -> SinkResult<()> {
            Ok(())
        }

        #[expect(clippy::panic, reason = "test double intentionally panics")]
        fn write(&mut self, packet: AudioPacket, _converter: &mut Converter) -> SinkResult<()> {
            if self.panic_on_write {
                panic!("scripted librespot sink write panic");
            }
            let _ = packet;
            Ok(())
        }
    }

    impl Drop for DropPanicSink {
        #[expect(clippy::panic, reason = "test double intentionally panics in drop")]
        fn drop(&mut self) {
            panic!("scripted librespot sink drop panic");
        }
    }

    fn converter() -> Converter {
        Converter::new(None)
    }

    #[test]
    fn taps_samples_before_delegating_to_physical_sink() {
        let analyzer = spotuify_audio::create_shared_analyzer();
        let counter = AudioCounterHandle::new();
        let samples_seen = Arc::new(AtomicUsize::new(0));
        let starts = Arc::new(AtomicUsize::new(0));
        let seen = samples_seen.clone();
        let started = starts.clone();
        let mut sink = build_librespot_sink_chain(
            move || {
                Box::new(RecordingSink {
                    samples_seen: seen.clone(),
                    starts: started.clone(),
                    panic_on_write: false,
                })
            },
            Some(analyzer.clone()),
            counter.clone(),
            SinkBudget::default(),
        );

        sink.start().expect("start should pass");
        sink.write(
            AudioPacket::Samples(vec![0.5; spotuify_audio::FFT_SIZE * CHANNELS]),
            &mut converter(),
        )
        .expect("write should pass");

        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert_eq!(
            samples_seen.load(Ordering::SeqCst),
            spotuify_audio::FFT_SIZE * CHANNELS
        );
        assert_eq!(
            counter.samples(),
            spotuify_audio::FFT_SIZE as u64 * CHANNELS as u64
        );

        let spectrum = analyzer
            .lock()
            .expect("analyzer lock should not be poisoned")
            .process();
        assert!(spectrum.peak > 0.0);
    }

    #[test]
    fn raw_packets_delegate_without_advancing_taps() {
        let counter = AudioCounterHandle::new();
        let samples_seen = Arc::new(AtomicUsize::new(0));
        let seen = samples_seen.clone();
        let mut sink = build_librespot_sink_chain(
            move || {
                Box::new(RecordingSink {
                    samples_seen: seen.clone(),
                    starts: Arc::new(AtomicUsize::new(0)),
                    panic_on_write: false,
                })
            },
            None,
            counter.clone(),
            SinkBudget::default(),
        );

        sink.write(AudioPacket::Raw(vec![1, 2, 3]), &mut converter())
            .expect("raw write should pass");

        assert_eq!(counter.samples(), 0);
        assert_eq!(samples_seen.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn panic_reconstructs_physical_sink() {
        let counter = AudioCounterHandle::new();
        let builds = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(AtomicUsize::new(0));
        let builds_for_factory = builds.clone();
        let seen_for_factory = seen.clone();
        let mut sink = build_librespot_sink_chain(
            move || {
                let build = builds_for_factory.fetch_add(1, Ordering::SeqCst);
                Box::new(RecordingSink {
                    samples_seen: seen_for_factory.clone(),
                    starts: Arc::new(AtomicUsize::new(0)),
                    panic_on_write: build == 0,
                })
            },
            None,
            counter,
            SinkBudget {
                max_panics: 3,
                window: Duration::from_secs(30),
            },
        );

        let first = sink.write(AudioPacket::Samples(vec![0.2; 8]), &mut converter());
        assert!(first.is_err(), "first write should surface recovered panic");
        sink.write(AudioPacket::Samples(vec![0.2; 8]), &mut converter())
            .expect("second write should use rebuilt sink");

        assert_eq!(builds.load(Ordering::SeqCst), 2);
        assert_eq!(seen.load(Ordering::SeqCst), 8);
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "test factory intentionally panics on first build"
    )]
    fn initial_build_panic_does_not_unwind() {
        // Models PortAudio `could not find device` panicking when the
        // configured output is gone at construction time. librespot builds the
        // sink on its player thread, so an escaping panic kills it. `new()`
        // must catch it and start `inner: None`, then rebuild on first op.
        let counter = AudioCounterHandle::new();
        let builds = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(AtomicUsize::new(0));
        let builds_for_factory = builds.clone();
        let seen_for_factory = seen.clone();
        let mut sink = build_librespot_sink_chain(
            move || {
                let build = builds_for_factory.fetch_add(1, Ordering::SeqCst);
                if build == 0 {
                    panic!("scripted build panic: could not find device");
                }
                Box::new(RecordingSink {
                    samples_seen: seen_for_factory.clone(),
                    starts: Arc::new(AtomicUsize::new(0)),
                    panic_on_write: false,
                })
            },
            None,
            counter,
            SinkBudget {
                max_panics: 5,
                window: Duration::from_secs(30),
            },
        );
        // Reaching here = the initial build panic was absorbed (not unwound).
        // First op rebuilds (build #2) and succeeds.
        sink.write(AudioPacket::Samples(vec![0.2; 8]), &mut converter())
            .expect("op after a caught build panic should rebuild and pass");
        assert_eq!(builds.load(Ordering::SeqCst), 2);
        assert_eq!(seen.load(Ordering::SeqCst), 8);
    }

    #[test]
    fn start_resets_counter_without_resetting_on_stop() {
        let counter = AudioCounterHandle::new();
        let mut sink = build_librespot_sink_chain(
            || {
                Box::new(RecordingSink {
                    samples_seen: Arc::new(AtomicUsize::new(0)),
                    starts: Arc::new(AtomicUsize::new(0)),
                    panic_on_write: false,
                })
            },
            None,
            counter.clone(),
            SinkBudget::default(),
        );

        sink.write(AudioPacket::Samples(vec![0.2; 8]), &mut converter())
            .expect("write should pass");
        assert_eq!(counter.samples(), 8);
        sink.stop().expect("stop should pass");
        assert_eq!(counter.samples(), 8);
        sink.start().expect("start should pass");
        assert_eq!(counter.samples(), 0);
    }

    #[test]
    fn drop_panic_does_not_unwind_out_of_chain() {
        let counter = AudioCounterHandle::new();
        let sink = build_librespot_sink_chain(
            || {
                Box::new(DropPanicSink {
                    panic_on_write: false,
                })
            },
            None,
            counter,
            SinkBudget::default(),
        );
        // Dropping the chain must NOT unwind even though the inner sink panics
        // in its Drop (models AirPods/PortAudio teardown). Without the
        // catch_unwind in LibrespotSinkChain::drop this would unwind out of the
        // test. Reaching the end = the drop-panic was absorbed.
        drop(sink);
    }

    #[test]
    fn write_panic_then_drop_panic_both_absorbed() {
        let counter = AudioCounterHandle::new();
        let mut sink = build_librespot_sink_chain(
            || {
                Box::new(DropPanicSink {
                    panic_on_write: true,
                })
            },
            None,
            counter,
            SinkBudget {
                max_panics: 5,
                window: Duration::from_secs(30),
            },
        );
        // write panics inside the inner sink → guarded() catches it, then drops
        // the panicked inner (which ALSO panics in Drop) inside catch_unwind →
        // surfaced as Err, never an unwind.
        let first = sink.write(AudioPacket::Samples(vec![0.2; 8]), &mut converter());
        assert!(
            first.is_err(),
            "write panic should surface as Err, not unwind"
        );
        // Dropping the (reconstructed) chain afterwards is still panic-safe.
        drop(sink);
    }
}
