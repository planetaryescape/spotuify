//! Phase 17 — `VisualizationTap` sink wrapper.
//!
//! Mirrors `AudioCounterTap`: sit inside the embedded audio sink chain,
//! mono-mix i16 PCM into f32 samples, push them into the shared FFT
//! analyzer, then delegate the original frames to the physical sink.

use spotuify_audio::SharedAnalyzer;

use crate::backends::recovering_sink::{Sink, SinkError};

const DEFAULT_CHANNELS: usize = 2;
const I16_SCALE: f32 = i16::MAX as f32;

pub struct VisualizationTap<S: Sink> {
    inner: S,
    analyzer: Option<SharedAnalyzer>,
    channels: usize,
}

impl<S: Sink> VisualizationTap<S> {
    pub fn new(inner: S, analyzer: Option<SharedAnalyzer>) -> Self {
        Self {
            inner,
            analyzer,
            channels: DEFAULT_CHANNELS,
        }
    }

    pub fn with_channels(mut self, channels: usize) -> Self {
        self.channels = channels.max(1);
        self
    }
}

impl<S: Sink> Sink for VisualizationTap<S> {
    fn start(&mut self) -> Result<(), SinkError> {
        self.inner.start()
    }

    fn stop(&mut self) -> Result<(), SinkError> {
        self.inner.stop()
    }

    fn write(&mut self, frames: &[i16]) -> Result<(), SinkError> {
        if let Some(analyzer) = &self.analyzer {
            push_i16_samples(analyzer, frames, self.channels);
        }
        self.inner.write(frames)
    }
}

pub(crate) fn push_i16_samples(analyzer: &SharedAnalyzer, samples: &[i16], channels: usize) {
    let mono = mixdown_i16_to_mono_f32(samples, channels);
    if let Ok(mut analyzer) = analyzer.lock() {
        analyzer.push_samples(&mono);
    }
}

pub(crate) fn mixdown_i16_to_mono_f32(samples: &[i16], channels: usize) -> Vec<f32> {
    let channels = channels.max(1);
    samples
        .chunks(channels)
        .map(|frame| {
            let sum = frame
                .iter()
                .map(|sample| (*sample as f32 / I16_SCALE).clamp(-1.0, 1.0))
                .sum::<f32>();
            sum / frame.len().max(1) as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use spotuify_audio::create_shared_analyzer;
    use std::sync::Mutex;

    struct Recorder {
        written: Mutex<Vec<i16>>,
    }

    impl Recorder {
        fn new() -> Self {
            Self {
                written: Mutex::new(Vec::new()),
            }
        }
    }

    impl Sink for Recorder {
        fn start(&mut self) -> Result<(), SinkError> {
            Ok(())
        }

        fn stop(&mut self) -> Result<(), SinkError> {
            Ok(())
        }

        fn write(&mut self, frames: &[i16]) -> Result<(), SinkError> {
            self.written
                .lock()
                .expect("recorder mutex should not be poisoned")
                .extend_from_slice(frames);
            Ok(())
        }
    }

    #[test]
    fn delegates_writes_to_inner_sink() {
        let recorder = Recorder::new();
        let mut tap = VisualizationTap::new(recorder, None);

        tap.write(&[1, 2, 3, 4]).expect("write should pass");

        assert_eq!(
            tap.inner
                .written
                .lock()
                .expect("recorder mutex should not be poisoned")
                .as_slice(),
            &[1, 2, 3, 4]
        );
    }

    #[test]
    fn mixdown_stereo_i16_to_mono_f32() {
        let mono = mixdown_i16_to_mono_f32(&[i16::MAX, i16::MAX, i16::MAX, i16::MIN], 2);

        assert_eq!(mono.len(), 2);
        assert!((mono[0] - 1.0).abs() < 0.0001);
        assert!(
            mono[1].abs() < 0.0001,
            "expected opposite channels to cancel"
        );
    }

    #[test]
    fn pushes_mono_mixdown_into_analyzer() {
        let analyzer = create_shared_analyzer();
        let frames = (0..spotuify_audio::FFT_SIZE)
            .flat_map(|i| {
                let phase = 2.0 * std::f32::consts::PI * 1_000.0 * i as f32 / 44_100.0;
                let sample = (phase.sin() * i16::MAX as f32 * 0.5) as i16;
                [sample, sample]
            })
            .collect::<Vec<_>>();
        let mut tap = VisualizationTap::new(Recorder::new(), Some(analyzer.clone()));

        tap.write(&frames).expect("write should pass");
        let spectrum = analyzer
            .lock()
            .expect("analyzer lock should not be poisoned")
            .process();

        assert!(spectrum.peak > 0.0);
        assert!(
            spectrum.bands.iter().any(|band| *band > 0.0),
            "expected analyzer to receive audio"
        );
    }

    #[test]
    fn no_op_when_analyzer_is_none() {
        let mut tap = VisualizationTap::new(Recorder::new(), None);

        tap.write(&[1, 2, 3, 4]).expect("write should pass");

        assert_eq!(
            tap.inner
                .written
                .lock()
                .expect("recorder mutex should not be poisoned")
                .as_slice(),
            &[1, 2, 3, 4]
        );
    }
}
