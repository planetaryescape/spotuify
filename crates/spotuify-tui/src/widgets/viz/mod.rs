//! Spectrum renderers. `bars` is spotuify's original widget; the other
//! thirteen styles are ported from cliamp (MIT, © Bjarne Øverli) — see
//! `THIRD_PARTY_LICENSES.md`.
//!
//! Every renderer draws from the same 12-band feed the daemon broadcasts at
//! 30 Hz. Styles with motion (falling peak caps, a fire heat field) keep
//! state between frames in [`VizState`], which the TUI advances once per
//! `SpectrumFrame` event and the renderer steps at a fixed 30 Hz timestep.

mod bars_dot;
mod bars_outline;
mod bricks;
mod classic_led;
mod classic_peak;
mod columns;
mod flame;
mod helpers;
mod matrix;
mod mirror;
mod pulse;
mod rain;
mod retro;
mod scatter;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{StatefulWidget, Widget};
use spotuify_protocol::{normalize_viz_style, VIZ_STYLES};

use super::spectrum::{spectrum_color_ratio, SpectrumColorScheme, SpectrumWidget};

/// Seconds per animation step. The daemon emits `SpectrumFrame` at 30 Hz, so
/// physics runs on a fixed timestep instead of wall-clock deltas — that keeps
/// the motion identical between a live terminal and a golden-buffer test.
const STEP_SECONDS: f32 = 1.0 / 30.0;

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

/// Motion state carried between frames. The TUI owns one of these and calls
/// [`VizState::on_spectrum_frame`] for every `SpectrumFrame` event; the
/// renderer then steps physics forward by however many frames it missed.
#[derive(Debug, Default)]
pub struct VizState {
    frame: u64,
    classic_peak: classic_peak::State,
    classic_led: classic_led::State,
    flame: flame::State,
    pulse: pulse::Coords,
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
    frame: u64,
    paint: Painter,
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
    style: VizStyle,
    color_scheme: SpectrumColorScheme,
    color_enabled: bool,
    accent: Option<Color>,
}

impl<'a> VizWidget<'a> {
    pub fn new(bands: &'a [f32; 12]) -> Self {
        Self {
            bands,
            style: VizStyle::Bars,
            color_scheme: SpectrumColorScheme::SpotifyGreen,
            color_enabled: crate::widgets::terminal::color_enabled(),
            accent: None,
        }
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
