//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_scatter.go`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::{band_width, braille_char, scatter_hash, BRAILLE_BIT};
use super::{put, Ctx};

/// A twinkling Braille particle field. Dot density per band follows the
/// squared band level, biased downward so particles settle near the baseline.
pub(super) fn render(ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let height = area.height;
    let dot_rows = usize::from(height) * 4;
    let band_count = ctx.bands.len();

    for row in 0..height {
        let style = ctx.paint.row(height - 1 - row, height);
        let mut col = 0_u16;
        for (b, level) in ctx.bands.iter().enumerate() {
            for c in 0..band_width(band_count, b, area.width) {
                let mut bits = 0_u32;
                for (dr, bit_row) in BRAILLE_BIT.iter().enumerate() {
                    for (dc, bit) in bit_row.iter().enumerate() {
                        let dot_row = usize::from(row) * 4 + dr;
                        let dot_col = usize::from(c) * 2 + dc;
                        let gravity =
                            0.5 + 0.5 * dot_row as f32 / (dot_rows.saturating_sub(1).max(1)) as f32;
                        if scatter_hash(b, dot_row, dot_col, ctx.anim_frame())
                            < level * level * gravity
                        {
                            bits |= bit;
                        }
                    }
                }
                if bits != 0 {
                    put(buf, area, col, row, braille_char(bits), style);
                }
                col += 1;
            }
            if b + 1 < band_count {
                col += 1;
            }
        }
    }
}
