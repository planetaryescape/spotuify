//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_columns.go`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::{band_width, frac_block, interpolate_band_columns};
use super::{put, Ctx};

/// One thin column per character cell, interpolated between bands so adjacent
/// columns differ slightly and the spectrum reads as a dense curve.
pub(super) fn render(ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let height = area.height;
    let band_count = ctx.bands.len();
    let band_cols: Vec<u16> = (0..band_count)
        .map(|b| band_width(band_count, b, area.width))
        .collect();
    let levels = interpolate_band_columns(ctx.bands, &band_cols);

    for row in 0..height {
        let row_bottom = f32::from(height - 1 - row) / f32::from(height);
        let row_top = f32::from(height - row) / f32::from(height);
        let style = ctx.paint.row(height - 1 - row, height);
        let mut col = 0_u16;
        let mut level_index = 0_usize;
        for (b, width) in band_cols.iter().enumerate() {
            for _ in 0..*width {
                let level = levels.get(level_index).copied().unwrap_or(0.0);
                level_index += 1;
                let glyph = frac_block(level, row_bottom, row_top);
                if glyph != ' ' {
                    put(buf, area, col, row, glyph, style);
                }
                col += 1;
            }
            if b + 1 < band_count {
                col += 1;
            }
        }
    }
}
