//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_bricks.go`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::band_width;
use super::{put, Ctx};

/// Solid columns built from half-height blocks, so each lit row reads as a
/// separate brick with a gap above it.
pub(super) fn render(ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let height = area.height;
    let band_count = ctx.bands.len();

    for row in 0..height {
        let threshold = f32::from(height - 1 - row) / f32::from(height);
        let style = ctx.paint.row(height - 1 - row, height);
        let mut col = 0_u16;
        for (b, level) in ctx.bands.iter().enumerate() {
            let width = band_width(band_count, b, area.width);
            if *level > threshold {
                for _ in 0..width {
                    put(buf, area, col, row, '▄', style);
                    col += 1;
                }
            } else {
                col += width;
            }
            if b + 1 < band_count {
                col += 1;
            }
        }
    }
}
