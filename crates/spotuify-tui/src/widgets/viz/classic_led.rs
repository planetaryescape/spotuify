//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_classic_led.go`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::resample_bands_linear;
use super::{put, Ctx, StepClock, STEP_SECONDS};

const BAR_WIDTH: u16 = 2;
const BAR_GAP: u16 = 1;
/// Fast attack so a kick lights the LEDs immediately, medium decay so the bar
/// visibly settles one row at a time.
const RISE_RATE: f32 = 60.0;
const FALL_RATE: f32 = 16.0;
/// The cap holds at the apex, then falls at a constant rate.
const PEAK_HOLD: f32 = 0.45;
const PEAK_FALL: f32 = 0.55;

/// Winamp-style LED matrix: chunky two-wide bars under held peak caps.
#[derive(Debug, Default)]
pub(super) struct State {
    clock: StepClock,
    body: Vec<f32>,
    peak: Vec<f32>,
    hold: Vec<f32>,
    rebuilds: u32,
}

impl State {
    pub(super) fn rebuilds(&self) -> u32 {
        self.rebuilds
    }

    pub(super) fn is_primed(&self) -> bool {
        !self.body.is_empty()
    }

    fn reset(&mut self, levels: &[f32]) {
        self.body = levels.to_vec();
        self.peak = levels.to_vec();
        self.hold = vec![0.0; levels.len()];
        self.rebuilds = self.rebuilds.saturating_add(1);
    }

    fn matches(&self, len: usize) -> bool {
        self.body.len() == len && self.peak.len() == len && self.hold.len() == len
    }
}

fn bar_count(width: u16) -> usize {
    // Widen before the addition: `u16::MAX + BAR_GAP` overflows.
    ((usize::from(width) + usize::from(BAR_GAP)) / usize::from(BAR_WIDTH + BAR_GAP)).max(1)
}

fn levels_for(ctx: &Ctx<'_>, area: Rect) -> Vec<f32> {
    resample_bands_linear(ctx.bands, bar_count(area.width))
}

pub(super) fn step(state: &mut State, ctx: &Ctx<'_>, area: Rect) {
    let steps = state.clock.take(ctx.frame);
    let levels = levels_for(ctx, area);
    if !state.matches(levels.len()) {
        state.reset(&levels);
        return;
    }
    for _ in 0..steps {
        advance(state, &levels);
    }
}

fn advance(state: &mut State, levels: &[f32]) {
    for (i, target) in levels.iter().enumerate() {
        let rate = if *target > state.body[i] {
            RISE_RATE
        } else {
            FALL_RATE
        };
        state.body[i] += (*target - state.body[i]) * (1.0 - (-rate * STEP_SECONDS).exp());

        if state.body[i] >= state.peak[i] {
            state.peak[i] = state.body[i];
            state.hold[i] = PEAK_HOLD;
        } else if state.hold[i] > 0.0 {
            state.hold[i] = (state.hold[i] - STEP_SECONDS).max(0.0);
        } else {
            state.peak[i] = state.body[i].max(state.peak[i] - PEAK_FALL * STEP_SECONDS);
        }
    }
}

pub(super) fn render(state: &State, ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let fallback = levels_for(ctx, area);
    let (body, peak) = if state.matches(fallback.len()) {
        (state.body.as_slice(), state.peak.as_slice())
    } else {
        (fallback.as_slice(), fallback.as_slice())
    };
    let height = area.height;
    let height_f = f32::from(height);
    // Column maths in usize: at u16::MAX width there are more bar slots than
    // fit in a u16, and `put` clips anything past the right edge anyway.
    let stride = usize::from(BAR_WIDTH + BAR_GAP);
    let render_width = stride * body.len() - usize::from(BAR_GAP);
    let pad = usize::from(area.width).saturating_sub(render_width);

    for row in 0..height {
        // Row index counted from the bottom: 0 is the lowest LED.
        let from_bottom = height - 1 - row;
        let style = ctx.paint.row(from_bottom, height);
        for (b, level) in body.iter().enumerate() {
            let lit = (level * height_f + 1e-6).floor() as u16;
            let peak_row = ((peak[b] * height_f + 1e-6).floor() as u16).min(height - 1);
            // The cap only draws while it sits strictly above the lit body.
            let show_peak = peak[b] > level + 0.5 / height_f && peak_row >= lit;
            let glyph = if from_bottom < lit {
                '▄'
            } else if show_peak && from_bottom == peak_row {
                '▀'
            } else {
                continue;
            };
            let x0 = pad + b * stride;
            if x0 >= usize::from(area.width) {
                break;
            }
            for dx in 0..usize::from(BAR_WIDTH) {
                put(buf, area, (x0 + dx) as u16, row, glyph, style);
            }
        }
    }
}
