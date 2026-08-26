//! Golden-buffer coverage for every spectrum style.
//!
//! Each style is rendered into a fixed-size `TestBackend` from a fixed band
//! set at a fixed frame, so the snapshots catch any change in glyphs, layout,
//! or colour tiers. Stateful styles are stepped a fixed number of frames
//! first, which is also what proves their physics is deterministic.

#![allow(clippy::unwrap_used)]

use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::Terminal;
use spotuify_protocol::VIZ_STYLES;
use spotuify_tui::widgets::viz::{VizState, VizStyle, VizWidget};

/// A spectrum with a clear shape: loud low end, a mid dip, a high-end bump.
/// Asymmetric on purpose so a renderer that mirrors or reverses its bands
/// produces a visibly different snapshot.
const BANDS: [f32; 12] = [
    0.95, 0.80, 0.62, 0.41, 0.18, 0.05, 0.22, 0.47, 0.70, 0.55, 0.33, 0.11,
];

/// Frames stepped before snapshotting, enough for a peak cap to launch, hold,
/// and start falling, and for the fire field to fill.
const WARMUP_FRAMES: u64 = 12;

/// Stateful styles: every one keeps physics between frames, so each must be
/// checked for determinism and for surviving a resize mid-animation.
const STATEFUL: [VizStyle; 7] = [
    VizStyle::ClassicPeak,
    VizStyle::ClassicLed,
    VizStyle::Flame,
    VizStyle::Terrain,
    VizStyle::Mosaic,
    VizStyle::Sand,
    VizStyle::Geyser,
];

/// Styles that trace `waveform` rather than `bands`.
const WAVEFORM_STYLES: [VizStyle; 3] = [VizStyle::Wave, VizStyle::Scope, VizStyle::Heartbeat];

/// One full sine cycle over the 128 points the daemon sends, so a trace that
/// reversed, mirrored, or dropped its samples snapshots differently.
fn waveform() -> Vec<f32> {
    (0..spotuify_protocol::VIZ_WAVEFORM_POINTS)
        .map(|i| {
            (i as f32 / spotuify_protocol::VIZ_WAVEFORM_POINTS as f32 * std::f32::consts::TAU).sin()
        })
        .collect()
}

fn render(style: VizStyle, area: Rect, color: bool, frames: u64) -> ratatui::buffer::Buffer {
    render_with(style, area, color, frames, &BANDS, &waveform())
}

fn render_with(
    style: VizStyle,
    area: Rect,
    color: bool,
    frames: u64,
    bands: &[f32; 12],
    wave: &[f32],
) -> ratatui::buffer::Buffer {
    render_animated(style, area, color, frames, wave, |_| *bands)
}

/// Like [`render_with`], but the spectrum may change each frame. `mosaic`
/// reaches a fixed point on a constant spectrum after a single step — ignite
/// to the band level, decay, ignite back — so a constant feed cannot show
/// that it animates at all.
fn render_animated(
    style: VizStyle,
    area: Rect,
    color: bool,
    frames: u64,
    wave: &[f32],
    bands_at: impl Fn(u64) -> [f32; 12],
) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
    let mut state = VizState::default();
    for frame in 0..frames {
        state.on_spectrum_frame();
        let bands = bands_at(frame);
        terminal
            .draw(|f| {
                f.render_stateful_widget(
                    VizWidget::new(&bands)
                        .waveform(wave)
                        .style(style)
                        .color_scheme("spotify-green")
                        .color_enabled(color),
                    area,
                    &mut state,
                );
            })
            .unwrap();
    }
    terminal.backend().buffer().clone()
}

/// A spectrum that swings between quiet and loud on a 6-frame period, so a
/// style driven by transients (a bass kick, a rising edge) actually sees one.
const PULSE_PERIOD: u64 = 6;

fn pulsing_bands(frame: u64) -> [f32; 12] {
    let gain = if frame % PULSE_PERIOD < PULSE_PERIOD / 2 {
        0.15
    } else {
        1.0
    };
    BANDS.map(|band| band * gain)
}

