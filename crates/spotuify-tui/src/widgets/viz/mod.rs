//! Spectrum renderers. `bars` is spotuify's original widget; the other
//! twenty-seven styles are ported from cliamp (MIT, © Bjarne Øverli) — see
//! `THIRD_PARTY_LICENSES.md`.
//!
//! Most renderers draw from the 12-band feed the daemon broadcasts at 30 Hz;
//! `wave`, `scope`, and `heartbeat` trace the decimated raw waveform the same
//! event carries while one of them is selected. Styles with motion (falling peak caps, a fire heat field) keep
//! state between frames in [`VizState`], which the TUI advances once per
//! `SpectrumFrame` event and the renderer steps at a fixed 30 Hz timestep.
//!
//! # Motion parity with cliamp
//!
//! cliamp drives each mode from its own timer, so its per-tick constants are
//! only wall-clock-correct at that mode's rate. It has three classes:
//!
//! - **anim** — [`ANIM_HZ`], cliamp's `TickFast` (50 ms). Every particle and
//!   scroll style, plus the `flame` / `terrain` / `mosaic` / `sand` / `geyser`
//!   simulations.
//! - **wave** — [`WAVE_HZ`], cliamp's `TickAnim` / `TickWave` (16 ms). The bar
//!   styles and the oscilloscope family.
//! - **fixed-timestep** — `classic-peak` and `classic-led` integrate rates in
//!   per-second units against a real `dt`, so [`STEP_SECONDS`] already makes
//!   them wall-clock correct at any feed rate. They read the raw frame.
//!
//! spotuify has one clock — [`FRAME_HZ`] — so a style's own frame counter is
//! rescaled to the tick index cliamp's clock would be on at the same instant:
//! [`Ctx::anim_frame`] and [`Ctx::wave_frame`]. Everything downstream — phase
//! terms, integer speed divisors, LCG advances, [`StepClock`] steps — then
//! keeps cliamp's constants unchanged and lands on cliamp's wall-clock motion.
//! An anim-class style advances on two of every three frames; a wave-class one
//! advances twice per frame. Renderers must never read `ctx.frame` directly
//! unless they are fixed-timestep; use one of the two accessors, or
//! [`Ctx::seconds`] when the quantity is genuinely in seconds.

mod ascii;
mod bars_dot;
mod bars_outline;
mod binary;
mod bricks;
mod bubbles;
mod butterfly;
mod classic_led;
mod classic_peak;
mod columns;
mod firefly;
mod firework;
mod flame;
mod geyser;
mod heartbeat;
mod helpers;
mod matrix;
mod mirror;
mod mosaic;
mod pulse;
mod rain;
mod retro;
mod sakura;
mod sand;
mod scatter;
mod scope;
mod terrain;
mod wave;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{StatefulWidget, Widget};
use spotuify_protocol::{normalize_viz_style, VIZ_STYLES};

use super::spectrum::{spectrum_color_ratio, SpectrumColorScheme, SpectrumWidget};

/// Seconds per animation step. The daemon emits `SpectrumFrame` at 30 Hz, so
/// physics runs on a fixed timestep instead of wall-clock deltas — that keeps
/// the motion identical between a live terminal and a golden-buffer test.
const STEP_SECONDS: f32 = 1.0 / FRAME_HZ as f32;

/// Rate of the daemon's `SpectrumFrame` feed, and therefore of `VizState`'s
/// frame counter.
const FRAME_HZ: u64 = 30;

/// cliamp's `TickFast` (50 ms), the cadence behind every particle, scroll, and
/// simulation style's per-tick constants.
const ANIM_HZ: u64 = 20;

/// cliamp's `TickAnim` / `TickWave` (16 ms), the cadence behind the bar styles
/// and the oscilloscope family.
const WAVE_HZ: u64 = 60;

