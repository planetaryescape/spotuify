//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_retro.go`.

use std::f32::consts::PI;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::{braille_char, sample_band_linear, BRAILLE_BIT};
use super::{put, Ctx};

/// Dot-grid layer ids. Priority when a cell mixes them is wave, then sun,
/// then grid — the wave is the part that reacts to audio.
const LAYER_GRID: u8 = 1;
const LAYER_WAVE: u8 = 2;
const LAYER_SUN: u8 = 3;

const V_LINES: usize = 18;
const H_LINES: usize = 10;

/// A synthwave scene: a striped sun over the horizon, an audio-reactive wave
/// along it, and a perspective grid floor scrolling toward the viewer.
pub(super) fn render(ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let dot_rows = usize::from(area.height) * 4;
    let dot_cols = usize::from(area.width) * 2;
    let horizon = (dot_rows * 2 / 5).max(2).min(dot_rows - 1);
    let floor_rows = dot_rows - horizon;
    let center_x = (dot_cols - 1) as f32 / 2.0;

    let mut grid = vec![0_u8; dot_rows * dot_cols];
    draw_sun(&mut grid, dot_cols, horizon, center_x);
    for dx in 0..dot_cols {
        grid[horizon * dot_cols + dx] = LAYER_GRID;
    }
    draw_floor(&mut grid, dot_rows, dot_cols, horizon, floor_rows, center_x);
    draw_floor_scroll(
        &mut grid,
        ctx.anim_frame(),
        dot_rows,
        dot_cols,
        horizon,
        floor_rows,
    );
    draw_wave(&mut grid, ctx.bands, dot_rows, dot_cols, horizon);

    for row in 0..area.height {
        for col in 0..area.width {
            let mut bits = 0_u32;
            let mut has_wave = false;
            let mut has_sun = false;
            for (dr, bit_row) in BRAILLE_BIT.iter().enumerate() {
                for (dc, bit) in bit_row.iter().enumerate() {
                    let dy = usize::from(row) * 4 + dr;
                    let dx = usize::from(col) * 2 + dc;
                    if dy >= dot_rows || dx >= dot_cols {
                        continue;
                    }
                    match grid[dy * dot_cols + dx] {
                        LAYER_GRID => bits |= bit,
                        LAYER_WAVE => {
                            bits |= bit;
                            has_wave = true;
                        }
                        LAYER_SUN => {
                            bits |= bit;
                            has_sun = true;
                        }
                        _ => {}
                    }
                }
            }
            if bits == 0 {
                continue;
            }
            let tier = if has_wave {
                2
            } else if has_sun {
                1
            } else {
                0
            };
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

/// Striped semicircle above the horizon; the lower half is banded so it reads
/// as the usual synthwave sun rather than a filled disc.
fn draw_sun(grid: &mut [u8], dot_cols: usize, horizon: usize, center_x: f32) {
    let radius = horizon as f32 * 0.85;
    for dy in 0..horizon {
        let row_dist = (horizon - dy) as f32;
        if row_dist > radius {
            continue;
        }
        if row_dist < radius * 0.5 {
            let stripe = ((radius * 0.15) as usize).max(1);
            if (row_dist as usize / stripe) % 2 == 1 {
                continue;
            }
        }
        let half = (radius * radius - row_dist * row_dist).sqrt();
        let left = (center_x - half).max(0.0) as usize;
        let right = ((center_x + half) as usize).min(dot_cols - 1);
        for dx in left..=right {
            grid[dy * dot_cols + dx] = LAYER_SUN;
        }
    }
}

/// Vertical floor lines converging on the vanishing point.
fn draw_floor(
    grid: &mut [u8],
    dot_rows: usize,
    dot_cols: usize,
    horizon: usize,
    floor_rows: usize,
    center_x: f32,
) {
    let denom = floor_rows.saturating_sub(1).max(1) as f32;
    for i in 0..=V_LINES {
        let bottom_x = i as f32 * (dot_cols - 1) as f32 / V_LINES as f32;
        for dy in horizon + 1..dot_rows {
            let t = (dy - horizon) as f32 / denom;
            let screen_x = center_x + (bottom_x - center_x) * t;
            let ix = screen_x.round();
            if ix >= 0.0 && (ix as usize) < dot_cols {
                grid[dy * dot_cols + ix as usize] = LAYER_GRID;
            }
        }
    }
}

/// Horizontal floor lines, spaced quadratically so they bunch at the horizon
/// and scroll toward the viewer.
fn draw_floor_scroll(
    grid: &mut [u8],
    frame: u64,
    dot_rows: usize,
    dot_cols: usize,
    horizon: usize,
    floor_rows: usize,
) {
    let scroll = (frame as f32 * 0.08) % 1.0;
    let depth = floor_rows.saturating_sub(2).max(1) as f32;
    for i in 0..H_LINES {
        let mut z = (i as f32 + scroll) / H_LINES as f32;
        if z > 1.0 {
            z -= 1.0;
        }
        let dy = horizon + 1 + (z * z * depth) as usize;
        if dy > horizon && dy < dot_rows {
            for dx in 0..dot_cols {
                grid[dy * dot_cols + dx] = LAYER_GRID;
            }
        }
    }
}

/// The spectrum drawn as a continuous curve sitting on the horizon.
fn draw_wave(grid: &mut [u8], bands: &[f32], dot_rows: usize, dot_cols: usize, horizon: usize) {
    if bands.is_empty() {
        return;
    }
    let max_wave = horizon as f32 * 0.85;
    let last = (bands.len() - 1) as f32;
    let mut previous: Option<usize> = None;
    for dx in 0..dot_cols {
        let pos = dx as f32 / (dot_cols - 1).max(1) as f32 * last;
        // Cosine easing between bands keeps the curve smooth at 12 bands.
        let index = pos as usize;
        let frac = pos - index as f32;
        let eased = index as f32 + (1.0 - (frac * PI).cos()) / 2.0;
        // A small floor keeps the wave visible during silence.
        let level = sample_band_linear(bands, eased).max(0.03);
        let y = (horizon as f32 - level * max_wave).max(0.0) as usize;
        let y = y.min(dot_rows - 1);
        grid[y * dot_cols + dx] = LAYER_WAVE;
        if let Some(prev) = previous {
            for fy in y.min(prev)..=y.max(prev) {
                grid[fy * dot_cols + dx] = LAYER_WAVE;
            }
        }
        previous = Some(y);
    }
}
