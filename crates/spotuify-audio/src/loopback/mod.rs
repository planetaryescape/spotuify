//! Cross-platform system-audio loopback capture (Phase 17).
//!
//! Feeds raw f32 samples from the system audio output into a
//! `SharedAnalyzer` so the spectrum visualizer can render audio
//! produced by any source (spotifyd, external Connect device, etc.).
//!
//! - **Linux**: cpal monitors any device whose name contains "monitor"
//!   (PipeWire and PulseAudio both expose these). `loopback-pipewire` is
//!   reserved for a future native pipewire-rs backend; it does not replace
//!   the cpal capture manager today.
//! - **Windows**: cpal opens the default output device with WASAPI loopback —
//!   no third-party software needed.
//! - **macOS**: macOS has no native system-wide loopback. The cpal backend
//!   tries to detect a virtual device named `BlackHole` or `Loopback Audio`
//!   first; if absent, falls back to default input device (microphone),
//!   which is essentially useless for music viz but does not crash.

#[cfg(feature = "loopback-cpal")]
pub mod cpal_capture;

#[cfg(all(feature = "loopback-pipewire", target_os = "linux"))]
pub mod pipewire;

#[cfg(feature = "loopback-cpal")]
pub use cpal_capture::{AudioCaptureManager, LoopbackError};
