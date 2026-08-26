//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_flame.go`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::{braille_char, rng_next, sample_band_linear, scatter_hash, BRAILLE_BIT};
use super::{put, Ctx, StepClock};

/// Below this heat a dot never lights.
const EMBER_FLOOR: f32 = 0.10;
/// Between `EMBER_FLOOR` and this, dots light stochastically so the flame tip
/// has a broken silhouette instead of a hard cutoff.
const WISP_CEILING: f32 = 0.25;
/// Heat at which a dot switches from the body tier to the hot core tier.
const CORE_HEAT: f32 = 0.55;

/// Doom-fire heat field: the bottom row is fed from the spectrum, and every
/// frame each cell inherits its neighbour-below's heat with a lateral wind
/// jitter and randomised decay.
#[derive(Debug, Default)]
pub(super) struct State {
    clock: StepClock,
    heat: Vec<f32>,
    dot_rows: usize,
    dot_cols: usize,
    rng: u64,
    /// Simulation steps run so far. Distinct from the render frame: the wisp
    /// stipple has to advance with the fire, not with repaints.
    frame: u64,
    rebuilds: u32,
}

impl State {
    pub(super) fn rebuilds(&self) -> u32 {
        self.rebuilds
    }

    pub(super) fn is_primed(&self) -> bool {
        !self.heat.is_empty()
    }

    /// Size the field to the panel. Returns `true` when it reallocated, which
    /// tells the caller to run at least one step so the first painted frame
    /// is not an empty field.
    fn ensure(&mut self, dot_rows: usize, dot_cols: usize) -> bool {
        if self.dot_rows == dot_rows && self.dot_cols == dot_cols {
            return false;
        }
        self.heat = vec![0.0; dot_rows * dot_cols];
        self.dot_rows = dot_rows;
        self.dot_cols = dot_cols;
        self.rng = 0xF1A3_C0DE_0BAD_CAFE;
        self.rebuilds = self.rebuilds.saturating_add(1);
        true
    }
}

pub(super) fn step(state: &mut State, ctx: &Ctx<'_>, area: Rect) {
    let steps = state.clock.take(ctx.anim_frame());
    let dot_rows = usize::from(area.height) * 4;
    let dot_cols = usize::from(area.width) * 2;
    if dot_rows < 4 || dot_cols < 4 {
        return;
    }
    let steps = if state.ensure(dot_rows, dot_cols) {
        steps.max(1)
    } else {
        steps
    };
    for _ in 0..steps {
        state.frame = state.frame.wrapping_add(1);
        seed_source_row(state, ctx.bands);
        propagate(state);
    }
}

/// Bottom row: a smooth spectrum sample plus per-column sparkle, with a floor
/// so a bed of embers stays lit through quiet passages.
fn seed_source_row(state: &mut State, bands: &[f32]) {
    let dot_cols = state.dot_cols;
    let last = bands.len().saturating_sub(1) as f32;
    for x in 0..dot_cols {
        let sparkle = rng_next(&mut state.rng) * 0.18;
        let source = if bands.is_empty() {
            0.0
        } else {
            sample_band_linear(bands, x as f32 / (dot_cols - 1).max(1) as f32 * last)
        };
        state.heat[x] = (0.30 + 0.70 * source + sparkle).min(1.05);
    }
}

/// Propagate heat upward. Top-down so row `y-1` is still the previous frame's
/// value when row `y` reads it.
fn propagate(state: &mut State) {
    let (dot_rows, dot_cols) = (state.dot_rows, state.dot_cols);
    for y in (1..dot_rows).rev() {
        // Heat decays faster near the top so flames taper.
        let height_frac = y as f32 / (dot_rows - 1).max(1) as f32;
        let decay_base = 0.010 + 0.028 * height_frac;
        for x in 0..dot_cols {
            state.rng = state
                .rng
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let r = state.rng >> 33;
            let wind = (r % 3) as i64 - 1;
            let jitter = ((r >> 2) % 100) as f32 / 100.0 * 0.018;
            let source_x = (x as i64 + wind).clamp(0, dot_cols as i64 - 1) as usize;
            let next = state.heat[(y - 1) * dot_cols + source_x] - decay_base - jitter;
            state.heat[y * dot_cols + x] = next.max(0.0);
        }
    }
}

pub(super) fn render(state: &State, ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let dot_rows = usize::from(area.height) * 4;
    let dot_cols = usize::from(area.width) * 2;
    if state.dot_rows != dot_rows || state.dot_cols != dot_cols {
        return;
    }

    for row in 0..area.height {
        for col in 0..area.width {
            let mut bits = 0_u32;
            let mut tier = 0_u8;
            let mut lit = false;
            for (dr, bit_row) in BRAILLE_BIT.iter().enumerate() {
                for (dc, bit) in bit_row.iter().enumerate() {
                    let y = usize::from(row) * 4 + dr;
                    let x = usize::from(col) * 2 + dc;
                    // Panel row 0 is the top; the heat buffer grows from the
                    // bottom, where the source row lives.
                    let heat = state.heat[(dot_rows - 1 - y) * dot_cols + x];
                    if heat < EMBER_FLOOR {
                        continue;
                    }
                    if heat < WISP_CEILING && scatter_hash(0, y, x, state.frame) > heat * 4.0 {
                        continue;
                    }
                    bits |= bit;
                    lit = true;
                    tier = tier.max(if heat >= CORE_HEAT { 1 } else { 2 });
                }
            }
            if !lit {
                continue;
            }
            put(
                buf,
                area,
                col,
                row,
                braille_char(bits),
                ctx.paint.tier(tier),
            );
        }
    }
}
