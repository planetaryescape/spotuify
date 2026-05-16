use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

pub struct SpectrumWidget<'a> {
    bands: &'a [f32; 12],
    color_scheme: SpectrumColorScheme,
    color_enabled: bool,
}

impl<'a> SpectrumWidget<'a> {
    pub fn new(bands: &'a [f32; 12]) -> Self {
        Self {
            bands,
            color_scheme: SpectrumColorScheme::SpotifyGreen,
            color_enabled: std::env::var_os("NO_COLOR").is_none(),
        }
    }

    pub fn color_scheme(mut self, value: &str) -> Self {
        self.color_scheme = SpectrumColorScheme::from_config(value);
        self
    }

    #[cfg(test)]
    fn force_color(mut self) -> Self {
        self.color_enabled = true;
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
        for band in 0..BAND_COUNT {
            let magnitude = self
                .bands
                .get(band as usize)
                .copied()
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            let total_subcells = (magnitude * area.height as f32 * 8.0).round() as u32;
            let x0 = area.x + band * slab;
            let x_end = (x0 + slab).min(area.right());

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
enum SpectrumColorScheme {
    SpotifyGreen,
    Rainbow,
    Monochrome,
}

impl SpectrumColorScheme {
    fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "rainbow" => Self::Rainbow,
            "monochrome" => Self::Monochrome,
            _ => Self::SpotifyGreen,
        }
    }
}

fn spectrum_color(row_from_bottom: u16, height: u16, scheme: SpectrumColorScheme) -> Color {
    let ratio = row_from_bottom as f32 / height.max(1) as f32;
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
    if ratio > 0.75 {
        Color::Rgb(245, 88, 88)
    } else if ratio > 0.45 {
        Color::Rgb(245, 185, 65)
    } else {
        Color::Rgb(30, 215, 96)
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
            .color_scheme(scheme)
            .force_color()
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
}
