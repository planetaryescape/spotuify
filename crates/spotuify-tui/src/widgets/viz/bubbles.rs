//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_bubbles.go`.

use std::f32::consts::TAU;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::{band_avg, scatter_hash, DotGrid};
use super::Ctx;

/// Fixed population. Deriving the count from energy would spawn and vanish
/// bubbles mid-air every time the music changed volume.
const COUNT: u64 = 18;
/// Ring radius range in dots.
const MIN_RADIUS: f32 = 1.5;
const RADIUS_SPAN: f32 = 2.5;
/// Ring wall thickness, in dots.
const WALL: f32 = 0.9;
/// Radians of sway phase per frame; amplitude at silence plus the energy-driven
/// extra.
const SWAY_RATE: f32 = 0.03;
const BASE_SWAY: f32 = 1.5;
const ENERGY_SWAY: f32 = 2.5;
/// Extra dot rows above the panel so a bubble finishes popping off-screen.
const HEADROOM: i64 = 8;

/// Rising bubbles: a hollow Braille ring with a specular highlight, swaying as
/// it climbs and thinning out stochastically as it nears the surface.
///
/// Radius and therefore fall speed come from the bubble's index alone. Deriving
/// them from the live spectrum instead makes every trajectory parameter jitter
/// per frame, and the bubble flickers around the panel rather than rising.
pub(super) fn render(ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let dot_rows = usize::from(area.height) * 4;
    let dot_cols = usize::from(area.width) * 2;
    if dot_rows < 4 || dot_cols < 4 {
        return;
    }
    let mut grid = DotGrid::new(dot_rows, dot_cols);

    let energy = band_avg(ctx.bands, 0, ctx.bands.len());
    let sway_amplitude = BASE_SWAY + energy * ENERGY_SWAY;

    for bubble in 0..COUNT {
        let seed = bubble.wrapping_mul(104_729).wrapping_add(7_919);
        let radius = MIN_RADIUS + (seed % 100) as f32 / 100.0 * RADIUS_SPAN;

        // Bigger bubbles rise slower, which is what makes the field feel buoyant.
        let speed_divisor = 3 + radius as u64;
        let wrap = dot_rows as u64 + (radius * 2.0) as u64 + HEADROOM as u64;
        let base_y = seed.wrapping_mul(3_037) % wrap;
        let scrolled = base_y.wrapping_add(ctx.frame / speed_divisor) % wrap;
        let y = (wrap - 1 - scrolled) as i64 - radius as i64 - 2;

        let sway = (ctx.frame as f32 * SWAY_RATE + (seed % 1_000) as f32 / 1_000.0 * TAU).sin();
        let x = (seed % dot_cols as u64) as i64 + (sway * sway_amplitude) as i64;

        // Pop: the ring thins out over the last few rows before the surface.
        let pop_zone = radius as i64 + 3;
        let pop_fade = if y < pop_zone {
            (y as f32 / pop_zone as f32).max(0.0)
        } else {
            1.0
        };

        let inner = radius - WALL;
        let bbox = radius as i64 + 1;
        for dy in -bbox..=bbox {
            for dx in -bbox..=bbox {
                let dist = ((dx * dx + dy * dy) as f32).sqrt();
                if dist > radius || dist < inner {
                    continue;
                }
                // The pop pattern is frame-independent so a fading ring
                // dissolves steadily instead of strobing.
                if pop_fade < 1.0
                    && scatter_hash(
                        bubble as usize,
                        (dy + bbox) as usize,
                        (dx + bbox) as usize,
                        0,
                    ) > pop_fade
                {
                    continue;
                }
                grid.set(x + dx, y + dy);
            }
        }

        if radius >= 2.0 && pop_fade > 0.5 {
            let hx = x - (radius * 0.45) as i64;
            let hy = y - (radius * 0.45) as i64;
            for (dy, dx) in [(0, 0), (0, 1), (1, 0)] {
                grid.set(hx + dx, hy + dy);
            }
        }
    }

    let height = area.height;
    grid.render(area, buf, |row| ctx.paint.row(height - 1 - row, height));
}
