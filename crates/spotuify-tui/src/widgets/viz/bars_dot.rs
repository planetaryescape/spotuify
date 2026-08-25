//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_bars_dot.go`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::{band_width, braille_char, BRAILLE_BIT};
use super::{put, Ctx};

/// Bars drawn as Braille dots instead of solid blocks: each cell is a 4×2 dot
/// grid filled bottom-up in proportion to the band level, so the bar reads as
/// stippled texture rather than a slab.
pub(super) fn render(ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let height = area.height;
    let dot_rows = usize::from(height) * 4;
    let band_count = ctx.bands.len();

    for row in 0..height {
        let style = ctx.paint.row(height - 1 - row, height);
        let mut col = 0_u16;
        for (b, level) in ctx.bands.iter().enumerate() {
            for _ in 0..band_width(band_count, b, area.width) {
                let mut bits = 0_u32;
                for (dr, bit_row) in BRAILLE_BIT.iter().enumerate() {
                    let dot_row = usize::from(row) * 4 + dr;
                    let dot_y = (dot_rows - 1 - dot_row) as f32 / dot_rows as f32;
                    if dot_y < *level {
                        bits |= bit_row[0] | bit_row[1];
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
