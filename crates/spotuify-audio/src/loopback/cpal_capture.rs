//! cpal-based loopback capture (Phase 17).
//!
//! Adapted from `LargeModGames/spotatui::src/infra/audio/capture.rs` with
//! these extensions:
//! - Device-name selection is split into pure scoring functions
//!   (`score_macos_name`, `score_linux_name`) that are unit-tested
//!   without mocking cpal.
//! - macOS branch detects BlackHole / Loopback Audio virtual devices
//!   instead of blindly falling back to the default input (which is
//!   usually the microphone).
//! - Push samples into our `SharedAnalyzer` rather than spotatui's
//!   bespoke analyzer type.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
#[cfg(not(target_os = "windows"))]
use cpal::BufferSize;
use cpal::{Device, Stream, StreamConfig};
use thiserror::Error;
use tracing::{debug, warn};

use crate::analyzer::SharedAnalyzer;

#[derive(Debug, Error)]
pub enum LoopbackError {
    #[error("no suitable loopback device found")]
    NoDevice,
    #[error("could not get stream config for device")]
    NoConfig,
    #[error("failed to build input stream: {0}")]
    BuildStream(String),
    #[error("failed to start stream: {0}")]
    StartStream(String),
}

/// Manages audio capture from a system loopback / monitor device and pushes
/// the captured PCM into a shared analyzer.
pub struct AudioCaptureManager {
    _stream: Stream,
    active: Arc<AtomicBool>,
    device_name: String,
    sample_rate: u32,
}

impl AudioCaptureManager {
    /// Open a loopback / monitor device and start the capture stream.
    /// Pushes mono-mixed f32 samples into `analyzer` from the audio thread.
    pub fn new(analyzer: SharedAnalyzer) -> Result<Self, LoopbackError> {
        let host = cpal::default_host();
        let device = find_loopback_device(&host).ok_or(LoopbackError::NoDevice)?;
        let device_name = device.name().unwrap_or_else(|_| "<unknown>".into());
        let config = get_compatible_config(&device).ok_or(LoopbackError::NoConfig)?;
        let sample_rate = config.sample_rate.0;
        let active = Arc::new(AtomicBool::new(true));
        let stream = build_stream(&device, &config, analyzer, active.clone())?;
        stream
            .play()
            .map_err(|e| LoopbackError::StartStream(e.to_string()))?;
        debug!(
            target: "spotuify_audio::loopback",
            device = %device_name,
            sample_rate,
            "loopback capture stream started"
        );
        Ok(Self {
            _stream: stream,
            active,
            device_name,
            sample_rate,
        })
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

impl Drop for AudioCaptureManager {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Relaxed);
    }
}

/// Find a suitable loopback / monitor device using per-platform policy.
fn find_loopback_device(host: &cpal::Host) -> Option<Device> {
    #[cfg(target_os = "windows")]
    {
        // WASAPI loopback opens the default output device as an input stream.
        return host.default_output_device();
    }

    #[cfg(target_os = "linux")]
    {
        let devices = host.input_devices().ok()?;
        let mut scored: Vec<(u8, Device)> = devices
            .filter_map(|d| {
                let name = d.name().ok()?;
                let score = score_linux_name(&name);
                score.map(|s| (s, d))
            })
            .collect();
        scored.sort_by_key(|(s, _)| *s);
        if let Some((_, d)) = scored.into_iter().next() {
            return Some(d);
        }
        // Last-resort: default input device. On pure ALSA this is the mic and
        // is useless for music viz, but does not crash.
        host.default_input_device()
    }

    #[cfg(target_os = "macos")]
    {
        // Prefer a virtual-audio loopback device by name; fall back to the
        // default input (usually the mic — surface a hint via the doctor).
        if let Ok(devices) = host.input_devices() {
            let mut named: Vec<(u8, Device)> = devices
                .filter_map(|d| {
                    let name = d.name().ok()?;
                    let score = score_macos_name(&name)?;
                    Some((score, d))
                })
                .collect();
            named.sort_by_key(|(s, _)| *s);
            if let Some((_, d)) = named.into_iter().next() {
                return Some(d);
            }
        }
        host.default_input_device()
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        host.default_input_device()
    }
}

/// Build a stream config that won't fight with the audio server's buffer sizing.
fn get_compatible_config(device: &Device) -> Option<StreamConfig> {
    #[cfg(target_os = "windows")]
    {
        if let Ok(config) = device.default_output_config() {
            return Some(config.into());
        }
        None
    }

    #[cfg(not(target_os = "windows"))]
    {
        let config = device.default_input_config().ok()?;
        Some(StreamConfig {
            channels: config.channels(),
            sample_rate: config.sample_rate(),
            buffer_size: BufferSize::Default,
        })
    }
}

