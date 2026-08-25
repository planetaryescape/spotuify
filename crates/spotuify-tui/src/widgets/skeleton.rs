//! Dim placeholder rows shown while a list's first fetch is in
//! flight, so "loading" and "actually empty" stop looking identical —
//! waiting with zero feedback was indistinguishable from a dead end.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::style::{border_strong, chip_bg};

/// `count` two-line placeholder rows (name + subtitle bars) shaped
/// like real media rows, with slight width variation so the block
/// reads as "content coming" rather than a solid slab.
pub fn skeleton_rows(count: usize, width: u16) -> Vec<Line<'static>> {
    let max_bar = (width as usize).saturating_sub(6).max(8);
    (0..count)
        .flat_map(|i| {
            let name_width = (14 + (i * 7) % 13).min(max_bar);
            let subtitle_width = (name_width.saturating_sub(5)).max(6).min(max_bar);
            [
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled("▮".repeat(name_width), Style::default().fg(chip_bg())),
                ]),
                Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        "▮".repeat(subtitle_width),
                        Style::default().fg(border_strong()),
                    ),
                ]),
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_come_in_pairs_and_fit_width() {
        let lines = skeleton_rows(3, 40);
        assert_eq!(lines.len(), 6);
        for line in &lines {
            assert!(line.width() <= 40);
        }
    }

    #[test]
    fn tiny_widths_do_not_underflow() {
        let lines = skeleton_rows(2, 4);
        assert_eq!(lines.len(), 4);
    }
}
