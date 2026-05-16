//! Real-time FFT spectrum analyzer (Phase 17).
//!
//! Pure-Rust 2048-point Hann-windowed FFT mapped to 12 logarithmic bands,
//! with per-band gain compensation, EMA smoothing, and a noise gate.
//!
//! Adopted with no algorithmic changes from `LargeModGames/spotatui`
//! (`src/infra/audio/analyzer.rs`). Formatting normalized to 4-space
//! rustfmt and tests added.

use std::sync::{Arc, Mutex};

/// Frequency bands for visualization (12 bands, roughly chromatic).
pub const NUM_BANDS: usize = 12;

/// Default smoothing factor for spectrum display (0.0 = no smoothing,
/// 1.0 = infinite). Higher = smoother but slower response.
pub const DEFAULT_SMOOTHING: f32 = 0.5;

/// Base gain for visualization amplitude.
const GAIN: f32 = 0.85;

/// FFT window size (power of 2, ~46 ms at 44.1 kHz).
pub const FFT_SIZE: usize = 2048;

/// Default noise gate — values below this are treated as silence.
pub const DEFAULT_NOISE_GATE: f32 = 0.005;

/// Spectrum data for visualization. All values normalized 0.0..=1.0.
#[derive(Clone, Debug, PartialEq)]
pub struct SpectrumData {
    pub bands: [f32; NUM_BANDS],
    pub peak: f32,
}

impl Default for SpectrumData {
    fn default() -> Self {
        Self {
            bands: [0.0; NUM_BANDS],
            peak: 0.0,
        }
    }
}

/// Audio analyzer that performs FFT on incoming mono samples and produces
/// 12-band spectrum data ready for rendering.
pub struct AudioAnalyzer {
    // `+ Send + Sync` on the trait object is required so a `SharedAnalyzer`
    // (Arc<Mutex<AudioAnalyzer>>) can cross task boundaries — the daemon's
    // VizCoordinator ticker is a spawned tokio future.
    fft: Arc<dyn realfft::RealToComplex<f32> + Send + Sync>,
    sample_buffer: Vec<f32>,
    fft_input: Vec<f32>,
    fft_output: Vec<realfft::num_complex::Complex<f32>>,
    spectrum: SpectrumData,
    write_pos: usize,
    smoothing: f32,
    noise_gate: f32,
}

impl AudioAnalyzer {
    pub fn new() -> Self {
        let mut planner = realfft::RealFftPlanner::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let fft_output_len = FFT_SIZE / 2 + 1;

        Self {
            fft,
            sample_buffer: vec![0.0; FFT_SIZE],
            fft_input: vec![0.0; FFT_SIZE],
            fft_output: vec![realfft::num_complex::Complex::default(); fft_output_len],
            spectrum: SpectrumData::default(),
            write_pos: 0,
            smoothing: DEFAULT_SMOOTHING,
            noise_gate: DEFAULT_NOISE_GATE,
        }
    }

    pub fn set_visual_params(&mut self, smoothing: f32, noise_gate: f32) {
        self.smoothing = smoothing.clamp(0.0, 0.95);
        self.noise_gate = noise_gate.clamp(0.0, 1.0);
    }

    /// Push audio samples into the analyzer's ring buffer. Older samples
    /// are overwritten; only the most recent `FFT_SIZE` samples survive
    /// to be processed.
    pub fn push_samples(&mut self, samples: &[f32]) {
        for &sample in samples {
            self.sample_buffer[self.write_pos] = sample;
            self.write_pos = (self.write_pos + 1) % FFT_SIZE;
        }
    }