fn build_stream(
    device: &Device,
    config: &StreamConfig,
    analyzer: SharedAnalyzer,
    active: Arc<AtomicBool>,
) -> Result<Stream, LoopbackError> {
    let channels = config.channels as usize;
    let active_for_err = active.clone();

    let data_callback = move |data: &[f32], _: &cpal::InputCallbackInfo| {
        // Mono mixdown by averaging channels.
        let mono: Vec<f32> = data
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect();
        if let Ok(mut a) = analyzer.lock() {
            a.push_samples(&mono);
        }
    };

    let error_callback = move |err: cpal::StreamError| {
        warn!(target: "spotuify_audio::loopback", "stream error: {err}");
        active_for_err.store(false, Ordering::Relaxed);
    };

    device
        .build_input_stream(config, data_callback, error_callback, None)
        .map_err(|e| LoopbackError::BuildStream(e.to_string()))
}

// =====================================================================
// Pure scoring functions — testable without cpal devices.
// Lower score = higher priority.
// =====================================================================

/// Score a Linux device name for monitor/loopback preference.
/// Returns None for non-monitor devices (skip them).
///
/// Priority (lowest score wins):
///   0 = bluez / bluetooth monitor (the active wireless sink)
///   1 = speaker / analog monitor (built-in speakers)
///   2 = generic monitor
///   3 = hdmi monitor (rarely used for music)
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn score_linux_name(name: &str) -> Option<u8> {
    let lower = name.to_ascii_lowercase();
    if !lower.contains("monitor") {
        return None;
    }
    if lower.contains("bluez") || lower.contains("bluetooth") {
        return Some(0);
    }
    if lower.contains("speaker") || lower.contains("analog") {
        return Some(1);
    }
    if lower.contains("hdmi") {
        return Some(3);
    }
    Some(2)
}

/// Score a macOS device name for BlackHole / Loopback Audio detection.
/// Returns None for devices that aren't a known virtual loopback.
///
/// Priority (lowest score wins):
///   0 = BlackHole (any channel count: BlackHole 2ch, BlackHole 16ch, ...)
///   1 = Loopback Audio (Loopback.app)
///   2 = "loopback" anywhere in the name (generic third-party)
pub(crate) fn score_macos_name(name: &str) -> Option<u8> {
    let lower = name.to_ascii_lowercase();
    if lower.starts_with("blackhole") {
        return Some(0);
    }
    if lower.starts_with("loopback audio") {
        return Some(1);
    }
    if lower.contains("loopback") {
        return Some(2);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_skips_non_monitors() {
        assert_eq!(score_linux_name("Built-in Microphone"), None);
        assert_eq!(score_linux_name("USB Audio"), None);
    }

    #[test]
    fn linux_bluetooth_outranks_speakers() {
        let bt = score_linux_name("bluez_card.XX_XX.Monitor").expect("bt monitor");
        let sp = score_linux_name("alsa_output.analog-stereo.monitor").expect("analog monitor");
        let hdmi = score_linux_name("alsa_output.hdmi-stereo.monitor").expect("hdmi monitor");
        let generic = score_linux_name("Some Generic Monitor").expect("generic");
        assert!(bt < sp);
        assert!(sp < generic);
        assert!(generic < hdmi);
    }

    #[test]
    fn macos_blackhole_outranks_loopback() {
        let bh = score_macos_name("BlackHole 2ch").expect("blackhole");
        let bh16 = score_macos_name("BlackHole 16ch").expect("blackhole 16ch");
        let lb = score_macos_name("Loopback Audio").expect("loopback audio");
        let generic = score_macos_name("Some Loopback Device").expect("generic loopback");
        assert_eq!(bh, 0);
        assert_eq!(bh16, 0);
        assert_eq!(lb, 1);
        assert_eq!(generic, 2);
    }

    #[test]
    fn macos_rejects_unrelated_devices() {
        assert_eq!(score_macos_name("Built-in Microphone"), None);
        assert_eq!(score_macos_name("AirPods Pro"), None);
        assert_eq!(score_macos_name("External USB Mic"), None);
    }

    #[test]
    fn macos_case_insensitive() {
        assert!(score_macos_name("BLACKHOLE 2CH").is_some());
        assert!(score_macos_name("blackhole 2ch").is_some());
    }
}
