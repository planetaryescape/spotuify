//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_firefly.go`.

use std::f32::consts::TAU;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::{band_avg, BrailleGrid};
use super::Ctx;

const COUNT: u64 = 26;
/// `BrailleGrid` tiers (1-based): the grass silhouette is coolest, a lit fly
/// hottest, its halo in between.
const GRASS_TIER: u8 = 1;
const HALO_TIER: u8 = 2;
const LIT_TIER: u8 = 3;
/// Radians of blink phase per frame, and how much treble biases a fly toward
/// being lit.
const BLINK_RATE: f32 = 0.18;
const BLINK_GAIN: f32 = 0.4;
/// A fly lights when its blink phase plus the treble term clears this.
const BLINK_THRESHOLD: f32 = 0.55;
/// Dots of lateral push at full bass.
const WIND: f32 = 1.5;

/// A meadow at dusk. Each fly wanders a slow Lissajous path seeded by its
/// index, so no two share a trajectory; treble decides how often they light,
/// bass leans the whole swarm sideways.
pub(super) fn render(ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let dot_rows = usize::from(area.height) * 4;
    let dot_cols = usize::from(area.width) * 2;
    // Go's signed ints let cliamp survive a 1-row panel; the dot-space
    // arithmetic below is unsigned here, so two rows is the floor.
    if dot_rows < 8 || dot_cols < 8 {
        return;
    }
    let mut grid = BrailleGrid::new(dot_rows, dot_cols);

    let bands = ctx.bands.len();
    let bass = band_avg(ctx.bands, 0, (bands / 3).max(1));
    let treble = band_avg(ctx.bands, 2 * bands / 3, bands);

    let grass = grass_heights(dot_cols);
    for (x, height) in grass.iter().enumerate() {
        for d in 0..*height {
            if let Some(y) = dot_rows.checked_sub(1 + d) {
                grid.set(x, y, GRASS_TIER);
            }
        }
    }

    let t = ctx.anim_frame() as f32;
    for fly in 0..COUNT {
        let seed = fly.wrapping_mul(2_246_822_519).wrapping_add(11);
        // Two incommensurate frequencies, so the path never closes into a
        // repeating loop.
        let fx = 0.012 + (seed % 17) as f32 / 3_500.0;
        let fy = 0.018 + ((seed >> 4) % 19) as f32 / 2_900.0;
        let px = (seed % 1_000) as f32 / 1_000.0 * TAU;
        let py = ((seed >> 8) % 1_000) as f32 / 1_000.0 * TAU;

        let base_x = (dot_cols / 2) as f32 + (t * fx + px).cos() * (dot_cols - 6) as f32 * 0.45;
        let base_y =
            (dot_rows - 4) as f32 * 0.5 + (t * fy + py).sin() * (dot_rows - 6) as f32 * 0.4;
        let x = (base_x + bass * WIND * (t * 0.02 + px).sin()) as i64;
        let y = base_y as i64;
        if x < 0 || x >= dot_cols as i64 || y < 0 || y >= dot_rows as i64 - 1 {
            continue;
        }
        let (x, y) = (x as usize, y as usize);
        if y >= dot_rows.saturating_sub(grass[x]) {
            continue;
        }

        let blink = (t * BLINK_RATE + fly as f32 * 1.31).sin() * 0.5;
        if blink + 0.5 + treble * BLINK_GAIN <= BLINK_THRESHOLD {
            // Unlit flies still show faintly, so the swarm's shape persists.
            grid.set(x, y, HALO_TIER);
            continue;
        }

        grid.set(x, y, LIT_TIER);
        for (dy, dx) in [(-1_i64, 0_i64), (1, 0), (0, -1), (0, 1)] {
            let (hx, hy) = (x as i64 + dx, y as i64 + dy);
            if hx < 0 || hx >= dot_cols as i64 || hy < 0 || hy >= dot_rows as i64 {
                continue;
            }
            if (hy as usize) < dot_rows.saturating_sub(grass[hx as usize]) {
                grid.set(hx as usize, hy as usize, HALO_TIER);
            }
        }
    }

    grid.render(area, buf, ctx.paint);
}

/// Ragged grass silhouette: two out-of-phase sines so the skyline has no
/// visible period.
fn grass_heights(dot_cols: usize) -> Vec<usize> {
    (0..dot_cols)
        .map(|x| {
            let x = x as f32;
            1 + (2.5 + 1.5 * (x * 0.41).sin() + (x * 0.17 + 2.3).sin()) as usize
        })
        .collect()
}
