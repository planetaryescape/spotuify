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

fn render(style: VizStyle, area: Rect, color: bool, frames: u64) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
    let mut state = VizState::default();
    for _ in 0..frames {
        state.on_spectrum_frame();
        terminal
            .draw(|frame| {
                frame.render_stateful_widget(
                    VizWidget::new(&BANDS)
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
    for style in [VizStyle::ClassicPeak, VizStyle::ClassicLed, VizStyle::Flame] {
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

#[test]
fn stateful_styles_keep_moving_as_frames_arrive() {
    let area = Rect::new(0, 0, 40, 8);
    // The fire field is the one style whose output must change frame to frame
    // even on a constant spectrum: the propagation jitter is what animates it.
    let early = render(VizStyle::Flame, area, true, 4);
    let late = render(VizStyle::Flame, area, true, 40);
    assert_ne!(describe(&early), describe(&late));
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
    let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
    for entry in VIZ_STYLES {
        let mut state = VizState::default();
        for _ in 0..WARMUP_FRAMES {
            state.on_spectrum_frame();
            terminal
                .draw(|frame| {
                    frame.render_stateful_widget(
                        VizWidget::new(&silent).style(VizStyle::from_name(entry.name)),
                        area,
                        &mut state,
                    );
                })
                .unwrap();
        }
    }
}

#[test]
fn resizing_mid_animation_does_not_panic_or_wedge_a_style() {
    let mut state = VizState::default();
    for (width, height) in [(40_u16, 8_u16), (12, 3), (80, 20), (1, 1), (40, 8)] {
        let area = Rect::new(0, 0, width, height);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        for _ in 0..5 {
            state.on_spectrum_frame();
            terminal
                .draw(|frame| {
                    frame.render_stateful_widget(
                        VizWidget::new(&BANDS).style(VizStyle::Flame),
                        area,
                        &mut state,
                    );
                })
                .unwrap();
        }
    }
}