/// The tick index a cliamp clock running at `cliamp_hz` would be on when
/// spotuify is on `frame`. Divides before multiplying so the common case can't
/// overflow; wrapping matches the frame counter, which wraps too.
const fn rescale_frame(frame: u64, cliamp_hz: u64) -> u64 {
    (frame / FRAME_HZ)
        .wrapping_mul(cliamp_hz)
        .wrapping_add(frame % FRAME_HZ * cliamp_hz / FRAME_HZ)
}

/// Cap on physics steps per render. A terminal that repaints slower than the
/// spectrum feed catches up a few frames at a time rather than fast-forwarding
/// through a long stall.
const MAX_CATCH_UP_STEPS: u64 = 4;

/// Height ratios fed to `spectrum_color_ratio` for the three intensity tiers
/// the ported styles colour with. Each ratio sits inside a different band of
/// every spotuify colour scheme, so tiers stay visually distinct.
const TIER_RATIO: [f32; 3] = [0.0, 0.60, 0.90];

/// One of the spectrum renderers. The wire/config name is
/// [`spotuify_protocol::VIZ_STYLES`]; this enum is the TUI's view of it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VizStyle {
    #[default]
    Bars,
    BarsDot,
    BarsOutline,
    Bricks,
    Columns,
    ClassicPeak,
    ClassicLed,
    Mirror,
    Scatter,
    Rain,
    Matrix,
    Flame,
    Retro,
    Pulse,
    Wave,
    Scope,
    Heartbeat,
    Sakura,
    Firework,
    Bubbles,
    Terrain,
    Firefly,
    Mosaic,
    Sand,
    Geyser,
    Butterfly,
    Binary,
    Ascii,
}

impl VizStyle {
    /// Resolve a config/CLI name. Unknown names fall back to `bars`, matching
    /// `spotuify_protocol::normalize_viz_style`.
    pub fn from_name(name: &str) -> Self {
        match normalize_viz_style(name) {
            "bars-dot" => Self::BarsDot,
            "bars-outline" => Self::BarsOutline,
            "bricks" => Self::Bricks,
            "columns" => Self::Columns,
            "classic-peak" => Self::ClassicPeak,
            "classic-led" => Self::ClassicLed,
            "mirror" => Self::Mirror,
            "scatter" => Self::Scatter,
            "rain" => Self::Rain,
            "matrix" => Self::Matrix,
            "flame" => Self::Flame,
            "retro" => Self::Retro,
            "pulse" => Self::Pulse,
            "wave" => Self::Wave,
            "scope" => Self::Scope,
            "heartbeat" => Self::Heartbeat,
            "sakura" => Self::Sakura,
            "firework" => Self::Firework,
            "bubbles" => Self::Bubbles,
            "terrain" => Self::Terrain,
            "firefly" => Self::Firefly,
            "mosaic" => Self::Mosaic,
            "sand" => Self::Sand,
            "geyser" => Self::Geyser,
            "butterfly" => Self::Butterfly,
            "binary" => Self::Binary,
            "ascii" => Self::Ascii,
            _ => Self::Bars,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bars => "bars",
            Self::BarsDot => "bars-dot",
            Self::BarsOutline => "bars-outline",
            Self::Bricks => "bricks",
            Self::Columns => "columns",
            Self::ClassicPeak => "classic-peak",
            Self::ClassicLed => "classic-led",
            Self::Mirror => "mirror",
            Self::Scatter => "scatter",
            Self::Rain => "rain",
            Self::Matrix => "matrix",
            Self::Flame => "flame",
            Self::Retro => "retro",
            Self::Pulse => "pulse",
            Self::Wave => "wave",
            Self::Scope => "scope",
            Self::Heartbeat => "heartbeat",
            Self::Sakura => "sakura",
            Self::Firework => "firework",
            Self::Bubbles => "bubbles",
            Self::Terrain => "terrain",
            Self::Firefly => "firefly",
            Self::Mosaic => "mosaic",
            Self::Sand => "sand",
            Self::Geyser => "geyser",
            Self::Butterfly => "butterfly",
            Self::Binary => "binary",
            Self::Ascii => "ascii",
        }
    }

