//! Geometry and sampling helpers shared by the ported spectrum styles.
//!
//! Derived from cliamp (MIT, © Bjarne Øverli): `ui/visualizer.go` and
//! `ui/vis_braillegrid.go`. See `THIRD_PARTY_LICENSES.md`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::{put, Painter};

/// Unicode block elements indexed by eighth-of-a-cell fill level.
const BAR_BLOCKS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Bit contributed by dot (row, col) of a cell's 4×2 Braille grid.
pub(super) const BRAILLE_BIT: [[u32; 2]; 4] =
    [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

const BRAILLE_BASE: u32 = 0x2800;

/// The Braille glyph for a set of dot bits. Bits are always in range, so the
/// fallback never fires in practice.
pub(super) fn braille_char(bits: u32) -> char {
    char::from_u32(BRAILLE_BASE | bits).unwrap_or(' ')
}

/// Character width of band `b`. At narrow widths only the leading bands get
/// columns; the inter-band gaps come out of the leftover space.
pub(super) fn band_width(total_bands: usize, b: usize, width: u16) -> u16 {
    let width = width as usize;
    if total_bands == 0 || b >= total_bands || width == 0 {
        return 0;
    }
    let visible = total_bands.min(width);
    if b >= visible {
        return 0;
    }
    let gaps = (visible - 1).min(width.saturating_sub(visible));
    let band_cols = width - gaps;
    let base = band_cols / visible;
    let extra = band_cols % visible;
    (if b < extra { base + 1 } else { base }) as u16
}

/// Per-column levels, interpolating linearly from each band to the next so
/// adjacent columns differ instead of stepping.
pub(super) fn interpolate_band_columns(bands: &[f32], band_cols: &[u16]) -> Vec<f32> {
    let total: usize = band_cols.iter().map(|w| *w as usize).sum();
    let mut cols = Vec::with_capacity(total);
    for (b, level) in bands.iter().enumerate() {
        let width = band_cols.get(b).copied().unwrap_or(0);
        if width == 0 {
            continue;
        }
        let next = bands.get(b + 1).copied().unwrap_or(*level);
        for c in 0..width {
            let t = f32::from(c) / f32::from(width);
            cols.push(level * (1.0 - t) + next * t);
        }
    }
    cols
}

/// Linear sample of `bands` at fractional index `pos`, clamped to both ends.
pub(super) fn sample_band_linear(bands: &[f32], pos: f32) -> f32 {
    match bands.len() {
        0 => return 0.0,
        1 => return bands[0],
        _ => {}
    }
    if pos <= 0.0 {
        return bands[0];
    }
    let last = (bands.len() - 1) as f32;
    if pos >= last {
        return bands[bands.len() - 1];
    }
    let idx = pos as usize;
    let frac = pos - idx as f32;
    bands[idx] * (1.0 - frac) + bands[idx + 1] * frac
}

/// Stretch (or squash) `bands` onto `total_cols` evenly spaced samples.
pub(super) fn resample_bands_linear(bands: &[f32], total_cols: usize) -> Vec<f32> {
    if total_cols == 0 || bands.is_empty() {
        return Vec::new();
    }
    if bands.len() == total_cols {
        return bands.to_vec();
    }
    if total_cols == 1 {
        return vec![sample_band_linear(bands, (bands.len() - 1) as f32 / 2.0)];
    }
    let last = (bands.len() - 1) as f32;
    (0..total_cols)
        .map(|col| sample_band_linear(bands, col as f32 / (total_cols - 1) as f32 * last))
        .collect()
}

/// Block glyph for `level` within the row spanning `[row_bottom, row_top]`.
pub(super) fn frac_block(level: f32, row_bottom: f32, row_top: f32) -> char {
    if level >= row_top {
        return '█';
    }
    if level > row_bottom {
        let frac = (level - row_bottom) / (row_top - row_bottom);
        let idx = ((frac * (BAR_BLOCKS.len() - 1) as f32) as usize).min(BAR_BLOCKS.len() - 1);
        return BAR_BLOCKS[idx];
    }
    ' '
}

/// Pseudo-random value in `[0, 1)` for a dot position and frame. Dots hold
/// their value for a few frames so particle fields twinkle rather than boil.
pub(super) fn scatter_hash(band: usize, row: usize, col: usize, frame: u64) -> f32 {
    let f = frame.wrapping_add((row * 3 + col) as u64) / 3;
    let mut h = (band as u64)
        .wrapping_mul(7919)
        .wrapping_add((row as u64).wrapping_mul(6271))
        .wrapping_add((col as u64).wrapping_mul(3037))
        .wrapping_add(f.wrapping_mul(104_729));
    h ^= h >> 16;
    h = h.wrapping_mul(0x45d9_f3b3_7197_344b);
    h ^= h >> 16;
    (h % 10_000) as f32 / 10_000.0
}

/// Advance a 64-bit LCG and return the next value in `[0, 1)`.
pub(super) fn rng_next(state: &mut u64) -> f32 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    ((*state >> 33) % 1000) as f32 / 1000.0
}

/// A 4×2-dots-per-cell raster target. Each dot carries a colour tier
/// (1..=3, 0 = unlit); `render` packs each cell's dots into one Braille glyph
/// coloured by the highest tier it contains.
pub(super) struct BrailleGrid {
    cells: Vec<u8>,
    dot_rows: usize,
    dot_cols: usize,
}

impl BrailleGrid {
    pub(super) fn new(dot_rows: usize, dot_cols: usize) -> Self {
        Self {
            cells: vec![0; dot_rows * dot_cols],
            dot_rows,
            dot_cols,
        }
    }

    pub(super) fn set(&mut self, x: usize, y: usize, tier: u8) {
        if x >= self.dot_cols || y >= self.dot_rows {
            return;
        }
        let cell = &mut self.cells[y * self.dot_cols + x];
        *cell = (*cell).max(tier);
    }

    pub(super) fn render(&self, area: Rect, buf: &mut Buffer, paint: &Painter) {
        for row in 0..area.height {
            for col in 0..area.width {
                let mut bits = 0_u32;
                let mut tier = 0_u8;
                for (dr, bit_row) in BRAILLE_BIT.iter().enumerate() {
                    for (dc, bit) in bit_row.iter().enumerate() {
                        let y = row as usize * 4 + dr;
                        let x = col as usize * 2 + dc;
                        if y >= self.dot_rows || x >= self.dot_cols {
                            continue;
                        }
                        let t = self.cells[y * self.dot_cols + x];
                        if t == 0 {
                            continue;
                        }
                        bits |= bit;
                        tier = tier.max(t);
                    }
                }
                if bits == 0 {
                    continue;
                }
                let style = paint.tier(tier.saturating_sub(1));
                put(buf, area, col, row, braille_char(bits), style);
            }
        }
    }
}
