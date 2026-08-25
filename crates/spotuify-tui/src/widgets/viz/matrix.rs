//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_matrix.go`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::{band_width, scatter_hash};
use super::{put, Ctx};

/// Half-width katakana plus digits, the usual digital-rain alphabet.
const MATRIX_CHARS: [char; 41] = [
    'ｦ', 'ｧ', 'ｨ', 'ｩ', 'ｪ', 'ｫ', 'ｬ', 'ｭ', 'ｮ', 'ｯ', 'ｰ', 'ｱ', 'ｲ', 'ｳ', 'ｴ', 'ｵ', 'ｶ', 'ｷ', 'ｸ',
    'ｹ', 'ｺ', 'ｻ', 'ｼ', 'ｽ', 'ｾ', 'ｿ', 'ﾀ', 'ﾁ', 'ﾂ', 'ﾃ', 'ﾄ', '0', '1', '2', '3', '4', '5', '6',
    '7', '8', '9',
];

/// Falling character streams. Each column has a fixed fall speed derived from
/// its position; band energy decides how many columns are raining.
pub(super) fn render(ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let band_count = ctx.bands.len();

    for row in 0..area.height {
        let mut col = 0_u16;
        for (b, level) in ctx.bands.iter().enumerate() {
            for _ in 0..band_width(band_count, b, area.width) {
                draw_stream(ctx, area, buf, b, col, row, *level);
                col += 1;
            }
            if b + 1 < band_count {
                col += 1;
            }
        }
    }
}

fn draw_stream(
    ctx: &Ctx<'_>,
    area: Rect,
    buf: &mut Buffer,
    band: usize,
    col: u16,
    row: u16,
    energy: f32,
) {
    // Stable activation gate, re-rolled every 20 frames.
    if scatter_hash(band, 0, usize::from(col), ctx.frame / 20) > energy * 1.5 + 0.1 {
        return;
    }
    let seed = u64::from(col).wrapping_mul(7919).wrapping_add(104_729);
    let speed = 2 + seed % 3;
    let trail_len = 3 + (seed / 7) % 3;
    let cycle_len = u64::from(area.height) + trail_len + 4;
    let offset = (seed / 13) % cycle_len;
    let pos = (ctx.frame / speed + offset) % cycle_len;
    let Some(dist) = pos.checked_sub(u64::from(row)) else {
        return;
    };
    if dist > trail_len {
        return;
    }
    // The glyph mutates roughly every 4 frames so the trail shimmers.
    let char_seed = seed ^ (u64::from(row).wrapping_mul(31) + (ctx.frame / 4).wrapping_mul(17));
    let glyph = MATRIX_CHARS[(char_seed % MATRIX_CHARS.len() as u64) as usize];
    let tier = match dist {
        0 => 2,
        1..=2 => 1,
        _ => 0,
    };
    put(buf, area, col, row, glyph, ctx.paint.tier(tier));
}
