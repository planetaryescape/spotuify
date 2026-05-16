//! Phase 17 audio visualization.
//!
//! Provides a 12-band log-spaced FFT spectrum analyzer plus optional
//! cross-platform system-audio loopback capture. The analyzer is fed by
//! either a sink-tap inside the embedded librespot chain (when the
//! embedded backend is in use) or by a loopback capture stream from cpal
//! or PipeWire. Consumers tick `process()` at a fixed rate to obtain
//! `SpectrumData` frames for rendering.

mod analyzer;
mod source;

#[cfg(feature = "loopback-cpal")]
pub mod loopback;

pub use analyzer::{
    create_shared_analyzer, AudioAnalyzer, SharedAnalyzer, SpectrumData, DEFAULT_NOISE_GATE,
    DEFAULT_SMOOTHING, FFT_SIZE, NUM_BANDS,
};
pub use source::{VizSource, VizSourceKind};
