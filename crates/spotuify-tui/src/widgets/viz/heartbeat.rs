//! Ported from cliamp (MIT, © Bjarne Øverli): `ui/vis_heartbeat.go`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::helpers::{sample_waveform, BrailleGrid};
use super::Ctx;

/// How far from the centre line a full-scale sample reaches.
const AMPLITUDE: f32 = 0.45;
/// Dashed baseline: `DASH` dots on, `DASH` off.
const DASH: usize = 6;
/// Tier for the trace itself (`BrailleGrid` tiers are 1-based).
const TRACE_TIER: u8 = 3;
/// Tier for the resting baseline.
const BASELINE_TIER: u8 = 1;

/// A hospital-monitor ECG trace. Squaring the sample magnitude (keeping its
/// sign) sharpens transients into QRS-style spikes and flattens low-level
/// noise into the dashed baseline.
pub(super) fn render(ctx: &Ctx<'_>, area: Rect, buf: &mut Buffer) {
    let dot_rows = usize::from(area.height) * 4;
    let dot_cols = usize::from(area.width) * 2;
    if dot_rows == 0 || dot_cols == 0 {
        return;
    }

    let centre = dot_rows as f32 / 2.0;
    let amplitude = dot_rows as f32 * AMPLITUDE;
    let last_row = (dot_rows - 1) as f32;
    let positions: Vec<usize> = (0..dot_cols)
        .map(|x| {
            let sample = sample_waveform(ctx.waveform, x, dot_cols);
            let shaped = sample * sample.abs();
            ((centre - shaped * amplitude).clamp(0.0, last_row)) as usize
        })
        .collect();

    let base = dot_rows / 2;
    let mut grid = BrailleGrid::new(dot_rows, dot_cols);
    for (x, y) in positions.iter().enumerate() {
        let previous = if x == 0 { *y } else { positions[x - 1] };
        for fill in *y.min(&previous)..=*y.max(&previous) {
            // A dot resting on the centre line is baseline, not trace, however
            // it got there. `BrailleGrid` keeps the hottest tier per cell, so
            // tiering per dot gives cliamp's per-cell rule for free: a cell is
            // trace-coloured exactly when something in it left the centre.
            // Without this, silence — every sample at the centre — paints a
            // flat line in the alarm colour.
            let tier = if fill == base {
                BASELINE_TIER
            } else {
                TRACE_TIER
            };
            grid.set(x, fill, tier);
        }
    }

    for x in (0..dot_cols).filter(|x| (x / DASH).is_multiple_of(2)) {
        grid.set(x, base, BASELINE_TIER);
    }

    grid.render(area, buf, ctx.paint);
}