    pub fn description(self) -> &'static str {
        let name = self.as_str();
        VIZ_STYLES
            .iter()
            .find(|style| style.name == name)
            .map_or("", |style| style.description)
    }
}

/// Which panel a `VizWidget` is drawing into. Every viewport is a different
/// size, and the stateful styles key their buffers on size — sharing one
/// state across two viewports makes each frame look like a resize, which
/// resets the physics and reallocates. One state per viewport keeps each
/// one animating.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum VizViewport {
    /// The spectrum panel at the bottom of the player page.
    Panel,
    /// The live preview inside the style picker.
    Preview,
    /// The whole-terminal visualizer.
    Fullscreen,
}

impl VizViewport {
    pub const ALL: [Self; 3] = [Self::Panel, Self::Preview, Self::Fullscreen];

    pub fn index(self) -> usize {
        match self {
            Self::Panel => 0,
            Self::Preview => 1,
            Self::Fullscreen => 2,
        }
    }
}

/// Motion state carried between frames, for one viewport. The TUI owns one
/// per [`VizViewport`] and calls [`VizState::on_spectrum_frame`] on each for
/// every `SpectrumFrame` event; the renderer then steps physics forward by
/// however many frames it missed.
#[derive(Debug, Default)]
pub struct VizState {
    frame: u64,
    classic_peak: classic_peak::State,
    classic_led: classic_led::State,
    flame: flame::State,
    pulse: pulse::Coords,
    terrain: terrain::State,
    mosaic: mosaic::State,
    sand: sand::State,
    geyser: geyser::State,
}

impl VizState {
    /// Register one spectrum frame from the daemon.
    pub fn on_spectrum_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    /// Animation frame counter, read by the stateless motion styles.
    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// How many times a stateful style has had to throw away its buffers and
    /// start over because the panel changed size. Steady-state rendering must
    /// leave this at 1 (the initial build); anything more means two viewports
    /// are fighting over one state.
    pub fn rebuilds(&self) -> u32 {
        self.classic_peak.rebuilds()
            + self.classic_led.rebuilds()
            + self.flame.rebuilds()
            + self.terrain.rebuilds()
            + self.mosaic.rebuilds()
            + self.sand.rebuilds()
            + self.geyser.rebuilds()
    }

    /// Rebuilds of the pulse polar-coordinate cache, which is the most
    /// expensive one (a `hypot` + `atan2` per dot).
    pub fn pulse_rebuilds(&self) -> u32 {
        self.pulse.rebuilds()
    }

    /// `true` once a stateful style has buffers to animate from.
    pub fn has_motion_state(&self) -> bool {
        self.classic_peak.is_primed()
            || self.classic_led.is_primed()
            || self.flame.is_primed()
            || self.terrain.is_primed()
            || self.mosaic.is_primed()
            || self.sand.is_primed()
            || self.geyser.is_primed()
    }
}

/// Tracks how far a stateful style has stepped its physics. Keyed on the
/// absolute frame index rather than a delta so rendering the same frame twice
/// — the style picker draws a live preview over the player panel — advances
/// the animation exactly once.
#[derive(Debug, Default)]
pub(super) struct StepClock {
    stepped_to: u64,
}

impl StepClock {
    fn take(&mut self, frame: u64) -> u64 {
        let steps = frame.saturating_sub(self.stepped_to);
        self.stepped_to = frame;
        steps.min(MAX_CATCH_UP_STEPS)
    }
}

/// Resolves an intensity tier or a bar row to a concrete cell style, honouring
/// the configured colour scheme, the album accent, and `NO_COLOR`.
#[derive(Clone, Copy)]
pub(super) struct Painter {
    scheme: SpectrumColorScheme,
    accent: Option<Color>,
    enabled: bool,
}

impl Painter {
    /// Style for intensity tier 0 (low) … 2 (high).
    fn tier(self, tier: u8) -> Style {
        if !self.enabled {
            return Style::default();
        }
        let ratio = TIER_RATIO[usize::from(tier).min(TIER_RATIO.len() - 1)];
        Style::default().fg(spectrum_color_ratio(ratio, self.scheme, self.accent))
    }

