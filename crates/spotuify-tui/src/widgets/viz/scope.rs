//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_scope.go`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::DotGrid;
use super::Ctx;

/// Radians of wobble phase per wave-class tick.
const WOBBLE_RATE: f32 = 0.02;
/// Fraction of the buffer the phase delay wobbles either side of a quarter.
const WOBBLE_SPAN: f32 = 0.125;
/// Longest gap between consecutive plot points that still gets joined up.
/// Beyond this the signal jumped rather than travelled, and drawing the
/// connection would smear a chord across the figure.
const MAX_JOIN: i64 = 30;

/// A Lissajous XY scope. The audio tap is mono, so the Y axis is the same
/// signal phase-delayed; the delay drifts so the figure keeps evolving —
/// circles for a pure tone, knots for music.
pub(super) fn render(ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let dot_rows = usize::from(area.height) * 4;
    let dot_cols = usize::from(area.width) * 2;
    if dot_rows == 0 || dot_cols == 0 {
        return;
    }
    let mut grid = DotGrid::new(dot_rows, dot_cols);

    let n = ctx.waveform.len();
    if n <= 1 {
        // No signal: park the beam at the origin, like a real XY scope.
        grid.set(plot(0.0, dot_cols), plot(0.0, dot_rows));
    } else {
        let wobble = (ctx.wave_frame() as f32 * WOBBLE_RATE).sin() * (n as f32 * WOBBLE_SPAN);
        let delay = ((n / 4) as f32 + wobble).clamp(1.0, (n - 1) as f32) as usize;

        // Every pair is plotted. cliamp strides the buffer to cap the figure
        // at 512 points, which its 64-band FFT window needs and our 128
        // samples never reach.
        let mut previous: Option<(i64, i64)> = None;
        for i in 0..n - delay {
            let x = plot(ctx.waveform[i], dot_cols);
            let y = plot(-ctx.waveform[i + delay], dot_rows);
            grid.set(x, y);
            if let Some((px, py)) = previous {
                join(&mut grid, (px, py), (x, y));
            }
            previous = Some((x, y));
        }
    }

    let height = area.height;
    grid.render(area, buf, |row| ctx.paint.row(height - 1 - row, height));
}

/// Map a sample in `-1.0..=1.0` onto `0..span` dots.
fn plot(sample: f32, span: usize) -> i64 {
    let last = (span - 1) as f32;
    (((sample + 1.0) * 0.5 * last) as i64).clamp(0, last as i64)
}

/// Straight-line fill between two plot points so the figure reads as a curve
/// rather than a dot cloud.
fn join(grid: &mut DotGrid, from: (i64, i64), to: (i64, i64)) {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let steps = dx.abs().max(dy.abs());
    if steps == 0 || steps >= MAX_JOIN {
        return;
    }
    for s in 1..steps {
        grid.set(from.0 + dx * s / steps, from.1 + dy * s / steps);
    }
}
