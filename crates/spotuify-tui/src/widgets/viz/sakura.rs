//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_sakura.go`.

use std::f32::consts::TAU;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::DotGrid;
use super::Ctx;

/// Petal silhouettes as `(row, col)` dot offsets from the petal's origin,
/// largest (nearest) first. The first six fall slowly, the last three are
/// distant specks that fall twice as fast.
const SHAPES: [&[(i64, i64)]; 9] = [
    &[(0, 1), (1, 0), (1, 1), (1, 2), (2, 0), (2, 1)],
    &[(0, 1), (1, 0), (1, 1), (1, 2), (2, 1), (2, 2)],
    &[(0, 1), (0, 2), (1, 0), (1, 1), (1, 2), (2, 1)],
    &[(0, 1), (1, 0), (1, 1), (2, 0)],
    &[(0, 0), (1, 0), (1, 1), (2, 1)],
    &[(0, 0), (0, 1), (1, 1), (2, 1)],
    &[(0, 0), (1, 1)],
    &[(0, 1), (1, 0)],
    &[(0, 0), (0, 1), (1, 0)],
];

/// Index at and above which a shape counts as distant.
const DISTANT_FROM: usize = 6;
/// Petals on screen during silence, and how many more a full spectrum adds.
const BASE_PETALS: u32 = 12;
const ENERGY_PETALS: f32 = 16.0;
/// Dot rows of off-screen margin, so petals enter and leave rather than pop.
const MARGIN: i64 = 10;
/// Radians of sway phase per frame, and the sway's half-width in dots.
const SWAY_RATE: f32 = 0.015;
const SWAY_WIDTH: f32 = 3.0;

/// Cherry blossom petals drifting down. Each has its own shape, fall speed,
/// and sway phase derived from its index, so the field is deterministic but
/// never looks regular. Energy only controls how many are in the air.
pub(super) fn render(ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let dot_rows = usize::from(area.height) * 4;
    let dot_cols = usize::from(area.width) * 2;
    if dot_rows < 4 || dot_cols < 4 {
        return;
    }
    let mut grid = DotGrid::new(dot_rows, dot_cols);

    let energy = super::helpers::band_avg(ctx.bands, 0, ctx.bands.len());
    let petals = BASE_PETALS + (energy * ENERGY_PETALS) as u32;
    let wrap = dot_rows as u64 + MARGIN as u64;

    for petal in 0..u64::from(petals) {
        let seed = petal.wrapping_mul(104_729).wrapping_add(7_919);

        let shape_index = (seed.wrapping_mul(4_391) % SHAPES.len() as u64) as usize;
        let fall_speed = if shape_index >= DISTANT_FROM { 2 } else { 1 };

        // Wrapping scroll with the margin split above and below the panel.
        let base_y = seed.wrapping_mul(3_037) % wrap;
        let scrolled = base_y.wrapping_add(ctx.anim_frame().wrapping_mul(fall_speed) / 8);
        let y = (scrolled % wrap) as i64 - MARGIN / 2;

        let sway_phase = (seed % 1_000) as f32 / 1_000.0 * TAU;
        let sway = (ctx.anim_frame() as f32 * SWAY_RATE + sway_phase).sin() * SWAY_WIDTH;
        let x = (seed % dot_cols as u64) as i64 + sway as i64;

        for (dr, dc) in SHAPES[shape_index] {
            grid.set(x + dc, y + dr);
        }
    }

    let height = area.height;
    grid.render(area, buf, |row| ctx.paint.row(height - 1 - row, height));
}