/// Every cell the buffer actually drew something in.
fn lit_cells(buffer: &ratatui::buffer::Buffer) -> usize {
    let area = buffer.area();
    (0..area.height)
        .flat_map(|y| (0..area.width).map(move |x| (x, y)))
        .filter(|(x, y)| !buffer[(*x, *y)].symbol().trim().is_empty())
        .count()
}

/// Snapshot both the glyphs and the per-cell foreground colours. The buffer's
/// own `Debug` is noisy, so this prints the panel as text plus a colour map
/// with one legend character per distinct colour.
fn describe(buffer: &ratatui::buffer::Buffer) -> String {
    let area = buffer.area();
    let mut palette: Vec<Color> = Vec::new();
    let mut glyphs = String::new();
    let mut colors = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buffer[(x, y)];
            glyphs.push_str(cell.symbol());
            let index = palette
                .iter()
                .position(|c| *c == cell.fg)
                .unwrap_or_else(|| {
                    palette.push(cell.fg);
                    palette.len() - 1
                });
            colors.push(char::from(b'a' + index as u8));
        }
        glyphs.push('\n');
        colors.push('\n');
    }
    let legend = palette
        .iter()
        .enumerate()
        .map(|(i, color)| format!("{} = {color:?}", char::from(b'a' + i as u8)))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{glyphs}\n{colors}\n{legend}")
}

#[test]
fn every_style_has_a_golden_frame_in_colour() {
    let area = Rect::new(0, 0, 40, 8);
    for entry in VIZ_STYLES {
        let style = VizStyle::from_name(entry.name);
        let buffer = render(style, area, true, WARMUP_FRAMES);
        insta::assert_snapshot!(format!("color_{}", entry.name), describe(&buffer));
    }
}

#[test]
fn every_style_has_a_golden_frame_without_colour() {
    let area = Rect::new(0, 0, 40, 8);
    for entry in VIZ_STYLES {
        let style = VizStyle::from_name(entry.name);
        let buffer = render(style, area, false, WARMUP_FRAMES);
        insta::assert_snapshot!(format!("nocolor_{}", entry.name), describe(&buffer));
    }
}

#[test]
fn stateful_styles_reach_the_same_frame_from_the_same_input() {
    let area = Rect::new(0, 0, 40, 8);
    for style in STATEFUL {
        let first = render(style, area, true, WARMUP_FRAMES);
        let second = render(style, area, true, WARMUP_FRAMES);
        assert_eq!(
            describe(&first),
            describe(&second),
            "{} is not deterministic",
            style.as_str()
        );
    }
}

/// Every stateful style has to visibly advance between frames. Determinism
/// and no-panic coverage both pass on a renderer whose `step` does nothing, so
/// this is the assertion that says the physics runs at all.
///
/// Most animate on their own under a held spectrum. `classic-peak`,
/// `classic-led`, and `mosaic` are driven by the input instead — caps fall to
/// rest, tiles settle at their band's level — so they get a pulsing feed.
#[test]
fn stateful_styles_keep_moving_as_frames_arrive() {
    let area = Rect::new(0, 0, 40, 8);
    let held: [(VizStyle, u64, u64); 4] = [
        (VizStyle::Flame, 4, 40),
        (VizStyle::Terrain, 4, 20),
        (VizStyle::Sand, 4, 24),
        (VizStyle::Geyser, 4, 24),
    ];
    for (style, early, late) in held {
        assert_ne!(
            describe(&render(style, area, true, early)),
            describe(&render(style, area, true, late)),
            "{} is frozen on a constant spectrum",
            style.as_str()
        );
    }

    let wave = waveform();

    // The two classic styles fall back to the live spectrum when their buffers
    // are unprimed, so a `step` that did nothing would still draw the current
    // bands. Both runs therefore have to END on the same pulse phase: the
    // final frame's input is then identical and only accumulated physics can
    // make the buffers differ. Comparing different phases lets a dead `step`
    // pass — verified, `classic-led` slips through 4-vs-21 and is caught by
    // 4-vs-22.
    let (early_frames, late_frames) = (4_u64, 22_u64);
    assert_eq!(
        (early_frames - 1) % PULSE_PERIOD,
        (late_frames - 1) % PULSE_PERIOD,
        "the two runs must end on the same pulse phase"
    );
    for style in [VizStyle::ClassicPeak, VizStyle::ClassicLed] {
        let early = render_animated(style, area, true, early_frames, &wave, pulsing_bands);
        let late = render_animated(style, area, true, late_frames, &wave, pulsing_bands);
        assert_ne!(
            describe(&early),
            describe(&late),
            "{} is frozen on a changing spectrum",
            style.as_str()
        );
    }

    // `mosaic` has no such fallback — it draws only what its tiles hold, so a
    // dead `step` renders an empty panel, and two empty panels compare equal.
    // Same-phase runs would not work here anyway: a tile ignites to its band's
    // level outright, so every run ending on the same loud frame lands in the
    // identical state. Compare different depths into the decay tail instead,
    // which is the only place its memory is observable.
    let shallow = render_animated(VizStyle::Mosaic, area, true, 4, &wave, pulsing_bands);
    let deep = render_animated(VizStyle::Mosaic, area, true, 21, &wave, pulsing_bands);
    assert_ne!(
        describe(&shallow),
        describe(&deep),
        "mosaic is frozen on a changing spectrum"
    );
}

