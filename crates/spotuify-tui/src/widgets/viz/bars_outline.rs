//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_bars_outline.go`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::{band_gap, band_width};
use super::{put, Ctx};

/// Only the top edge of each bar, drawn as a horizontal rule — a minimal
/// line-graph reading of the same spectrum.
pub(super) fn render(ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let height = area.height;
    let band_count = ctx.bands.len();

    for row in 0..height {
        let row_bottom = f32::from(height - 1 - row) / f32::from(height);
        let row_top = f32::from(height - row) / f32::from(height);
        let style = ctx.paint.row(height - 1 - row, height);
        let mut col = 0_u16;
        for (b, level) in ctx.bands.iter().enumerate() {
            let width = band_width(band_count, b, area.width);
            // The peak sits in this row when the level crosses it but does not
            // reach the row above; everything else is empty.
            if *level > row_bottom && *level < row_top {
                for _ in 0..width {
                    put(buf, area, col, row, '─', style);
                    col += 1;
                }
            } else {
                col += width;
            }
            if b + 1 < band_count {
                col += band_gap(band_count, area.width);
            }
        }
    }
}
