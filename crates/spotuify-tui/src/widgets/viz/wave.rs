//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_wave.go`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::{sample_waveform, DotGrid};
use super::Ctx;

/// A Braille oscilloscope. One y-position per dot column, drawn as a
/// continuous trace by filling between each column and the one before it.
///
/// An empty waveform — a spectrum style's frame, or a daemon too old to send
/// one — traces the zero line, which is what silence looks like anyway.
pub(super) fn render(ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let dot_rows = usize::from(area.height) * 4;
    let dot_cols = usize::from(area.width) * 2;
    if dot_rows == 0 || dot_cols == 0 {
        return;
    }

    let positions: Vec<i64> = (0..dot_cols)
        .map(|x| dot_row_for(sample_waveform(ctx.waveform, x, dot_cols), dot_rows))
        .collect();

    let mut grid = DotGrid::new(dot_rows, dot_cols);
    for (x, y) in positions.iter().enumerate() {
        let previous = if x == 0 { *y } else { positions[x - 1] };
        for fill in *y.min(&previous)..=*y.max(&previous) {
            grid.set(x as i64, fill);
        }
    }

    let height = area.height;
    grid.render(area, buf, |row| ctx.paint.row(height - 1 - row, height));
}

/// Map a sample in `-1.0..=1.0` onto a dot row, `+1` at the top.
fn dot_row_for(sample: f32, dot_rows: usize) -> i64 {
    let span = (dot_rows - 1) as f32;
    (((1.0 - sample) * span / 2.0) as i64).clamp(0, span as i64)
}