/// A stateful style must draw something once it has run a few frames — the
/// motion check above compares two buffers, and two empty ones are equal in
/// all the wrong ways.
#[test]
fn stateful_styles_draw_something_once_primed() {
    let area = Rect::new(0, 0, 40, 8);
    for style in STATEFUL {
        let buffer = render(style, area, true, WARMUP_FRAMES);
        assert!(
            lit_cells(&buffer) > 0,
            "{} drew an empty panel",
            style.as_str()
        );
    }
}

#[test]
fn no_style_panics_on_degenerate_or_oversized_areas() {
    let areas = [
        Rect::new(0, 0, 1, 1),
        Rect::new(0, 0, 1, 40),
        Rect::new(0, 0, 200, 1),
        Rect::new(0, 0, 3, 2),
        Rect::new(0, 0, 200, 60),
        // The widest a ratatui Rect can be: the bar-slot arithmetic must not
        // overflow u16 on the way to computing column positions.
        Rect::new(0, 0, u16::MAX, 1),
    ];
    for entry in VIZ_STYLES {
        let style = VizStyle::from_name(entry.name);
        for area in areas {
            render(style, area, true, 3);
        }
    }
}

#[test]
fn no_style_panics_on_a_silent_spectrum() {
    let area = Rect::new(0, 0, 40, 8);
    let silent = [0.0_f32; 12];
    for entry in VIZ_STYLES {
        let style = VizStyle::from_name(entry.name);
        render_with(style, area, true, WARMUP_FRAMES, &silent, &waveform());
    }
}

/// A daemon older than the `waveform` field sends no samples at all. No style
/// may panic on that, and the three that trace one must fall back to
/// something legible.
#[test]
fn no_style_panics_without_a_waveform() {
    let area = Rect::new(0, 0, 40, 8);
    for entry in VIZ_STYLES {
        let style = VizStyle::from_name(entry.name);
        render_with(style, area, true, WARMUP_FRAMES, &BANDS, &[]);
    }
}

#[test]
fn scope_and_traces_degrade_to_a_resting_beam_without_a_waveform() {
    let area = Rect::new(0, 0, 40, 8);

    // A trace with nothing to trace is a flat line: one row, one repeated
    // glyph the whole way across, not a scattering of dots.
    for style in [VizStyle::Wave, VizStyle::Heartbeat] {
        let buffer = render_with(style, area, true, WARMUP_FRAMES, &BANDS, &[]);
        let rows = text_rows(&buffer);
        let drawn: Vec<&String> = rows.iter().filter(|r| !r.trim().is_empty()).collect();
        assert_eq!(
            drawn.len(),
            1,
            "{} should collapse to one row of trace, got {rows:?}",
            style.as_str()
        );
        let glyphs: std::collections::HashSet<char> =
            drawn[0].chars().filter(|c| *c != ' ').collect();
        assert_eq!(
            glyphs.len(),
            1,
            "{} should draw one repeated glyph, got {glyphs:?}",
            style.as_str()
        );
    }

    // The XY scope parks its beam at the origin instead — one mark, centred.
    let buffer = render_with(VizStyle::Scope, area, true, WARMUP_FRAMES, &BANDS, &[]);
    let lit: Vec<(u16, u16)> = (0..area.height)
        .flat_map(|y| (0..area.width).map(move |x| (x, y)))
        .filter(|(x, y)| buffer[(*x, *y)].symbol().trim() != "")
        .collect();
    // Dot-space centre of a 40×8 panel: dot column 39 of 80, dot row 15 of 32.
    assert_eq!(lit, vec![(19, 3)]);
}