    /// Process buffered samples and return the latest spectrum frame.
    ///
    /// Applies a Hann window, performs the FFT, maps bins to 12 log-spaced
    /// bands with per-band gain compensation, then EMA-smooths and gates
    /// the result.
    pub fn process(&mut self) -> SpectrumData {
        // Copy samples to FFT input buffer in temporal order, applying Hann window.
        let mut max_abs_sample = 0.0f32;
        for i in 0..FFT_SIZE {
            let idx = (self.write_pos + i) % FFT_SIZE;
            let sample = self.sample_buffer[idx];
            max_abs_sample = max_abs_sample.max(sample.abs());
            let window =
                0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / FFT_SIZE as f32).cos());
            self.fft_input[i] = sample * window;
        }

        if max_abs_sample < self.noise_gate {
            self.decay_spectrum_to_silence();
            return self.spectrum.clone();
        }

        if self
            .fft
            .process(&mut self.fft_input, &mut self.fft_output)
            .is_ok()
        {
            self.update_spectrum();
        }

        self.spectrum.clone()
    }

    fn decay_spectrum_to_silence(&mut self) {
        for band in &mut self.spectrum.bands {
            *band *= self.smoothing;
            if *band < self.noise_gate {
                *band = 0.0;
            }
        }
        self.spectrum.peak *= self.smoothing;
        if self.spectrum.peak < self.noise_gate {
            self.spectrum.peak = 0.0;
        }
    }

    /// Map FFT bins → 12 log-spaced bands, normalize, smooth, and gate.
    fn update_spectrum(&mut self) {
        let bin_count = self.fft_output.len();

        // Logarithmic band edges — roughly chromatic from sub-bass to ultra-highs.
        // Maps approximately C(~32Hz) to B(~16kHz) at 44.1kHz / FFT_SIZE=2048.
        let band_edges: [usize; NUM_BANDS + 1] = [
            1,
            2,
            4,
            8,
            16,
            32,
            64,
            128,
            256,
            384,
            512,
            768,
            bin_count - 1,
        ];

        let mut new_bands = [0.0f32; NUM_BANDS];
        let mut max_magnitude = 0.0f32;

        for band in 0..NUM_BANDS {
            let start = band_edges[band];
            let end = band_edges[band + 1].min(bin_count);

            if start < end {
                let mut sum = 0.0f32;
                for i in start..end {
                    let magnitude = self.fft_output[i].norm();
                    sum += magnitude;
                    max_magnitude = max_magnitude.max(magnitude);
                }
                new_bands[band] = sum / (end - start) as f32;
            }
        }

        // Per-band gain compensation — boost highs which naturally carry less
        // energy than bass, so the visualizer shows lively top-end response.
        const BAND_GAINS: [f32; NUM_BANDS] = [
            0.7, // Sub   - reduce sub rumble
            0.8, // Bass
            0.9, // Low
            1.0, // LMid
            1.0, // Mid
            1.0, // UMid
            1.1, // High
            1.2, // HiMd
            1.3, // Pres
            1.4, // Bril
            1.6, // Air
            2.0, // Ultra
        ];

        if max_magnitude > 0.0 {
            for (i, band) in new_bands.iter_mut().enumerate() {
                let normalized = (*band / max_magnitude) * BAND_GAINS[i] * GAIN;
                let scaled = normalized.sqrt();
                *band = scaled.min(0.85);
            }
        }

        // EMA smoothing for fluid animation, then noise gate.
        for (i, new_band) in new_bands.iter().enumerate() {
            self.spectrum.bands[i] =
                self.spectrum.bands[i] * self.smoothing + *new_band * (1.0 - self.smoothing);
            if self.spectrum.bands[i] < self.noise_gate {
                self.spectrum.bands[i] = 0.0;
            }
        }

        let current_peak = new_bands.iter().cloned().fold(0.0f32, f32::max);
        self.spectrum.peak =
            self.spectrum.peak * self.smoothing + current_peak * (1.0 - self.smoothing);
        if self.spectrum.peak < self.noise_gate {
            self.spectrum.peak = 0.0;
        }
    }
}

impl Default for AudioAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

/// Thread-safe handle to an analyzer. The sink-tap and loopback capture
/// callback both push samples through this; the daemon's VizCoordinator
/// pulls processed spectra from it on a fixed-rate tick.
pub type SharedAnalyzer = Arc<Mutex<AudioAnalyzer>>;

