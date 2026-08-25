//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_firework.go`.

use std::f32::consts::TAU;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::{band_avg, scatter_hash, DotGrid};
use super::Ctx;

/// Bursts in flight during silence, and how many more a full spectrum adds.
const BASE_BURSTS: u64 = 5;
const ENERGY_BURSTS: f32 = 9.0;
/// Frames from launch to the end of a burst's fade, and how many of those the
/// rising trail occupies.
const CYCLE: u64 = 48;
const LAUNCH: u64 = 10;
/// Explosion radius in dots at zero energy, plus the energy-driven extra.
const BASE_RADIUS: f32 = 3.0;
const ENERGY_RADIUS: f32 = 8.0;
/// Downward drift over a burst's life, in dots.
const GRAVITY: f32 = 5.0;
/// How much faster the particles vanish than the burst lasts — below 1.0 the
/// shell would still be fully lit when the next cycle starts.
const FADE_RATE: f32 = 1.3;
const BASE_PARTICLES: u32 = 18;
const ENERGY_PARTICLES: f32 = 18.0;

/// Firework bursts: a rising trail from the bottom, then a shell of particles
/// that expands fast, arcs down under gravity, and thins out stochastically.
/// Energy drives both how many shells are up and how big each one gets.
pub(super) fn render(ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let dot_rows = usize::from(area.height) * 4;
    let dot_cols = usize::from(area.width) * 2;
    if dot_rows < 4 || dot_cols < 4 || ctx.bands.is_empty() {
        return;
    }
    let mut grid = DotGrid::new(dot_rows, dot_cols);

    let energy = band_avg(ctx.bands, 0, ctx.bands.len());
    let bursts = BASE_BURSTS + (energy * ENERGY_BURSTS) as u64;

    for i in 0..bursts {
        // Re-seeding per cycle is what moves a burst somewhere new each time
        // it relaunches instead of firing from the same spot forever.
        let cycle = ctx.frame.wrapping_add(i.wrapping_mul(7)) / CYCLE;
        let seed = cycle
            .wrapping_mul(104_729)
            .wrapping_add(i.wrapping_mul(7_919));

        // Stagger so the shells don't all detonate on the same frame.
        let offset = i * CYCLE / bursts + (seed / 3) % 5;
        let local = ctx.frame.wrapping_add(offset) % CYCLE;

        let cx = (seed.wrapping_mul(6_271) % dot_cols as u64) as i64;
        let cy = (seed.wrapping_mul(4_391) % (dot_rows / 2) as u64) as i64 + (dot_rows / 8) as i64;

        let band = (seed % ctx.bands.len() as u64) as usize;
        let band_energy = ctx.bands[band];

        if local < LAUNCH {
            let progress = local as f32 / LAUNCH as f32;
            let top = (dot_rows - 1) as f32;
            let trail = top - (top - cy as f32) * progress;
            for dy in 0..4 {
                grid.set(cx, trail as i64 + dy);
            }
            continue;
        }

        let t = (local - LAUNCH) as f32 / (CYCLE - LAUNCH) as f32;
        // Fast expansion for the first third, then coasting.
        let radius = (BASE_RADIUS + band_energy * ENERGY_RADIUS) * (t * 3.0).min(1.0);
        let drop = t * t * GRAVITY;
        let fade = (1.0 - t * FADE_RATE).max(0.0);
        let particles = BASE_PARTICLES + (band_energy * ENERGY_PARTICLES) as u32;

        for p in 0..particles {
            if scatter_hash(band, p as usize, (seed % 100) as usize, ctx.frame) > fade {
                continue;
            }
            let angle = p as f32 / particles as f32 * TAU;
            let speed = 0.6 + (seed.wrapping_add(u64::from(p) * 2_909) % 400) as f32 / 1_000.0;
            grid.set(
                cx + (angle.cos() * radius * speed) as i64,
                cy + (angle.sin() * radius * speed + drop) as i64,
            );
        }
    }

    let height = area.height;
    grid.render(area, buf, |row| ctx.paint.row(height - 1 - row, height));
}