/// Silence is a flat line on the centre row, and cliamp paints that row as
/// the resting baseline. Getting the tier order wrong paints it in the top
/// intensity colour instead — a monitor that alarms at rest.
#[test]
fn a_resting_heartbeat_is_drawn_in_the_baseline_colour() {
    let area = Rect::new(0, 0, 40, 8);
    let silent = [0.0_f32; 12];
    let resting = render_with(VizStyle::Heartbeat, area, true, WARMUP_FRAMES, &silent, &[]);
    insta::assert_snapshot!("resting_heartbeat", describe(&resting));

    // The tier the flat line is drawn in must be the one a bar's bottom row
    // gets, not the one its peak does.
    let baseline = drawn_colours(&resting);
    assert_eq!(
        baseline.len(),
        1,
        "a resting trace is one colour: {baseline:?}"
    );
    let bars = render_with(VizStyle::Bars, area, true, 1, &BANDS, &[]);
    let low = bars[(0, area.height - 1)].fg;
    let high = bars[(0, 0)].fg;
    assert_eq!(baseline[0], low, "resting trace should use the low tier");
    assert_ne!(baseline[0], high, "resting trace is drawn as a peak");
}

/// Distinct foreground colours across every cell that drew a glyph.
fn drawn_colours(buffer: &ratatui::buffer::Buffer) -> Vec<Color> {
    let area = buffer.area();
    let mut seen = Vec::new();
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buffer[(x, y)];
            if !cell.symbol().trim().is_empty() && !seen.contains(&cell.fg) {
                seen.push(cell.fg);
            }
        }
    }
    seen
}