    /// Style for a bar cell `row_from_bottom` rows above the baseline — the
    /// same vertical gradient the `bars` style uses.
    fn row(self, row_from_bottom: u16, height: u16) -> Style {
        if !self.enabled {
            return Style::default();
        }
        let ratio = f32::from(row_from_bottom) / f32::from(height.max(1));
        Style::default().fg(spectrum_color_ratio(ratio, self.scheme, self.accent))
    }
}

/// What a renderer needs for one frame.
pub(super) struct Ctx<'a> {
    bands: &'a [f32],
    /// Decimated raw samples in `-1.0..=1.0`, oldest first. Empty on a daemon
    /// older than the field, and in tests, so the waveform renderers must
    /// degrade to a resting trace rather than assume a length.
    waveform: &'a [f32],
    /// Raw `SpectrumFrame` count, at [`FRAME_HZ`]. Only the fixed-timestep
    /// styles read it directly — see the motion-parity note on this module.
    frame: u64,
    paint: Painter,
}

impl Ctx<'_> {
    /// Frame index for an anim-class style, on cliamp's [`ANIM_HZ`] clock.
    fn anim_frame(&self) -> u64 {
        rescale_frame(self.frame, ANIM_HZ)
    }

    /// Frame index for a wave-class style, on cliamp's [`WAVE_HZ`] clock.
    fn wave_frame(&self) -> u64 {
        rescale_frame(self.frame, WAVE_HZ)
    }

    /// Wall-clock seconds since the feed started. Class-independent: it is the
    /// same number whichever clock a style would have been ticking on.
    fn seconds(&self) -> f32 {
        self.frame as f32 * STEP_SECONDS
    }
}

/// Write one glyph at `(col, row)` relative to `area`, ignoring out-of-bounds
/// writes so every renderer can be written without clipping arithmetic.
pub(super) fn put(buf: &mut Buffer, area: Rect, col: u16, row: u16, ch: char, style: Style) {
    if col >= area.width || row >= area.height {
        return;
    }
    if let Some(cell) = buf.cell_mut((area.x + col, area.y + row)) {
        cell.set_char(ch);
        cell.set_style(style);
    }
}

pub struct VizWidget<'a> {
    bands: &'a [f32; 12],
    waveform: &'a [f32],
    style: VizStyle,
    color_scheme: SpectrumColorScheme,
    color_enabled: bool,
    accent: Option<Color>,
}

impl<'a> VizWidget<'a> {
    pub fn new(bands: &'a [f32; 12]) -> Self {
        Self {
            bands,
            waveform: &[],
            style: VizStyle::Bars,
            color_scheme: SpectrumColorScheme::SpotifyGreen,
            color_enabled: crate::widgets::terminal::color_enabled(),
            accent: None,
        }
    }

    /// Raw samples for the oscilloscope styles. Leave unset for the
    /// spectrum styles — they ignore it.
    pub fn waveform(mut self, value: &'a [f32]) -> Self {
        self.waveform = value;
        self
    }

    pub fn style(mut self, value: VizStyle) -> Self {
        self.style = value;
        self
    }

    pub fn color_scheme(mut self, value: &str) -> Self {
        self.color_scheme = SpectrumColorScheme::from_config(value);
        self
    }

    pub fn accent(mut self, value: Color) -> Self {
        self.accent = Some(value);
        self
    }

    pub fn color_enabled(mut self, value: bool) -> Self {
        self.color_enabled = value;
        self
    }
}

