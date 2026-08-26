//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_terrain.go`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::{band_avg, scatter_hash, DotGrid};
use super::{Ctx, StepClock};

/// Dot columns the range scrolls left per step, as cliamp does — 40 dot
/// columns a second on the anim clock.
const SCROLL: usize = 2;
/// How much per-column noise roughens the new ridge, as a fraction of full
/// height. Without it a sustained note draws a dead-flat plateau.
const ROUGHNESS: f32 = 0.12;

/// A scrolling height field. One entry per dot column; new samples enter at
/// the right and the whole range shifts left.
#[derive(Debug, Default)]
pub(super) struct State {
    clock: StepClock,
    heights: Vec<f32>,
    rebuilds: u32,
}

impl State {
    pub(super) fn rebuilds(&self) -> u32 {
        self.rebuilds
    }

    pub(super) fn is_primed(&self) -> bool {
        !self.heights.is_empty()
    }

    /// Resize to `dot_cols`, keeping the rightmost (newest) samples so a
    /// resize slides the range rather than flattening it.
    fn ensure(&mut self, dot_cols: usize) -> bool {
        if self.heights.len() == dot_cols {
            return false;
        }
        let mut next = vec![0.0; dot_cols];
        let keep = self.heights.len().min(dot_cols);
        next[dot_cols - keep..].copy_from_slice(&self.heights[self.heights.len() - keep..]);
        self.heights = next;
        self.rebuilds = self.rebuilds.saturating_add(1);
        true
    }
}

pub(super) fn step(state: &mut State, ctx: &Ctx<'_>, area: Rect) {
    let steps = state.clock.take(ctx.anim_frame());
    let dot_cols = usize::from(area.width) * 2;
    if dot_cols <= SCROLL {
        return;
    }
    let steps = if state.ensure(dot_cols) {
        steps.max(1)
    } else {
        steps
    };

    let average = band_avg(ctx.bands, 0, ctx.bands.len());
    for _ in 0..steps {
        state.heights.copy_within(SCROLL.., 0);
        for (i, column) in (dot_cols - SCROLL..dot_cols).enumerate() {
            let noise = scatter_hash(0, 0, i, ctx.anim_frame()) * ROUGHNESS;
            state.heights[column] = (average + noise).min(1.0);
        }
    }
}

pub(super) fn render(state: &State, ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let dot_rows = usize::from(area.height) * 4;
    let dot_cols = usize::from(area.width) * 2;
    if dot_rows == 0 || state.heights.len() != dot_cols {
        return;
    }

    let mut grid = DotGrid::new(dot_rows, dot_cols);
    let span = (dot_rows - 1) as f32;
    for (x, height) in state.heights.iter().enumerate() {
        // Fill from the ridge line down, so the range is solid rock rather
        // than a bare outline.
        let top = span - height * span;
        for y in top as i64..dot_rows as i64 {
            grid.set(x as i64, y);
        }
    }

    let rows = area.height;
    grid.render(area, buf, |row| ctx.paint.row(rows - 1 - row, rows));
}
