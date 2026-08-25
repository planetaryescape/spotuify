//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_binary.go`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::{band_width, scatter_hash};
use super::{put, Ctx};

/// Frames per scrolled row at silence. A loud band divides this down to 1, so
/// its column streams three times faster than a quiet one's.
const SLOWEST: f32 = 4.0;
/// Chance of a `1` at silence, and how much a full band adds.
const BASE_ONES: f32 = 0.15;
const ENERGY_ONES: f32 = 0.6;
/// Band level at which a `1` burns hottest, and at which a `0` stops being dim.
const HOT: f32 = 0.4;
const WARM: f32 = 0.3;

/// Columns of 0s and 1s streaming downward, each band at its own speed.
/// The bits themselves are a position hash, not a per-frame roll — scrolling
/// the sample point is what creates the motion, so the stream slides instead
/// of boiling.
pub(super) fn render(ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let count = ctx.bands.len();
    for row in 0..area.height {
        // usize: at u16::MAX width the column index outruns a u16.
        let mut column = 0_usize;
        for (b, level) in ctx.bands.iter().enumerate() {
            let speed = (SLOWEST - level * 3.0).max(1.0) as u64;
            let scroll = ctx.frame / speed;
            let ones = level * ENERGY_ONES + BASE_ONES;

            for _ in 0..band_width(count, b, area.width) {
                let sample_row = u64::from(row).wrapping_add(scroll) as usize;
                let one = scatter_hash(b, sample_row, column, 0) < ones;
                let tier = match (one, *level) {
                    (true, l) if l > HOT => 2,
                    (true, _) => 1,
                    (false, l) if l > WARM => 1,
                    (false, _) => 0,
                };
                let glyph = if one { '1' } else { '0' };
                put(buf, area, column as u16, row, glyph, ctx.paint.tier(tier));
                column += 1;
            }
            if b + 1 < count {
                column += 1;
            }
        }
    }
}
