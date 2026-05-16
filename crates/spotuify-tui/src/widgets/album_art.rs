//! Deterministic 2-colour gradient art fallback.
//!
//! When `ratatui-image` can't render the cover (no image bytes,
//! protocol unsupported, fetch failed), we draw a diagonal gradient
//! seeded on the track id so a given track always shows the same
//! pattern. Result: the player never falls back to a "broken-looking"
//! ASCII glyph, and similar tracks (same album, same artist seed)
//! look related.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

#[derive(Clone, Debug)]
pub struct GradientArt {
    seed: u64,
    label: Option<String>,
}

impl GradientArt {
    pub fn new(seed_text: &str) -> Self {
        let mut hasher = DefaultHasher::new();
        seed_text.hash(&mut hasher);
        Self {
            seed: hasher.finish(),
            label: None,
        }
    }

    /// Optional centred caption (typically the kind glyph or first
    /// letter of the title). Drawn over the gradient so the fallback
    /// still feels like cover art, not abstract noise.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Derive the two endpoint colours from the seed. Hue picks are
    /// 140° apart on the HSL wheel so the pair always contrasts; the
    /// rotation seeds off `seed & 0xff`.
    fn endpoints(&self) -> (Color, Color) {
        let hue1 = (self.seed & 0xff) as f32 / 255.0 * 360.0;
        let hue2 = (hue1 + 140.0) % 360.0;
        (hsl_to_rgb(hue1, 0.55, 0.45), hsl_to_rgb(hue2, 0.60, 0.30))
    }
}

impl Widget for GradientArt {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (a, b) = self.endpoints();
        let width = area.width.max(1);
        let height = area.height.max(1);
        // Use `▀` (upper half block) for every cell: foreground = top
        // sub-pixel, background = bottom sub-pixel. That doubles
        // vertical resolution so the gradient bands look smooth even
        // on tiny rects.
        for cy in 0..height {
            for cx in 0..width {
                // Two sub-pixels per cell (top + bottom).
                let top_t = sample_t(cx, cy * 2, width, height * 2, self.seed);
                let bot_t = sample_t(cx, cy * 2 + 1, width, height * 2, self.seed);
                let fg = blend(a, b, top_t);
                let bg = blend(a, b, bot_t);
                if let Some(cell) = buf.cell_mut((area.x + cx, area.y + cy)) {
                    cell.set_symbol("▀")
                        .set_style(Style::default().fg(fg).bg(bg));
                }
            }
        }

        // Optional caption: stamped over the centre row(s) of the
        // gradient in BG-tone bold so it reads on top.
        if let Some(label) = &self.label {
            let label_chars: Vec<char> = label.chars().collect();
            if label_chars.is_empty() {
                return;
            }
            let label_str: String = label_chars.iter().collect();
            let cx_start = area.x + (width.saturating_sub(label_chars.len() as u16) / 2);
            let cy_mid = area.y + height / 2;
            for (i, ch) in label_chars.iter().enumerate() {
                let x = cx_start + i as u16;
                if x >= area.x + width {
                    break;
                }
                if let Some(cell) = buf.cell_mut((x, cy_mid)) {
                    // Stomp on top of the gradient with a high-
                    // contrast pure-foreground caption.
                    let existing_bg = cell.bg;
                    cell.set_symbol(&ch.to_string()).set_style(
                        Style::default()
                            .fg(Color::Rgb(245, 248, 250))
                            .bg(existing_bg)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    );
                }
            }
            let _ = label_str;
        }
    }
}

/// Sample value in [0, 1] for cell (x, y) in a width×height rect.
/// Combines a diagonal gradient with a small seed-based phase so two
/// different seeds produce visibly different patterns.
fn sample_t(x: u16, y: u16, width: u16, height: u16, seed: u64) -> f32 {
    let total = (width as f32 + height as f32).max(1.0);
    let base = (x as f32 + y as f32) / total;
    // Phase is in [-0.25, 0.25] so the band offsets stay subtle.
    let phase = ((seed >> 8) & 0xff) as f32 / 255.0 * 0.5 - 0.25;
    (base + phase).clamp(0.0, 1.0)
}

fn blend(a: Color, b: Color, t: f32) -> Color {
    let (ar, ag, ab) = rgb(a);
    let (br, bg, bb) = rgb(b);
    let lerp = |x: u8, y: u8| -> u8 {
        let v = x as f32 + (y as f32 - x as f32) * t;
        v.clamp(0.0, 255.0) as u8
    };
    Color::Rgb(lerp(ar, br), lerp(ag, bg), lerp(ab, bb))
}

fn rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    }
}

