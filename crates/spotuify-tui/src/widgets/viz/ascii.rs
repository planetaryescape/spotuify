//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_ascii.go`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::resample_bands_linear;
use super::{classic_peak::columns_for, put, Ctx};

/// Shade glyphs by quarter of a row filled, lightest first.
const SHADES: [char; 3] = ['░', '▒', '▓'];

/// Thin single-character columns drawn with shade blocks instead of the
/// eighth-height block elements `bars` uses, on the same dense 1-wide /
/// 1-gap layout as `classic-peak`. Reads on terminals whose font lacks the
/// partial block glyphs.
pub(super) fn render(ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let levels = resample_bands_linear(ctx.bands, columns_for(area.width));
    let height = area.height;
    for row in 0..height {
        let row_bottom = f32::from(height - 1 - row) / f32::from(height);
        let row_top = f32::from(height - row) / f32::from(height);
        let style = ctx.paint.row(height - 1 - row, height);
        for (col, level) in levels.iter().enumerate() {
            let Some(glyph) = shade(*level, row_bottom, row_top) else {
                continue;
            };
            // usize throughout: at u16::MAX width there are more slots than
            // fit in a u16.
            let x = col * 2;
            if x >= usize::from(area.width) {
                break;
            }
            put(buf, area, x as u16, row, glyph, style);
        }
    }
}

/// Which shade `level` fills the row `[row_bottom, row_top]` with, or `None`
/// when the bar does not reach this row at all.
fn shade(level: f32, row_bottom: f32, row_top: f32) -> Option<char> {
    if level >= row_top {
        return Some('█');
    }
    if level <= row_bottom {
        return None;
    }
    let quarter = ((level - row_bottom) / (row_top - row_bottom) * 4.0) as usize;
    quarter.checked_sub(1).map(|i| SHADES[i.min(2)])
}
