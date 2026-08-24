# Phase 17 - Audio Visualization

## Goal

Real-time spectrum visualization in the TUI driven by actual audio samples, not Spotify's deprecated Audio Analysis API. Prefer the embedded sink tap; use loopback only when visualizing external/system audio.

## Evidence base

| Approach | Reference | Notes |
|---|---|---|
| System audio loopback (cpal monitor / WASAPI / BlackHole) | spotatui `infra/audio/capture.rs` | Works regardless of backend; requires platform-specific loopback |
| Sink-wrapper FFT tap inside librespot | spotify-player `streaming.rs:200-213` (`VisualizationSink`) | Works only with embedded librespot; no platform setup needed |
| `realfft` 2048-point Hann-windowed FFT | spotatui `audio/analyzer.rs:14` | 12 log-spaced bands, EMA smoothing |
| Renderers | spotatui uses `tui-equalizer` + `tui-bar-graph` | We can roll our own or vendor these |

## Decision: hybrid, prefer sink-wrapper

**Use sink-wrapper FFT tap when on embedded librespot (Phase 9). Fall back to system audio loopback for external Connect devices or other system audio.**

Sink-wrapper pros:
- No platform setup. Works out of the box on every OS.
- Audio data is the *decoded* PCM about to play, before the audio backend → represents what the user hears (after librespot's volume/normalisation).
- No latency between visualization and audio (loopback adds buffer delay).
- No permissions required (macOS microphone permission, etc.).

Loopback pros:
- Works for external Connect devices, mac AirPlay, and other system audio.
- Captures system-wide audio mix; useful if user wants visualization of any audio source.

Default: sink-wrapper on embedded, loopback fallback for external/system audio. Configurable via `[viz] source = "auto" | "sink" | "loopback" | "none"`.

## Deliverables

### `crates/spotuify-audio` (new)

```text
crates/spotuify-audio/
├── src/
│   ├── lib.rs
│   ├── source.rs           // Source trait: SinkTap | Loopback
│   ├── sink_tap.rs         // wraps librespot Sink, taps PCM into mpsc channel
│   ├── loopback/
│   │   ├── mod.rs
│   │   ├── cpal_monitor.rs  // linux pipewire/pulse monitor
│   │   ├── wasapi.rs        // windows WASAPI loopback
│   │   └── macos.rs         // BlackHole/Loopback virtual device
│   ├── fft.rs              // realfft 2048-point + Hann window
│   ├── bands.rs            // 12 log-spaced bands
│   └── smoothing.rs        // EMA + noise gate
```

### Audio source interface

```rust
pub trait AudioSource: Send {
    fn samples(&mut self) -> &[f32];     // mono mixdown, ~44.1k stereo source
    fn sample_rate(&self) -> u32;
    fn frames_available(&self) -> usize;
}
```

### Sink tap
- Implemented for the embedded librespot path in
  `crates/spotuify-player/src/backends/librespot_sink_chain.rs`.
- Inserted via Phase 9's sink-factory closure chain:
  `sink_factory() -> AudioCounterTap + VisualizationTap + RecoveringSink-style guard -> backend_sink`.
- Converts librespot PCM to i16, updates audible-sample counters, mono-mixes samples, and pushes them into the shared FFT analyzer.
- Analyzer locking is best-effort; visualization frames can be dropped without affecting playback.
- Verified with:
  `CARGO_TARGET_DIR=target-cli cargo test -p spotuify-player --features 'embedded-playback,rodio-backend' librespot_sink_chain --quiet`.

### Loopback (Linux)
- `cpal::Host::devices()`, filter for name containing `"monitor"`.
- Priority order: bluez/bluetooth → speaker/analog → default → hdmi.
- Falls back to default input device.
- Native PipeWire capture is intentionally deferred. The
  `loopback-pipewire` feature is only a reserved module boundary today;
  the implemented Linux path is cpal monitor capture over PipeWire/PulseAudio.
- Sample rate adaptation: source may be 48kHz; we resample to 44.1kHz or just work at native rate.

### Loopback (Windows)
- `cpal::default_output_device()` then `BuildStreamConfig::default()` with loopback flag.
- WASAPI loopback is built into Windows; no third-party software needed.

### Loopback (macOS)
- macOS has NO native system-wide loopback. Document that user needs BlackHole or Loopback.app.
- Detection: look for input device named "BlackHole" or "Loopback Audio" first; fall back to default input (which is microphone — useless for music viz, but at least doesn't crash).
- Show a one-time TUI banner explaining the BlackHole setup on first use.

### FFT pipeline
1. Take latest N=2048 mono samples from ring buffer.
2. Apply Hann window: `w[n] = 0.5 * (1 - cos(2πn / (N-1)))`.
3. `realfft::RealFftPlanner` forward FFT → 1025 complex bins.
4. Magnitude `|c|` per bin.
5. Map 1025 bins → 12 logarithmic bands. Per-band gain compensation for low high-frequency energy (spotatui `analyzer.rs:131-144` is a good baseline).
6. Noise gate at configured `[viz] noise_gate` (default 0.005).
7. EMA smoothing using configured `[viz] smoothing` (default 0.5).
8. sqrt-scaling for dB-like response.

### Renderers
- 12-bar equalizer (bottom of player tab in `player_large` mode).
- Vertical bars or peak-meter style.
- Color gradient maps band magnitude to color (configurable theme).
- ASCII fallback if terminal lacks color support.

### Performance budget
- FFT at ~30 Hz target.
- Total CPU < 5% on a modest laptop.
- Bounded mpsc + drop-on-full ensures visualization never holds back the audio pipeline.
- Skip FFT entirely when:
  - Visualization is off
  - Player is paused / stopped
  - TUI is not focused (no `LeaveFullscreen`-style event in ratatui; use `crossterm::FocusGained/Lost`)

### Configuration
- `[viz] enabled = false` (default off)
- `[viz] source = "auto" | "sink" | "loopback" | "none"`
- `[viz] bands = 12`
- `[viz] target_fps = 30`
- `[viz] smoothing = 0.5`
- `[viz] noise_gate = 0.005`
- `[viz] color_scheme = "spotify-green" | "rainbow" | "monochrome"`

`target_fps`, `smoothing`, and `noise_gate` are applied at daemon startup.
`color_scheme` is parsed from config and applied by the TUI spectrum widget
when it starts.

## Work items

1. [x] New `crates/spotuify-audio` workspace member.
2. [x] Implement visualization source typing plus embedded sink-tap implementation (depends on Phase 9). The original `AudioSource` trait was superseded by shared analyzer handles fed by sink tap or loopback capture.
3. [x] Implement loopback through cpal: Linux monitor-device selection, Windows WASAPI loopback, and macOS BlackHole/Loopback Audio detection with safe fallback.
4. [x] Implement FFT pipeline with configurable smoothing and noise gate.
5. [x] Implement TUI equalizer widget with `spotify-green`, `rainbow`, and
   `monochrome` color schemes.
6. [x] Wire into Player tab in `player_large` mode.
7. [x] Doctor/status reports visualization source, sample rate, target FPS, and dropped frames through `VizDiagnostics`.
8. [x] Document BlackHole / Loopback Audio requirement in README troubleshooting.
9. [x] CLI `spotuify viz enable|disable|source|status`.
10. [x] Performance test: 1 hour playback with viz on, monitor CPU and dropped frames remains manual verification for release QA; local coverage uses analyzer, source-selection, status, and TUI widget tests.

## Verification

- Embedded librespot + sink tap: bars move in time with audio; no audible glitches.
- Linux with PipeWire + loopback: bars move when any system audio plays.
- Windows loopback: works out of box.
- macOS with BlackHole installed and set as output device: bars move.
- macOS without BlackHole: banner shown, fallback to none (no crash).
- Paused playback → bars decay to zero and stop updating.
- Toggle visualization on/off via CLI → live update in TUI without restart.
- CPU usage stays under 5% with viz on during normal playback.
- TUI lose focus → FFT throttles to 1 Hz or stops.
- Analyzer tests cover config-driven noise-gate suppression.
- Viz coordinator tests cover focus throttling, source selection, disabled ticker behavior, and sink-source activation.
- CLI parser/help snapshots cover the `viz` command family.

## Definition of done

The shipped Phase 17 slice provides embedded sink-tap visualization, cpal
loopback fallback, runtime diagnostics, CLI/TUI controls, configurable
analyzer behavior, and a 12-band Player-tab renderer. Long-running CPU
budget and live per-OS loopback smoke remain manual verification items.

## Styles (added 2026-08-24)

The 12-band feed drives 14 renderers, selected by `viz.style` and listed in
`spotuify_protocol::VIZ_STYLES`. `bars` is the original widget in
`crates/spotuify-tui/src/widgets/spectrum.rs`; the other thirteen are ported
from cliamp (MIT) and live one-per-file under
`crates/spotuify-tui/src/widgets/viz/`, with shared geometry in `helpers.rs`.
See D032 in `docs/blueprint/13-decision-log.md` and `THIRD_PARTY_LICENSES.md`.

Surfaces:

- CLI: `spotuify viz styles` lists them; `spotuify viz style [<name>|next|prev]`
  reads or sets. `spotuify viz status` reports the style in effect.
- TUI: `ctrl+v` opens a picker with live preview (Enter commits, Esc restores,
  `/` filters, and the analyzer sources are rows at the bottom of the same
  list). `V` opens the fullscreen visualizer. The panel title shows `style=`.
- MCP: `viz_style_get` / `viz_style_set`.
- Wire: `set-viz-style` → Ack, then a `ClientPreferencesChanged` broadcast
  carrying the fresh `ClientPreferences`; clients apply it in place, exactly
  as they would `ClientSeed.preferences`.

Motion styles (`classic-peak`, `classic-led`, `flame`, `pulse`) keep state in
`VizState`, which the TUI advances once per `SpectrumFrame`; physics steps at a
fixed 1/30 s so golden-buffer snapshots are exact. Coverage lives in
`crates/spotuify-tui/tests/viz_styles.rs`: a colour and a `NO_COLOR` snapshot
per style, determinism and animation checks for the stateful ones, plus
degenerate-area (1×1, 1×40, 200×1) and silent-spectrum panic guards.