impl StatefulWidget for VizWidget<'_> {
    type State = VizState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut VizState) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        // `bars` is spotuify's own renderer, including its `#` ASCII fallback
        // under NO_COLOR. Leave it exactly as it was.
        if self.style == VizStyle::Bars {
            SpectrumWidget::new(self.bands)
                .color_scheme(self.color_scheme)
                .color_enabled(self.color_enabled)
                .accent(self.accent)
                .render(area, buf);
            return;
        }

        let ctx = Ctx {
            bands: &self.bands[..],
            waveform: self.waveform,
            frame: state.frame,
            paint: Painter {
                scheme: self.color_scheme,
                accent: self.accent,
                enabled: self.color_enabled,
            },
        };

        match self.style {
            VizStyle::Bars => unreachable!("handled above"),
            VizStyle::BarsDot => bars_dot::render(&ctx, area, buf),
            VizStyle::BarsOutline => bars_outline::render(&ctx, area, buf),
            VizStyle::Bricks => bricks::render(&ctx, area, buf),
            VizStyle::Columns => columns::render(&ctx, area, buf),
            VizStyle::ClassicPeak => {
                classic_peak::step(&mut state.classic_peak, &ctx, area);
                classic_peak::render(&state.classic_peak, &ctx, area, buf);
            }
            VizStyle::ClassicLed => {
                classic_led::step(&mut state.classic_led, &ctx, area);
                classic_led::render(&state.classic_led, &ctx, area, buf);
            }
            VizStyle::Mirror => mirror::render(&ctx, area, buf),
            VizStyle::Scatter => scatter::render(&ctx, area, buf),
            VizStyle::Rain => rain::render(&ctx, area, buf),
            VizStyle::Matrix => matrix::render(&ctx, area, buf),
            VizStyle::Flame => {
                flame::step(&mut state.flame, &ctx, area);
                flame::render(&state.flame, &ctx, area, buf);
            }
            VizStyle::Retro => retro::render(&ctx, area, buf),
            VizStyle::Pulse => pulse::render(&mut state.pulse, &ctx, area, buf),
            VizStyle::Wave => wave::render(&ctx, area, buf),
            VizStyle::Scope => scope::render(&ctx, area, buf),
            VizStyle::Heartbeat => heartbeat::render(&ctx, area, buf),
            VizStyle::Sakura => sakura::render(&ctx, area, buf),
            VizStyle::Firework => firework::render(&ctx, area, buf),
            VizStyle::Bubbles => bubbles::render(&ctx, area, buf),
            VizStyle::Terrain => {
                terrain::step(&mut state.terrain, &ctx, area);
                terrain::render(&state.terrain, &ctx, area, buf);
            }
            VizStyle::Firefly => firefly::render(&ctx, area, buf),
            VizStyle::Mosaic => {
                mosaic::step(&mut state.mosaic, &ctx, area);
                mosaic::render(&state.mosaic, &ctx, area, buf);
            }
            VizStyle::Sand => {
                sand::step(&mut state.sand, &ctx, area);
                sand::render(&state.sand, &ctx, area, buf);
            }
            VizStyle::Geyser => {
                geyser::step(&mut state.geyser, &ctx, area);
                geyser::render(&state.geyser, &ctx, area, buf);
            }
            VizStyle::Butterfly => butterfly::render(&ctx, area, buf),
            VizStyle::Binary => binary::render(&ctx, area, buf),
            VizStyle::Ascii => ascii::render(&ctx, area, buf),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_names_round_trip_through_the_protocol_roster() {
        for style in VIZ_STYLES {
            let parsed = VizStyle::from_name(style.name);
            assert_eq!(parsed.as_str(), style.name);
            assert_eq!(parsed.description(), style.description);
        }
    }

    #[test]
    fn unknown_style_names_fall_back_to_bars() {
        assert_eq!(VizStyle::from_name("nope"), VizStyle::Bars);
        assert_eq!(VizStyle::from_name(""), VizStyle::Bars);
        assert_eq!(VizStyle::from_name("  MATRIX  "), VizStyle::Matrix);
    }

    #[test]
    fn catch_up_is_capped_and_re_rendering_a_frame_does_not_step_again() {
        let mut clock = StepClock::default();

        assert_eq!(clock.take(100), MAX_CATCH_UP_STEPS);
        assert_eq!(clock.take(100), 0);
        assert_eq!(clock.take(102), 2);
    }
}