fn text_rows(buffer: &ratatui::buffer::Buffer) -> Vec<String> {
    let area = buffer.area();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn waveform_styles_change_when_the_waveform_does() {
    let area = Rect::new(0, 0, 40, 8);
    let flat = vec![0.0_f32; spotuify_protocol::VIZ_WAVEFORM_POINTS];
    for style in WAVEFORM_STYLES {
        let quiet = render_with(style, area, true, WARMUP_FRAMES, &BANDS, &flat);
        let loud = render_with(style, area, true, WARMUP_FRAMES, &BANDS, &waveform());
        assert_ne!(
            describe(&quiet),
            describe(&loud),
            "{} ignores its waveform",
            style.as_str()
        );
    }
}

/// A stateful style's buffers are keyed on the panel size, so a resize throws
/// them away. It has to rebuild and keep drawing — silently rendering nothing
/// forever after a resize is the failure this guards, and it looks identical
/// to "no panic" from the outside.
#[test]
fn resizing_mid_animation_does_not_panic_or_wedge_a_style() {
    for style in STATEFUL {
        let mut state = VizState::default();
        let mut buffer = None;
        for (width, height) in [(40_u16, 8_u16), (12, 3), (80, 20), (1, 1), (40, 8)] {
            let area = Rect::new(0, 0, width, height);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            for _ in 0..5 {
                state.on_spectrum_frame();
                terminal
                    .draw(|frame| {
                        frame.render_stateful_widget(
                            VizWidget::new(&BANDS).style(style),
                            area,
                            &mut state,
                        );
                    })
                    .unwrap();
            }
            buffer = Some(terminal.backend().buffer().clone());
        }
        let buffer = buffer.unwrap();
        assert!(
            lit_cells(&buffer) > 0,
            "{} drew nothing after the resize sequence",
            style.as_str()
        );
    }
}

/// Motion parity with cliamp.
///
/// cliamp runs each mode off its own timer, so its per-tick constants are only
/// wall-clock-correct at that mode's rate; spotuify has one 30 Hz feed. Each
/// expectation below is a rate in real seconds, derived from cliamp's driver
/// cadence in `ui/tick.go` times the per-tick step in that mode's `ui/vis_*.go`,
/// then measured off rendered buffers over a two-second run. A style stepping
/// straight off the 30 Hz feed comes out exactly 1.5x fast and misses every one
/// of these by far more than the one-frame tolerance.
mod parity {
    use super::*;
    use std::collections::HashMap;

    /// `SpectrumFrame` rate, and cliamp's `TickFast` (50 ms) — the cadence
    /// behind every anim-class style.
    const FEED_HZ: u64 = 30;
    const CLIAMP_ANIM_HZ: u64 = 20;

    const RUN_SECONDS: u64 = 2;
    /// Frames in the measurement window.
    const RUN_FRAMES: u64 = FEED_HZ * RUN_SECONDS;
    /// cliamp ticks in the same window: what the port must actually advance by.
    const RUN_TICKS: u64 = CLIAMP_ANIM_HZ * RUN_SECONDS;

    /// Render `frames` frames and pull one measurement out of each.
    fn capture<T>(
        style: VizStyle,
        area: Rect,
        frames: u64,
        bands_at: impl Fn(u64) -> [f32; 12],
        measure: impl Fn(&ratatui::buffer::Buffer) -> T,
    ) -> Vec<T> {
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        let mut state = VizState::default();
        let mut out = Vec::with_capacity(frames as usize);
        for frame in 0..frames {
            state.on_spectrum_frame();
            let bands = bands_at(frame);
            terminal
                .draw(|f| {
                    f.render_stateful_widget(
                        VizWidget::new(&bands)
                            .style(style)
                            .color_scheme("spotify-green")
                            .color_enabled(true),
                        area,
                        &mut state,
                    );
                })
                .unwrap();
            out.push(measure(terminal.backend().buffer()));
        }
        out
    }

    /// Row of `glyph` in each column, if it is on screen at all.
    fn glyph_row_per_column(buf: &ratatui::buffer::Buffer, glyph: &str) -> Vec<Option<u16>> {
        let area = buf.area();
        (0..area.width)
            .map(|x| (0..area.height).find(|y| buf[(x, *y)].symbol() == glyph))
            .collect()
    }

    /// `ui/vis_rain.go` puts a drop's bright head at `pos = frame/speed +
    /// offset`, so a column descends one row every `speed` ticks, and `speed =
    /// 1 + seed%3` bottoms out at 1. The fastest columns therefore fall at
    /// cliamp's whole tick rate: 20 rows a second, 40 rows in two seconds.
    #[test]
    fn rain_drops_fall_at_cliamp_rate() {
        // Tall enough that a head starting at row 0 is still on screen 40 rows
        // later, and that the fall cycle cannot wrap inside the window.
        let area = Rect::new(0, 0, 40, 60);
        // A saturated spectrum opens every column's activation gate.
        let heads = capture(
            VizStyle::Rain,
            area,
            RUN_FRAMES * 3,
            |_| [1.0; 12],
            |buf| glyph_row_per_column(buf, "┃"),
        );

        let mut fastest = 0_u16;
        for column in 0..usize::from(area.width) {
            for start in 0..heads.len() - RUN_FRAMES as usize {
                if heads[start][column] != Some(0) {
                    continue;
                }
                if let Some(row) = heads[start + RUN_FRAMES as usize][column] {
                    fastest = fastest.max(row);
                }
            }
        }
        assert!(
            fastest.abs_diff(RUN_TICKS as u16) <= 1,
            "rain's fastest drop fell {fastest} rows in {RUN_SECONDS}s; cliamp falls {RUN_TICKS}"
        );
    }

    /// `ui/vis_terrain.go` shifts the height field left by two dot columns —
    /// one terminal cell — every tick, so the ridge/plain boundary walks 20
    /// cells a second and 40 cells in two seconds.
    #[test]
    fn terrain_scrolls_at_cliamp_rate() {
        let area = Rect::new(0, 0, 60, 8);
        // Fill the whole field with full-height ridge first (one cell per tick,
        // so 60 cells needs more than 60 ticks), then cut to silence: the flat
        // plain enters from the right and the boundary is the count of columns
        // still reaching the top row.
        let loud_frames = 120_u64;
        let ridge = capture(
            VizStyle::Terrain,
            area,
            loud_frames + RUN_FRAMES + 1,
            |frame| {
                if frame < loud_frames {
                    [1.0; 12]
                } else {
                    [0.0; 12]
                }
            },
            |buf| {
                (0..buf.area().width)
                    .filter(|x| !buf[(*x, 0)].symbol().trim().is_empty())
                    .count()
            },
        );

        let start = ridge[loud_frames as usize];
        let end = ridge[(loud_frames + RUN_FRAMES) as usize];
        assert_eq!(start, usize::from(area.width), "the ridge never filled");
        let travelled = start - end;
        assert!(
            travelled.abs_diff(RUN_TICKS as usize) <= 1,
            "terrain scrolled {travelled} cells in {RUN_SECONDS}s; cliamp scrolls {RUN_TICKS}"
        );
    }

    /// Dot bit contributed by each `(row, col)` of a cell's Braille grid.
    const BRAILLE_BIT: [[u32; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

    /// Lit dots per dot row. Petals move vertically plus a horizontal sway, so
    /// collapsing the panel onto its rows isolates the fall from the sway.
    fn dots_per_row(buf: &ratatui::buffer::Buffer) -> Vec<u32> {
        let area = buf.area();
        let mut rows = vec![0_u32; usize::from(area.height) * 4];
        for y in 0..area.height {
            for x in 0..area.width {
                let Some(bits) = buf[(x, y)]
                    .symbol()
                    .chars()
                    .next()
                    .map(u32::from)
                    .and_then(|c| c.checked_sub(0x2800))
                    .filter(|bits| *bits < 0x100)
                else {
                    continue;
                };
                for (dr, bit_row) in BRAILLE_BIT.iter().enumerate() {
                    for bit in bit_row {
                        if bits & bit != 0 {
                            rows[usize::from(y) * 4 + dr] += 1;
                        }
                    }
                }
            }
        }
        rows
    }

    /// Widest fall [`best_shift`] will consider, comfortably above the 10 dot
    /// rows even the distant petals manage in the window.
    const MAX_SHIFT: usize = 15;

    /// Downward shift that best lines `before` up with `after`.
    fn best_shift(before: &[u32], after: &[u32]) -> usize {
        (0..=MAX_SHIFT)
            .max_by_key(|shift| {
                before
                    .iter()
                    .enumerate()
                    .map(|(row, count)| {
                        u64::from((*count).min(after.get(row + shift).copied().unwrap_or(0)))
                    })
                    .sum::<u64>()
            })
            .unwrap_or(0)
    }

    /// `ui/vis_sakura.go` advances a petal by `frame*fallSpeed/8` dot rows, and
    /// `fallSpeed` is 1 for the six near shapes, 2 for the three distant ones.
    /// Near petals therefore fall 20/8 = 2.5 dot rows a second — 5 dot rows in
    /// two seconds — and are the majority of the field, so they are the shift
    /// the whole panel lines up on.
    #[test]
    fn sakura_petals_fall_at_cliamp_rate() {
        let area = Rect::new(0, 0, 60, 20);
        let profiles = capture(
            VizStyle::Sakura,
            area,
            RUN_FRAMES * 2,
            |_| [0.0; 12],
            dots_per_row,
        );

        let near_fall = (RUN_TICKS / 8) as usize;
        let mut votes: HashMap<usize, usize> = HashMap::new();
        for start in 0..RUN_FRAMES as usize {
            let shift = best_shift(&profiles[start], &profiles[start + RUN_FRAMES as usize]);
            *votes.entry(shift).or_default() += 1;
        }
        let winner = votes
            .iter()
            .max_by_key(|(_, count)| **count)
            .map(|(shift, _)| *shift)
            .unwrap();
        assert!(
            winner.abs_diff(near_fall) <= 1,
            "sakura's petal field fell {winner} dot rows in {RUN_SECONDS}s; \
             cliamp falls {near_fall} (votes: {votes:?})"
        );
    }
}
