//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_classic_peak.go`.
//!
//! cliamp drives this from a 64-band analysis; spotuify's daemon publishes 12
//! bands, so the columns are resampled from those. The physics is unchanged —
//! the caps just ride a coarser spectrum.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::{frac_block, resample_bands_linear};
use super::{put, Ctx, StepClock, STEP_SECONDS};

/// Cap glyphs, top of the cell to bottom, giving quarter-row resolution.
const CAP_GLYPHS: [char; 4] = ['⎺', '⎻', '⎼', '⎽'];

const BAR_WIDTH: u16 = 1;
const BAR_GAP: u16 = 1;
/// Minimum upward launch velocity for a newly detached cap.
const LAUNCH_BASE: f32 = 0.8;
/// Extra launch velocity in proportion to how far the bar rose.
const LAUNCH_GAIN: f32 = 1.4;
const LAUNCH_MAX: f32 = 1.7;
const GRAVITY: f32 = 9.5;
/// Pause at the apex before the cap starts falling.
const APEX_HOLD: f32 = 0.08;
const RISE_RATE: f32 = 34.0;
const FALL_RATE: f32 = 10.0;
/// Tolerance for treating cap and bar positions as visually equal.
const EPSILON: f32 = 0.01;

/// Bar bodies plus independently falling peak caps.
#[derive(Debug, Default)]
pub(super) struct State {
    clock: StepClock,
    bar: Vec<f32>,
    peak: Vec<f32>,
    velocity: Vec<f32>,
    hold: Vec<f32>,
}

impl State {
    fn reset(&mut self, levels: &[f32]) {
        self.bar = levels.to_vec();
        self.peak = levels.to_vec();
        self.velocity = vec![0.0; levels.len()];
        self.hold = vec![0.0; levels.len()];
    }

    fn matches(&self, len: usize) -> bool {
        self.bar.len() == len && self.peak.len() == len
    }

    fn landed(&self, i: usize) -> bool {
        self.velocity[i] == 0.0 && self.peak[i] <= self.bar[i] + EPSILON
    }
}

fn columns_for(width: u16) -> usize {
    usize::from(((width + BAR_GAP) / (BAR_WIDTH + BAR_GAP)).max(1))
}

fn levels_for(ctx: &Ctx<'_>, area: Rect) -> Vec<f32> {
    resample_bands_linear(ctx.bands, columns_for(area.width))
}

pub(super) fn step(state: &mut State, ctx: &Ctx<'_>, area: Rect) {
    let steps = state.clock.take(ctx.frame);
    let levels = levels_for(ctx, area);
    if !state.matches(levels.len()) {
        state.reset(&levels);
        return;
    }
    for _ in 0..steps {
        launch_caps(state, &levels);
        advance(state, &levels);
    }
}

/// A cap that has landed re-launches the moment its bar overtakes it, with a
/// velocity proportional to the jump — that is what makes loud hits throw the
/// cap higher than quiet ones.
fn launch_caps(state: &mut State, levels: &[f32]) {
    for (i, level) in levels.iter().enumerate() {
        if state.landed(i) && *level > state.peak[i] {
            let delta = *level - state.peak[i];
            state.peak[i] = *level;
            state.velocity[i] = LAUNCH_MAX.min(LAUNCH_BASE + LAUNCH_GAIN * delta);
            state.hold[i] = 0.0;
        }
    }
}

fn advance(state: &mut State, levels: &[f32]) {
    for (i, level) in levels.iter().enumerate() {
        let rate = if *level > state.bar[i] {
            RISE_RATE
        } else {
            FALL_RATE
        };
        state.bar[i] += (*level - state.bar[i]) * (1.0 - (-rate * STEP_SECONDS).exp());

        if state.hold[i] > 0.0 {
            state.hold[i] = (state.hold[i] - STEP_SECONDS).max(0.0);
            if state.hold[i] > 0.0 {
                continue;
            }
        }

        let previous_velocity = state.velocity[i];
        state.peak[i] = (state.peak[i] + state.velocity[i] * STEP_SECONDS).min(1.0);
        state.velocity[i] -= GRAVITY * STEP_SECONDS;

        let at_apex = previous_velocity > 0.0 && state.velocity[i] <= 0.0;
        if at_apex && state.peak[i] > state.bar[i] + EPSILON {
            state.velocity[i] = 0.0;
            state.hold[i] = APEX_HOLD;
            continue;
        }
        if state.peak[i] <= state.bar[i] {
            state.peak[i] = state.bar[i];
            state.velocity[i] = 0.0;
            state.hold[i] = 0.0;
        }
    }
}

pub(super) fn render(state: &State, ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let fallback = levels_for(ctx, area);
    let (bars, peaks) = if state.matches(fallback.len()) {
        (state.bar.as_slice(), state.peak.as_slice())
    } else {
        (fallback.as_slice(), fallback.as_slice())
    };
    let height = area.height;
    let render_width = (BAR_WIDTH + BAR_GAP) * bars.len() as u16 - BAR_GAP;
    let pad = area.width.saturating_sub(render_width);

    for row in 0..height {
        let row_bottom = f32::from(height - 1 - row) / f32::from(height);
        let row_top = f32::from(height - row) / f32::from(height);
        let style = ctx.paint.row(height - 1 - row, height);
        for (col, level) in bars.iter().enumerate() {
            let (cap_row, cap_glyph) = cap_position(peaks[col], height);
            let glyph = if detached(*level, peaks[col], height) && row == cap_row {
                cap_glyph
            } else {
                frac_block(*level, row_bottom, row_top)
            };
            if glyph != ' ' {
                put(
                    buf,
                    area,
                    pad + col as u16 * (BAR_WIDTH + BAR_GAP),
                    row,
                    glyph,
                    style,
                );
            }
        }
    }
}

/// Which row a cap sits in, and which quarter-row glyph to draw there.
fn cap_position(level: f32, height: u16) -> (u16, char) {
    let dot_rows = usize::from(height).max(1) * 4;
    let dot_y = ((1.0 - level.min(1.0)) * (dot_rows - 1) as f32).round() as usize;
    ((dot_y / 4) as u16, CAP_GLYPHS[dot_y % 4])
}

/// A cap only draws once it has visibly separated from its bar.
fn detached(level: f32, peak: f32, height: u16) -> bool {
    let min_gap = EPSILON.max(0.5 / f32::from(height.max(1)) / 4.0);
    peak > level + min_gap
}
