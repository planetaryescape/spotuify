//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_pulse.go`.

use std::f32::consts::{PI, TAU};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::{braille_char, scatter_hash, BRAILLE_BIT};
use super::{put, Ctx};

/// Per-dot polar coordinates for the current panel size. Cached because the
/// render loop would otherwise run thousands of `sqrt`/`atan2` calls a frame.
#[derive(Debug, Default)]
pub(super) struct Coords {
    width: u16,
    height: u16,
    max_r: f32,
    dist: Vec<f32>,
    angle: Vec<f32>,
    rebuilds: u32,
}

impl Coords {
    pub(super) fn rebuilds(&self) -> u32 {
        self.rebuilds
    }

    fn ensure(&mut self, area: Rect) {
        if self.width == area.width && self.height == area.height && !self.dist.is_empty() {
            return;
        }
        let dot_rows = usize::from(area.height) * 4;
        let dot_cols = usize::from(area.width) * 2;
        let center_x = dot_cols as f32 / 2.0;
        let center_y = dot_rows as f32 / 2.0;
        // Terminal cells are roughly twice as tall as wide; scaling x keeps
        // the shape a circle rather than a flattened ellipse.
        let x_scale = center_y / center_x;

        let size = dot_rows * dot_cols;
        self.width = area.width;
        self.height = area.height;
        self.max_r = center_y - 1.0;
        self.dist = Vec::with_capacity(size);
        self.angle = Vec::with_capacity(size);
        self.rebuilds = self.rebuilds.saturating_add(1);
        for row in 0..usize::from(area.height) {
            for col in 0..usize::from(area.width) {
                for dr in 0..4 {
                    for dc in 0..2 {
                        let dx = ((col * 2 + dc) as f32 - center_x) * x_scale;
                        let dy = (row * 4 + dr) as f32 - center_y;
                        self.dist.push(dx.hypot(dy));
                        self.angle.push(dy.atan2(dx).rem_euclid(TAU));
                    }
                }
            }
        }
    }

    fn index(&self, row: u16, col: u16, dr: usize, dc: usize) -> usize {
        ((usize::from(row) * usize::from(self.width) + usize::from(col)) * 4 + dr) * 2 + dc
    }
}

/// A pulsating Braille ellipse. Its radius at each angle blends that angle's
/// band energy with the overall level, so the whole shape surges on a beat
/// while still deforming per frequency; transients throw off a shockwave ring.
pub(super) fn render(coords: &mut Coords, ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    if ctx.bands.is_empty() {
        return;
    }
    coords.ensure(area);
    let band_count = ctx.bands.len();
    let max_r = coords.max_r;
    let avg_energy = ctx.bands.iter().sum::<f32>() / band_count as f32;

    // Expanding ring that fades as it grows.
    let shock_phase = (ctx.frame as f32 * 0.10) % 1.0;
    let shock_r = max_r * (0.3 + 0.7 * shock_phase);
    let shock_strength = avg_energy * avg_energy * (1.0 - shock_phase * shock_phase);
    // Gentle breathing keeps the shape alive during silence.
    let breath = (ctx.frame as f32 * 0.05).sin() * 0.02;
    let rotation = ctx.frame as f32 * (0.015 + avg_energy * 0.04);
    let band_scale = band_count as f32 / TAU;

    for row in 0..area.height {
        for col in 0..area.width {
            let mut bits = 0_u32;
            let mut max_norm = 0.0_f32;
            for (dr, bit_row) in BRAILLE_BIT.iter().enumerate() {
                for (dc, bit) in bit_row.iter().enumerate() {
                    let idx = coords.index(row, col, dr, dc);
                    let dist = coords.dist[idx];
                    let angle = (coords.angle[idx] + rotation).rem_euclid(TAU);

                    let band_pos = angle * band_scale;
                    let band_idx = (band_pos as usize) % band_count;
                    let next_band = (band_idx + 1) % band_count;
                    let frac = band_pos - band_pos.floor();
                    let t = (1.0 - (frac * PI).cos()) / 2.0;
                    let energy = ctx.bands[band_idx] * (1.0 - t) + ctx.bands[next_band] * t;

                    // Blend per-band with the overall level so the whole shape
                    // beats instead of only the loud sector bulging.
                    let blended = energy * 0.6 + avg_energy * 0.4;
                    let radius = max_r * (0.08 + breath + 0.92 * blended * blended);

                    if radius > 0.5 && dist <= radius {
                        max_norm = max_norm.max(dist / radius);
                        bits |= bit;
                    } else if radius > 0.5 && dist < radius + 1.5 {
                        // Stochastic anti-aliased edge.
                        let fade = 1.0 - (dist - radius) / 1.5;
                        if scatter_hash(
                            band_idx,
                            usize::from(row) * 4 + dr,
                            usize::from(col) * 2 + dc,
                            ctx.frame,
                        ) < fade * 0.7
                        {
                            bits |= bit;
                            max_norm = max_norm.max(0.9);
                        }
                    }

                    if shock_strength > 0.05 {
                        let shock_dist = (dist - shock_r).abs();
                        let thickness = 0.6 + shock_strength * 1.5;
                        if shock_dist < thickness && 1.0 - shock_dist / thickness > 0.4 {
                            bits |= bit;
                            max_norm = max_norm.max(0.65);
                        }
                    }
                }
            }
            if bits == 0 {
                continue;
            }
            put(
                buf,
                area,
                col,
                row,
                braille_char(bits),
                ctx.paint.tier(spec_tier(max_norm)),
            );
        }
    }
}

/// Radial colour gradient: core, body, edge.
fn spec_tier(norm: f32) -> u8 {
    if norm >= 0.6 {
        2
    } else if norm >= 0.3 {
        1
    } else {
        0
    }
}
