//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_mirror.go`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::BrailleGrid;
use super::Ctx;

/// Percentage of the panel width the mirrored bars occupy.
const SPAN_PERCENT: usize = 84;

/// Vertical bars around a persistent horizontal axis, mirrored above and
/// below it. Braille subcells keep the taper readable in a short panel.
pub(super) fn render(ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let dot_rows = usize::from(area.height) * 4;
    let dot_cols = usize::from(area.width) * 2;
    let span = (dot_cols * SPAN_PERCENT / 100).max(2);
    let span = dot_cols.min(span - span % 2);
    let bar_count = (span / 2).max(1);
    let x0 = (dot_cols - span) / 2;
    let axis_y = dot_rows / 2;
    let max_radius = axis_y.min(dot_rows.saturating_sub(1) - axis_y);

    let mut grid = BrailleGrid::new(dot_rows, dot_cols);
    for x in x0..x0 + span {
        grid.set(x, axis_y, 1);
    }

    let env = if ctx.bands.is_empty() {
        0.0
    } else {
        ctx.bands.iter().map(|b| b.clamp(0.0, 1.0)).sum::<f32>() / ctx.bands.len() as f32
    };

    // cliamp reads `frame * TickAnim` off its 60 Hz clock, which is seconds.
    let t = ctx.seconds();
    let half_bars = (bar_count - 1) as f32 / 2.0;
    for i in 0..bar_count {
        let distance = if half_bars > 0.0 {
            (i as f32 - half_bars).abs() / half_bars
        } else {
            0.0
        };
        // Two detuned sines so neighbouring bars breathe out of phase.
        let wobble = 0.4
            + 0.6 * ((t * 4.6 + i as f32 * 0.42).sin() * (t * 1.9 - i as f32 * 0.13).sin()).abs();
        let amplitude = dot_rows as f32
            * 0.80
            * (1.0 - distance * 0.55)
            * (0.3 + 0.7 * env)
            * (0.35 + 0.65 * wobble);
        let radius = max_radius.min(amplitude.round().max(1.0) as usize);
        let x = x0 + i * 2 + 1;
        for y in axis_y - radius..=axis_y + radius {
            let to_axis = y.abs_diff(axis_y);
            // Outer quarter of each bar is drawn a tier hotter, so the tips
            // read as the loud part rather than the whole bar flattening.
            let tier = if to_axis as f32 / radius as f32 >= 0.75 {
                3
            } else {
                2
            };
            grid.set(x, y, tier);
        }
    }

    grid.render(area, buf, ctx.paint);
}
