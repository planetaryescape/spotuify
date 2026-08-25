//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_butterfly.go`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::{sample_band_linear, scatter_hash, DotGrid};
use super::Ctx;

/// Radians of wing-wobble phase per frame, and per dot row — the row term is
/// what gives the silhouette its rippling edge instead of a clean lens shape.
const WOBBLE_RATE: f32 = 0.08;
const WOBBLE_PER_ROW: f32 = 0.3;
const WOBBLE_DEPTH: f32 = 0.15;
/// Widest a wing reaches, as a fraction of the half-panel.
const REACH: f32 = 0.9;
/// Beyond this fraction of the wing, the edge flickers.
const EDGE_FROM: f32 = 0.6;
const FLICKER_RATE: f32 = 0.1;
/// Energy below which a row's spine stops being drawn.
const SPINE_FLOOR: f32 = 0.05;

/// A symmetric ink-blot. Each dot row samples one point of the spectrum and
/// extends a wing either side of the centre; a sine wobble and a stochastic
/// edge keep the shape organic rather than geometric.
pub(super) fn render(ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let dot_rows = usize::from(area.height) * 4;
    let dot_cols = usize::from(area.width) * 2;
    if dot_rows == 0 || dot_cols == 0 || ctx.bands.is_empty() {
        return;
    }
    let mut grid = DotGrid::new(dot_rows, dot_cols);

    let centre = (dot_cols / 2) as i64;
    let last_band = (ctx.bands.len() - 1) as f32;
    let last_row = (dot_rows - 1).max(1) as f32;

    for dy in 0..dot_rows {
        let position = dy as f32 / last_row * last_band;
        let band = position as usize;
        let energy = sample_band_linear(ctx.bands, position);

        let wobble =
            (ctx.frame as f32 * WOBBLE_RATE + dy as f32 * WOBBLE_PER_ROW).sin() * WOBBLE_DEPTH;
        let wing = (centre as f32 * (energy + wobble) * REACH) as i64;

        for dx in 0..wing.max(0) {
            let norm = dx as f32 / wing.max(1) as f32;
            // Dense at the body, sparse at the tips.
            let mut threshold = (1.0 - norm * norm) * energy;
            if norm > EDGE_FROM {
                threshold *= 0.5
                    + 0.5
                        * (ctx.frame as f32 * FLICKER_RATE + dy as f32 * 0.5 + dx as f32 * 0.3)
                            .sin();
            }
            if scatter_hash(band, dy, dx as usize, ctx.frame / 3) < threshold {
                grid.set(centre + dx, dy as i64);
                grid.set(centre - 1 - dx, dy as i64);
            }
        }

        if energy > SPINE_FLOOR {
            grid.set(centre, dy as i64);
            grid.set(centre - 1, dy as i64);
        }
    }

    // Unlike the other Braille styles this one grades top-to-bottom, so the
    // wings read as one body rather than as stacked bars.
    let rows = area.height;
    grid.render(area, buf, |row| ctx.paint.row(row, rows));
}