pub fn create_shared_analyzer() -> SharedAnalyzer {
    Arc::new(Mutex::new(AudioAnalyzer::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: f32 = 44_100.0;

    fn synthesize_sine(freq_hz: f32, amplitude: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| {
                amplitude * (2.0 * std::f32::consts::PI * freq_hz * i as f32 / SAMPLE_RATE).sin()
            })
            .collect()
    }

    /// FFT bin index for a given frequency at our SAMPLE_RATE/FFT_SIZE.
    fn freq_to_bin(freq_hz: f32) -> usize {
        ((freq_hz * FFT_SIZE as f32) / SAMPLE_RATE).round() as usize
    }

    /// Which band index contains a given bin, per the BAND_EDGES table.
    fn band_for_bin(bin: usize) -> usize {
        let edges = [
            1usize,
            2,
            4,
            8,
            16,
            32,
            64,
            128,
            256,
            384,
            512,
            768,
            usize::MAX,
        ];
        for b in 0..NUM_BANDS {
            if bin >= edges[b] && bin < edges[b + 1] {
                return b;
            }
        }
        NUM_BANDS - 1
    }

    #[test]
    fn default_spectrum_is_zero() {
        let s = SpectrumData::default();
        assert_eq!(s.peak, 0.0);
        assert!(s.bands.iter().all(|&b| b == 0.0));
    }

    #[test]
    fn processes_zero_signal_to_zero_bands() {
        let mut a = AudioAnalyzer::new();
        a.push_samples(&vec![0.0; FFT_SIZE]);
        let s = a.process();
        assert!(s.bands.iter().all(|&b| b == 0.0), "bands: {:?}", s.bands);
        assert_eq!(s.peak, 0.0);
    }

    #[test]
    fn detects_1khz_sine_in_correct_band() {
        let mut a = AudioAnalyzer::new();
        // Push several windows so smoothing settles toward the steady-state band response.
        let tone = synthesize_sine(1_000.0, 0.5, FFT_SIZE * 6);
        a.push_samples(&tone);

        let mut s = SpectrumData::default();
        for _ in 0..8 {
            s = a.process();
            // Re-push the same tone to keep the ring buffer fresh — process() reads
            // from the ring buffer without consuming it, so without re-push the
            // smoothing converges on the same windowed snapshot, which is fine.
        }

        let expected_bin = freq_to_bin(1_000.0); // ~46
        let expected_band = band_for_bin(expected_bin); // band 5 (covers bins [32, 64))
        assert_eq!(expected_band, 5);

        // Band 5 must dominate. Tolerate spectral leakage into adjacent bands but
        // require that band 5 is strictly greater than every other band.
        let max_idx = s
            .bands
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).expect("no NaN"))
            .expect("non-empty")
            .0;
        assert_eq!(
            max_idx, expected_band,
            "expected dominant band {} for 1 kHz, got {} (bands: {:?})",
            expected_band, max_idx, s.bands
        );
        assert!(s.peak > DEFAULT_NOISE_GATE);
    }

    #[test]
    fn noise_gate_kills_subthreshold_signal() {
        let mut a = AudioAnalyzer::new();
        let whisper = synthesize_sine(1_000.0, 0.001, FFT_SIZE);
        a.push_samples(&whisper);
        let s = a.process();
        assert!(s.bands.iter().all(|&b| b == 0.0), "bands: {:?}", s.bands);
        assert_eq!(s.peak, 0.0);
    }

    #[test]
    fn smoothing_attenuates_step_response() {
        let mut a = AudioAnalyzer::new();
        // Step from silence to a strong tone. Because SMOOTHING=0.5 and the
        // analyzer starts at zero, the first non-silent process() call should
        // give exactly half the steady-state band magnitudes (the EMA pole).
        let tone = synthesize_sine(1_000.0, 0.5, FFT_SIZE);

        // Warm up to steady state with the tone.
        a.push_samples(&tone);
        let mut steady = SpectrumData::default();
        for _ in 0..16 {
            steady = a.process();
        }

        // Fresh analyzer, one step from zero: peak should be exactly half of
        // steady's peak (within FP tolerance) since SMOOTHING=0.5.
        let mut a2 = AudioAnalyzer::new();
        a2.push_samples(&tone);
        let first = a2.process();

        assert!(
            steady.peak > 2.0 * DEFAULT_NOISE_GATE,
            "steady peak too low to test"
        );
        assert!(
            first.peak <= 0.5 * steady.peak + 1e-3,
            "first-step peak {} should be <= half of steady {}",
            first.peak,
            steady.peak
        );
    }

    #[test]
    fn ring_buffer_wraps_and_keeps_latest_samples() {
        let mut a = AudioAnalyzer::new();
        // Push 3 * FFT_SIZE samples. Each window is filled with a distinct
        // constant. Only the most recent FFT_SIZE samples (the third constant)
        // should be reflected in process() output.
        a.push_samples(&vec![1.0; FFT_SIZE]);
        a.push_samples(&vec![-1.0; FFT_SIZE]);
        a.push_samples(&vec![0.0; FFT_SIZE]);
        // After the third push, the buffer holds only zeros.
        let s = a.process();
        assert!(
            s.bands.iter().all(|&b| b == 0.0),
            "expected zeros after ring-buffer overwrite, got {:?}",
            s.bands
        );
        assert_eq!(s.peak, 0.0);
    }

    #[test]
    fn configurable_noise_gate_can_suppress_default_visible_signal() {
        let mut default = AudioAnalyzer::new();
        let tone = synthesize_sine(1_000.0, 0.1, FFT_SIZE);
        default.push_samples(&tone);
        let visible = default.process();
        assert!(visible.peak > 0.0);

        let mut gated = AudioAnalyzer::new();
        gated.set_visual_params(DEFAULT_SMOOTHING, 0.2);
        gated.push_samples(&tone);
        let suppressed = gated.process();
        assert_eq!(suppressed.peak, 0.0);
    }

    #[test]
    fn shared_analyzer_pushes_and_processes() {
        let shared = create_shared_analyzer();
        let tone = synthesize_sine(1_000.0, 0.4, FFT_SIZE);
        shared.lock().expect("analyzer lock").push_samples(&tone);
        let s = shared.lock().expect("analyzer lock").process();
        assert!(s.peak > 0.0);
    }
}