/// Compact HSL → RGB. h in [0, 360), s/l in [0, 1].
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> Color {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h_ = h / 60.0;
    let x = c * (1.0 - (h_ % 2.0 - 1.0).abs());
    let (r1, g1, b1) = if (0.0..1.0).contains(&h_) {
        (c, x, 0.0)
    } else if (1.0..2.0).contains(&h_) {
        (x, c, 0.0)
    } else if (2.0..3.0).contains(&h_) {
        (0.0, c, x)
    } else if (3.0..4.0).contains(&h_) {
        (0.0, x, c)
    } else if (4.0..5.0).contains(&h_) {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    let m = l - c / 2.0;
    Color::Rgb(
        ((r1 + m) * 255.0).clamp(0.0, 255.0) as u8,
        ((g1 + m) * 255.0).clamp(0.0, 255.0) as u8,
        ((b1 + m) * 255.0).clamp(0.0, 255.0) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn count_unique_bg(buffer: &Buffer) -> usize {
        let area = buffer.area();
        let mut seen = std::collections::HashSet::new();
        for y in 0..area.height {
            for x in 0..area.width {
                seen.insert(format!("{:?}", buffer[(x, y)].bg));
            }
        }
        seen.len()
    }

    #[test]
    fn gradient_fills_rect_with_multiple_colours_so_it_never_looks_broken() {
        let backend = TestBackend::new(18, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                let area = frame.area();
                let art = GradientArt::new("spotify:track:doves-in-the-wind").with_label("D");
                frame.render_widget(art, area);
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        assert!(
            count_unique_bg(buffer) >= 4,
            "gradient should produce ≥4 distinct background colours, got {}",
            count_unique_bg(buffer)
        );
    }

    #[test]
    fn same_seed_produces_identical_gradient_so_a_track_looks_the_same_each_run() {
        let mut t1 = Terminal::new(TestBackend::new(12, 4)).expect("t1");
        let mut t2 = Terminal::new(TestBackend::new(12, 4)).expect("t2");
        for term in [&mut t1, &mut t2] {
            term.draw(|frame| {
                frame.render_widget(GradientArt::new("track-A"), frame.area());
            })
            .expect("draw");
        }
        let area = t1.backend().buffer().area();
        for y in 0..area.height {
            for x in 0..area.width {
                let a = &t1.backend().buffer()[(x, y)];
                let b = &t2.backend().buffer()[(x, y)];
                assert_eq!(a.symbol(), b.symbol(), "symbol differs at {x},{y}");
                assert_eq!(format!("{:?}", a.fg), format!("{:?}", b.fg));
                assert_eq!(format!("{:?}", a.bg), format!("{:?}", b.bg));
            }
        }
    }

    #[test]
    fn different_seeds_produce_visibly_different_gradients() {
        let mut t_a = Terminal::new(TestBackend::new(12, 4)).expect("ta");
        let mut t_b = Terminal::new(TestBackend::new(12, 4)).expect("tb");
        t_a.draw(|f| f.render_widget(GradientArt::new("track-A"), f.area()))
            .expect("draw");
        t_b.draw(|f| f.render_widget(GradientArt::new("track-Z"), f.area()))
            .expect("draw");
        let mut diffs = 0;
        let area = t_a.backend().buffer().area();
        for y in 0..area.height {
            for x in 0..area.width {
                let a = &t_a.backend().buffer()[(x, y)];
                let b = &t_b.backend().buffer()[(x, y)];
                if a.fg != b.fg || a.bg != b.bg {
                    diffs += 1;
                }
            }
        }
        assert!(
            diffs >= (area.width as usize * area.height as usize) / 4,
            "different track ids should produce visibly different gradients (got {diffs} diffs)"
        );
    }

    #[test]
    fn snapshot_05_art_fallback() {
        let mut t = Terminal::new(TestBackend::new(18, 6)).expect("t");
        t.draw(|f| {
            f.render_widget(
                GradientArt::new("spotify:track:doves-in-the-wind").with_label("D"),
                f.area(),
            );
        })
        .expect("draw");
        let buf = t.backend().buffer();
        let area = buf.area();
        let frame: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        println!(
            "\n--- 05-art-fallback (track 'doves-in-the-wind', 18x6) ---\n{frame}\n--- end ---\n"
        );

        // Also render a different track so the user can see the
        // gradients differ.
        let mut t2 = Terminal::new(TestBackend::new(18, 6)).expect("t2");
        t2.draw(|f| {
            f.render_widget(
                GradientArt::new("spotify:track:never-too-much").with_label("N"),
                f.area(),
            );
        })
        .expect("draw");
        let buf2 = t2.backend().buffer();
        let area2 = buf2.area();
        let frame2: String = (0..area2.height)
            .map(|y| {
                (0..area2.width)
                    .map(|x| buf2[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        println!(
            "\n--- 05-art-fallback (track 'never-too-much', 18x6) ---\n{frame2}\n--- end ---\n"
        );
    }
}
