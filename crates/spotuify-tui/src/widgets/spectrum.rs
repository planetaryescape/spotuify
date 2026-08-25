use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

use super::style::tokens;

pub(crate) struct SpectrumWidget<'a> {
    bands: &'a [f32; 12],
    color_scheme: SpectrumColorScheme,
    color_enabled: bool,
    accent: Option<Color>,
}

impl<'a> SpectrumWidget<'a> {
    pub(crate) fn new(bands: &'a [f32; 12]) -> Self {
        Self {
            bands,
            color_scheme: SpectrumColorScheme::SpotifyGreen,
            color_enabled: crate::widgets::terminal::color_enabled(),
            accent: None,
        }
    }

    pub(crate) fn color_scheme(mut self, value: SpectrumColorScheme) -> Self {
        self.color_scheme = value;
        self
    }

    pub(crate) fn accent(mut self, value: Option<Color>) -> Self {
        self.accent = value;
        self
    }

    pub(crate) fn color_enabled(mut self, value: bool) -> Self {
        self.color_enabled = value;
        self
    }
}

impl Widget for SpectrumWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        const BAND_COUNT: u16 = 12;
        const GLYPHS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

        if area.width == 0 || area.height == 0 {
            return;
        }

        let ascii = !self.color_enabled;
        let slab = (area.width / BAND_COUNT).max(1);
        // Reserve the rightmost column of each slab as a gap so adjacent
        // bands read as separate items. Fall back to the full slab when
        // the area is narrow enough (slab == 1) so every band still gets
        // at least one column.
        let bar_width = if slab >= 2 { slab - 1 } else { slab };
        for band in 0..BAND_COUNT {
            let magnitude = self
                .bands
                .get(band as usize)
                .copied()
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            let total_subcells = (magnitude * area.height as f32 * 8.0).round() as u32;
            let x0 = area.x + band * slab;
            let x_end = (x0 + bar_width).min(area.right());

            for row_from_bottom in 0..area.height {
                let cell_min = row_from_bottom as u32 * 8;
                let level = total_subcells.saturating_sub(cell_min).min(8) as usize;
                if level == 0 {
                    continue;
                }
                let y = area.bottom().saturating_sub(row_from_bottom + 1);
                let glyph = if ascii { '#' } else { GLYPHS[level] };
                let style = if ascii {
                    Style::default()
                } else {
                    Style::default().fg(spectrum_color(
                        row_from_bottom,
                        area.height,
                        self.color_scheme,
                        self.accent,
                    ))
                };
                for x in x0..x_end {
                    let cell = &mut buf[(x, y)];
                    cell.set_char(glyph);
                    cell.set_style(style);
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SpectrumColorScheme {
    SpotifyGreen,
    Rainbow,
    Monochrome,
}

impl SpectrumColorScheme {
    pub(crate) fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "rainbow" => Self::Rainbow,
            "monochrome" => Self::Monochrome,
            _ => Self::SpotifyGreen,
        }
    }
}

pub(crate) fn spectrum_color(
    row_from_bottom: u16,
    height: u16,
    scheme: SpectrumColorScheme,
    accent: Option<Color>,
) -> Color {
    spectrum_color_ratio(
        row_from_bottom as f32 / height.max(1) as f32,
        scheme,
        accent,
    )
}

/// The colour a bar cell gets at `ratio` of the panel height (0 = bottom row,
/// 1 = top). Split out of [`spectrum_color`] so the ported styles that colour
/// by intensity tier rather than by row can reuse the same palettes.
pub(crate) fn spectrum_color_ratio(
    ratio: f32,
    scheme: SpectrumColorScheme,
    accent: Option<Color>,
) -> Color {
    match scheme {
        SpectrumColorScheme::Monochrome => return Color::Gray,
        SpectrumColorScheme::Rainbow => {
            if ratio > 0.80 {
                return Color::Rgb(220, 90, 255);
            } else if ratio > 0.60 {
                return Color::Rgb(70, 140, 255);
            } else if ratio > 0.40 {
                return Color::Rgb(54, 220, 190);
            } else if ratio > 0.20 {
                return Color::Rgb(245, 225, 65);
            }
            return Color::Rgb(245, 95, 80);
        }
        SpectrumColorScheme::SpotifyGreen => {}
    }
    if let Some(accent) = accent {
        return accent;
    }
    // The three intensity tiers are exactly what cliamp's `red`/`yellow`/
    // `green` theme roles are for, so read them as tokens: under
    // `terminal-default` these are the same colours the panel always had,
    // and under a theme the spectrum matches the rest of the UI.
    if ratio > 0.75 {
        tokens::danger()
    } else if ratio > 0.45 {
        tokens::warn()
    } else {
        tokens::success()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_one_full_band(scheme: &str) -> Buffer {
        let area = Rect::new(0, 0, 12, 4);
        let mut buf = Buffer::empty(area);
        let mut bands = [0.0; 12];
        bands[0] = 1.0;
        SpectrumWidget::new(&bands)
            .color_scheme(SpectrumColorScheme::from_config(scheme))
            .color_enabled(true)
            .render(area, &mut buf);
        buf
    }

    #[test]
    fn monochrome_scheme_uses_one_color_for_lit_cells() {
        let buf = render_one_full_band("monochrome");

        let first = buf[(0, 0)].fg;
        assert_eq!(first, Color::Gray);
        for y in 0..4 {
            assert_eq!(buf[(0, y)].fg, first);
        }
    }

    #[test]
    fn rainbow_scheme_uses_distinct_vertical_colors() {
        let buf = render_one_full_band("rainbow");

        assert_eq!(buf[(0, 3)].fg, Color::Rgb(245, 95, 80));
        assert_eq!(buf[(0, 0)].fg, Color::Rgb(70, 140, 255));
        assert_ne!(buf[(0, 0)].fg, buf[(0, 3)].fg);
    }

    fn winamp() -> spotuify_core::ThemeSpec {
        spotuify_core::ThemeSpec {
            name: "winamp".to_string(),
            source: spotuify_core::ThemeSource::Builtin,
            bg: Some("#000000".to_string()),
            accent: Some("#00FF00".to_string()),
            bright_fg: Some("#FFFFFF".to_string()),
            fg: Some("#969696".to_string()),
            green: Some("#29CE10".to_string()),
            yellow: Some("#D6B521".to_string()),
            red: Some("#EF3110".to_string()),
        }
    }

    /// The three intensity tiers are what cliamp's `green`/`yellow`/`red`
    /// roles exist for, so a theme has to reach them.
    #[test]
    fn spotify_green_tiers_follow_the_active_theme() {
        crate::widgets::style::set_active_theme(&spotuify_core::ThemeSpec::terminal_default());
        let builtin = [0.2f32, 0.6, 0.9]
            .map(|ratio| spectrum_color_ratio(ratio, SpectrumColorScheme::SpotifyGreen, None));
        assert_eq!(
            builtin,
            [
                Color::Rgb(30, 215, 96),
                Color::Rgb(245, 185, 65),
                Color::Rgb(245, 88, 88),
            ],
            "terminal-default must keep the colours the panel always had"
        );

        crate::widgets::style::set_active_theme(&winamp());
        let themed = [0.2f32, 0.6, 0.9]
            .map(|ratio| spectrum_color_ratio(ratio, SpectrumColorScheme::SpotifyGreen, None));
        assert_eq!(
            themed,
            [
                Color::Rgb(0x29, 0xCE, 0x10),
                Color::Rgb(0xD6, 0xB5, 0x21),
                Color::Rgb(0xEF, 0x31, 0x10),
            ],
            "the tiers should be the theme's green/yellow/red"
        );

        // Rainbow and monochrome are fixed palettes by design — a theme
        // must not repaint them.
        for scheme in [
            SpectrumColorScheme::Rainbow,
            SpectrumColorScheme::Monochrome,
        ] {
            crate::widgets::style::set_active_theme(&spotuify_core::ThemeSpec::terminal_default());
            let before = [0.2f32, 0.6, 0.9].map(|ratio| spectrum_color_ratio(ratio, scheme, None));
            crate::widgets::style::set_active_theme(&winamp());
            let after = [0.2f32, 0.6, 0.9].map(|ratio| spectrum_color_ratio(ratio, scheme, None));
            assert_eq!(before, after, "{scheme:?} must ignore the theme");
        }

        crate::widgets::style::set_active_theme(&spotuify_core::ThemeSpec::terminal_default());
    }
}
