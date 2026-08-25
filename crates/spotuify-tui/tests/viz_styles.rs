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
fn pulsing_bands(frame: u64) -> [f32; 12] {
    let gain = if frame % 6 < 3 { 0.15 } else { 1.0 };
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
    for style in [
        VizStyle::ClassicPeak,
        VizStyle::ClassicLed,
        VizStyle::Mosaic,
    ] {
        let early = render_animated(style, area, true, 4, &wave, pulsing_bands);
        let late = render_animated(style, area, true, 21, &wave, pulsing_bands);
        assert_ne!(
            describe(&early),
            describe(&late),
            "{} is frozen on a changing spectrum",
            style.as_str()
        );
    }
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

/// An older daemon sends no `waveform` at all, and every spectrum style's
/// frames carry none either. No style may panic on that, and the three that
/// need it must fall back to something legible.
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
