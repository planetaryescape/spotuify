//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_rain.go`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::{band_width, scatter_hash};
use super::{put, Ctx};

/// Bar-shaped columns filled with falling streaks. The bar height still tracks
/// the band level, but the interior animates: a bright head, a body, and a
/// dimmer tail. Louder bands activate more columns.
pub(super) fn render(ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let height = area.height;
    let band_count = ctx.bands.len();

    for row in 0..height {
        let row_norm = f32::from(height - 1 - row) / f32::from(height);
        let mut col = 0_u16;
        for (b, level) in ctx.bands.iter().enumerate() {
            for _ in 0..band_width(band_count, b, area.width) {
                if row_norm < *level {
                    draw_drop(ctx, area, buf, b, col, row, *level);
                }
                col += 1;
            }
            if b + 1 < band_count {
                col += 1;
            }
        }
    }
}

fn draw_drop(
    ctx: &Ctx<'_>,
    area: Rect,
    buf: &mut Buffer,
    band: usize,
    col: u16,
    row: u16,
    level: f32,
) {
    // Column activation gate, re-rolled every 12 frames so streaks persist
    // instead of strobing. Higher energy opens more columns.
    if scatter_hash(band, 0, usize::from(col), ctx.frame / 12) > level * 1.6 + 0.1 {
        return;
    }
    let seed = u64::from(col).wrapping_mul(7919).wrapping_add(104_729);
    let speed = 1 + seed % 3;
    let drop_len = 2 + (seed / 7) % 3;
    let cycle_len = u64::from(area.height) + drop_len + 3;
    let offset = (seed / 13) % cycle_len;
    let pos = (ctx.frame / speed + offset) % cycle_len;
    let Some(dist) = pos.checked_sub(u64::from(row)) else {
        return;
    };
    if dist >= drop_len {
        return;
    }
    let (glyph, tier) = match dist {
        0 => ('┃', 2),
        1 => ('│', 1),
        _ => (':', 0),
    };
    put(buf, area, col, row, glyph, ctx.paint.tier(tier));
}
