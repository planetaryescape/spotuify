//! Visual primitives shared across screens.
//!
//! Every chip / card / section header in the TUI flows through one of
//! the helpers here so the look stays coherent. Adding a colour role
//! or chip style means touching ONE place, not every renderer.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders};

// ---------------------------------------------------------------------
// Palette
//
// Roles, not raw colours. When a screen needs "the panel background",
// use `PANEL`, not Color::Rgb(...). The names here are aligned with
// the Spotify reference (green-on-dark) plus three new roles the
// visual revamp adds:
//   * ACCENT       — secondary accent for non-Spotify-branded affordances
//   * DIM_BORDER   — subtle borders (was raw Rgb in every renderer)
//   * CHIP_BG/_FG  — inverted chip background and foreground (one pair
//                    reused for key chips, section chips, button chips)
// ---------------------------------------------------------------------

pub const BG: Color = Color::Rgb(15, 18, 20);
pub const PANEL: Color = Color::Rgb(22, 27, 30);
pub const TEXT: Color = Color::Rgb(230, 238, 242);
pub const MUTED: Color = Color::Rgb(130, 140, 145);
pub const GREEN: Color = Color::Rgb(30, 215, 96);
/// Softer green used for list-row selection backgrounds. The bright
/// `GREEN` was reused everywhere — including the playback seeker
/// gauge — which made the selection chip read as another playback
/// indicator. This shade keeps the spotify family but is muted
/// enough that the seeker remains the only "live" green on screen.
pub const GREEN_SOFT: Color = Color::Rgb(50, 130, 75);
/// Subtle accent stripe rendered to the left of the now-playing row
/// in queue / playlist / search lists so the user can tell at a
/// glance which item is the one Spotify is currently emitting,
/// without leaning on the same green as the selection background.
pub const NOW_PLAYING_RAIL: Color = Color::Rgb(115, 230, 155);
pub const WARN: Color = Color::Rgb(245, 185, 65);
pub const RED: Color = Color::Rgb(245, 88, 88);
pub const ACCENT: Color = Color::Rgb(120, 210, 240);
pub const DIM_BORDER: Color = Color::Rgb(45, 55, 60);
pub const CHIP_BG: Color = Color::Rgb(60, 72, 78);
pub const CHIP_FG: Color = Color::Rgb(240, 248, 252);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UiPalette {
    pub accent: Color,
    /// Primary "branded" accent: Spotify green by default, the cover's
    /// dominant colour once album art is loaded. Everything that used
    /// to hardcode `GREEN` (tab chips, focused borders, selection
    /// marks, transport chips, gauges, status text) reads this so the
    /// whole frame follows the album, not just the now-playing bar.
    pub brand: Color,
    pub soft_accent: Color,
    pub background: Color,
    pub foreground: Color,
    pub now_playing_rail: Color,
}

impl UiPalette {
    pub const DEFAULT: Self = Self {
        accent: ACCENT,
        brand: GREEN,
        soft_accent: GREEN_SOFT,
        background: PANEL,
        foreground: BG,
        now_playing_rail: NOW_PLAYING_RAIL,
    };

    pub fn from_cover(image: &image::DynamicImage) -> Option<Self> {
        let rgb = dominant_terminal_safe_rgb(image)?;
        let accent = Color::Rgb(rgb.0, rgb.1, rgb.2);
        let foreground = readable_on(rgb);
        let bg = blend_rgb((22, 27, 30), rgb, 0.18);
        let soft = blend_rgb((50, 130, 75), rgb, 0.48);
        let rail = blend_rgb(rgb, (245, 248, 250), 0.30);
        Some(Self {
            accent,
            brand: accent,
            soft_accent: Color::Rgb(soft.0, soft.1, soft.2),
            background: Color::Rgb(bg.0, bg.1, bg.2),
            foreground,
            now_playing_rail: Color::Rgb(rail.0, rail.1, rail.2),
        })
    }
}

impl Default for UiPalette {
    fn default() -> Self {
        Self::DEFAULT
    }
}

thread_local! {
    /// Palette for the frame currently being drawn. `ui::render` sets
    /// this from `App::palette` at the top of every frame; the chip /
    /// card helpers and every accent-coloured renderer read it through
    /// the accessors below so all accent surfaces follow the album art
    /// instead of staying Spotify-green. Rendering is single-threaded,
    /// so a thread-local avoids threading the palette through dozens of
    /// helper signatures.
    static ACTIVE_PALETTE: std::cell::Cell<UiPalette> =
        const { std::cell::Cell::new(UiPalette::DEFAULT) };
}

pub fn set_active_palette(palette: UiPalette) {
    ACTIVE_PALETTE.with(|cell| cell.set(palette));
}

/// Album-adaptive brand accent (falls back to the Spotify green
/// default). This is what the old hardcoded `GREEN` call sites read.
pub fn accent() -> Color {
    ACTIVE_PALETTE.with(|cell| cell.get().brand)
}

/// Readable foreground for text drawn on an `accent()` background.
pub fn accent_foreground() -> Color {
    ACTIVE_PALETTE.with(|cell| cell.get().foreground)
}

/// Muted accent for selection backgrounds (adaptive `GREEN_SOFT`).
pub fn soft_accent() -> Color {
    ACTIVE_PALETTE.with(|cell| cell.get().soft_accent)
}

fn dominant_terminal_safe_rgb(image: &image::DynamicImage) -> Option<(u8, u8, u8)> {
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    if width == 0 || height == 0 {
        return None;
    }
    let step_x = (width / 48).max(1);
    let step_y = (height / 48).max(1);
    let mut buckets = std::collections::BTreeMap::<(u8, u8, u8), (u32, u32, u32, u32)>::new();
    for y in (0..height).step_by(step_y as usize) {
        for x in (0..width).step_by(step_x as usize) {
            let [r, g, b, a] = rgba.get_pixel(x, y).0;
            if a < 180 {
                continue;
            }
            let key = (r >> 3, g >> 3, b >> 3);
            let entry = buckets.entry(key).or_insert((0, 0, 0, 0));
            entry.0 += u32::from(r);
            entry.1 += u32::from(g);
            entry.2 += u32::from(b);
            entry.3 += 1;
        }
    }
    buckets
        .values()
        .filter(|(_, _, _, count)| *count > 0)
        .map(|(r, g, b, count)| {
            let rgb = (
                (*r / *count) as u8,
                (*g / *count) as u8,
                (*b / *count) as u8,
            );
            let sat = saturation(rgb);
            let lum = relative_luminance(rgb);
            let lum_score = (1.0 - (lum - 0.48).abs()).max(0.15);
            let score = *count as f32 * (0.35 + sat) * lum_score;
            (score, rgb)
        })
        .max_by(|(a, _), (b, _)| a.total_cmp(b))
        .map(|(_, rgb)| normalize_accent(rgb))
}

fn normalize_accent(rgb: (u8, u8, u8)) -> (u8, u8, u8) {
    let lum = relative_luminance(rgb);
    let target = if lum < 0.28 {
        0.42
    } else if lum > 0.72 {
        0.58
    } else {
        lum
    };
    if (target - lum).abs() < f32::EPSILON {
        return rgb;
    }
    let t = if target > lum {
        ((target - lum) / (1.0 - lum)).clamp(0.0, 1.0)
    } else {
        (1.0 - target / lum.max(0.01)).clamp(0.0, 1.0)
    };
    if target > lum {
        blend_rgb(rgb, (255, 255, 255), t)
    } else {
        blend_rgb(rgb, (0, 0, 0), t)
    }
}

fn readable_on(rgb: (u8, u8, u8)) -> Color {
    if relative_luminance(rgb) > 0.45 {
        BG
    } else {
        CHIP_FG
    }
}

fn saturation((r, g, b): (u8, u8, u8)) -> f32 {
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    if max <= f32::EPSILON {
        0.0
    } else {
        (max - min) / max
    }
}

fn relative_luminance((r, g, b): (u8, u8, u8)) -> f32 {
    (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0
}

fn blend_rgb(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).clamp(0.0, 255.0) as u8;
    (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

// ---------------------------------------------------------------------
// Chips
// ---------------------------------------------------------------------

/// Shortcut chip: `[K]` — bracket-wrapped bold key. The bracket
/// approach reads as a button without painting the cell background,
/// so chips on the bottom row of the terminal don't look like a
/// solid bar touching the screen edge.
pub fn key_chip(key: &str) -> Span<'static> {
    Span::styled(
        format!("[{key}]"),
        Style::default().fg(CHIP_FG).add_modifier(Modifier::BOLD),
    )
}

/// Section header chip: ` Title ` with the same inverted treatment as
/// the key chip but tinted with the accent colour so it reads as a
/// label, not a button.
pub fn section_chip(label: &str) -> Span<'static> {
    Span::styled(
        format!(" {label} "),
        Style::default()
            .fg(BG)
            .bg(ACCENT)
            .add_modifier(Modifier::BOLD),
    )
}

/// State chip: short label coloured by semantic role. Used for device
/// state (`playing` / `idle` / `restricted`), log severity, etc.
pub fn state_chip(label: &str, role: StateRole) -> Span<'static> {
    let (fg, bg) = match role {
        StateRole::Active => (accent_foreground(), accent()),
        StateRole::Warn => (BG, WARN),
        StateRole::Error => (CHIP_FG, RED),
        StateRole::Idle => (BG, MUTED),
        StateRole::Accent => (BG, ACCENT),
    };
    Span::styled(
        format!(" {label} "),
        Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
    )
}

#[derive(Copy, Clone, Debug)]
pub enum StateRole {
    Active,
    Warn,
    Error,
    Idle,
    Accent,
}

/// Button chip: like a key chip but uses GREEN for affirmative
/// actions (Yes, Play, Save) and RED for destructive ones.
pub fn button_chip(label: &str, role: ButtonRole) -> Span<'static> {
    let (fg, bg) = match role {
        ButtonRole::Affirm => (accent_foreground(), accent()),
        ButtonRole::Cancel => (CHIP_FG, CHIP_BG),
        ButtonRole::Danger => (CHIP_FG, RED),
    };
    Span::styled(
        format!(" {label} "),
        Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
    )
}

#[derive(Copy, Clone, Debug)]
pub enum ButtonRole {
    Affirm,
    Cancel,
    Danger,
}

// ---------------------------------------------------------------------
// Cards / blocks
// ---------------------------------------------------------------------

/// Card block: a panel with a tinted title chip in the top-left and a
/// dim 1-px border. Replaces the ad-hoc `panel_block` pattern that
/// every screen used to spell out by hand.
pub fn card_block(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(DIM_BORDER))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(BG)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(PANEL))
}

/// Focused card: same shape, accent border + accent title chip. Used
/// for the focused group in search, the focused panel in modals.
pub fn focused_card_block(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(accent()).add_modifier(Modifier::BOLD))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(accent_foreground())
                .bg(accent())
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(PANEL))
}

// ---------------------------------------------------------------------
// Tests
//
// One representative frame per chip / card so the snapshot exists in
// the tree and so the build proves we can compose every helper into a
// real rendered surface.
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::text::Line;
    use ratatui::widgets::Paragraph;
    use ratatui::Terminal;

    fn dump(buffer: &ratatui::buffer::Buffer) -> String {
        let area = buffer.area();
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn solid_image(rgb: [u8; 3]) -> image::DynamicImage {
        let mut img = image::RgbaImage::new(4, 4);
        for pixel in img.pixels_mut() {
            *pixel = image::Rgba([rgb[0], rgb[1], rgb[2], 255]);
        }
        image::DynamicImage::ImageRgba8(img)
    }

    #[test]
    fn cover_palette_extracts_terminal_safe_roles_from_art() {
        let palette = UiPalette::from_cover(&solid_image([0, 0, 80])).expect("palette");
        assert_ne!(palette.accent, UiPalette::DEFAULT.accent);
        assert_ne!(palette.background, UiPalette::DEFAULT.background);
        assert_ne!(
            palette.now_playing_rail,
            UiPalette::DEFAULT.now_playing_rail
        );
        assert_eq!(palette.foreground, CHIP_FG);
    }

    #[test]
    fn monochrome_light_covers_get_dark_readable_foreground() {
        let palette = UiPalette::from_cover(&solid_image([235, 235, 235])).expect("palette");
        assert_eq!(palette.foreground, BG);
    }

    #[test]
    fn chips_and_cards_render_recognisably_at_realistic_width() {
        // Reset the thread-local palette: under threaded `cargo test` a
        // prior test that rendered a custom palette would leak into the
        // chip helpers here.
        set_active_palette(UiPalette::DEFAULT);
        // 80 cols × 12 rows so the snapshot fits a typical PR review
        // panel; the layout itself works at 60–200+.
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                let area = frame.area();
                // Top row: chips lined up like a hint bar would render.
                let chips = Line::from(vec![
                    key_chip("space"),
                    Span::raw(" play  "),
                    key_chip("n"),
                    Span::raw(" next  "),
                    key_chip("L"),
                    Span::raw(" lyrics  "),
                    key_chip("Q"),
                    Span::raw(" queue  "),
                    key_chip("?"),
                    Span::raw(" help"),
                ]);
                frame.render_widget(
                    Paragraph::new(chips).style(Style::default().bg(BG)),
                    Rect::new(0, 0, area.width, 1),
                );
                // Section chips on row 2.
                let sections = Line::from(vec![
                    section_chip("Songs"),
                    Span::raw("  "),
                    section_chip("Albums"),
                    Span::raw("  "),
                    section_chip("Artists"),
                ]);
                frame.render_widget(
                    Paragraph::new(sections).style(Style::default().bg(BG)),
                    Rect::new(0, 2, area.width, 1),
                );
                // State chips on row 3.
                let states = Line::from(vec![
                    state_chip("playing", StateRole::Active),
                    Span::raw("  "),
                    state_chip("idle", StateRole::Idle),
                    Span::raw("  "),
                    state_chip("403", StateRole::Error),
                    Span::raw("  "),
                    state_chip("warn", StateRole::Warn),
                    Span::raw("  "),
                    state_chip("accent", StateRole::Accent),
                ]);
                frame.render_widget(
                    Paragraph::new(states).style(Style::default().bg(BG)),
                    Rect::new(0, 4, area.width, 1),
                );
                // Button chips on row 4.
                let buttons = Line::from(vec![
                    button_chip("Yes", ButtonRole::Affirm),
                    Span::raw("  "),
                    button_chip("No", ButtonRole::Cancel),
                    Span::raw("  "),
                    button_chip("Delete", ButtonRole::Danger),
                ]);
                frame.render_widget(
                    Paragraph::new(buttons).style(Style::default().bg(BG)),
                    Rect::new(0, 6, area.width, 1),
                );
                // Cards on rows 8-11.
                frame.render_widget(card_block("Tracks (12)"), Rect::new(0, 8, 26, 4));
                frame.render_widget(focused_card_block("Artists (3)"), Rect::new(28, 8, 26, 4));
                frame.render_widget(card_block("Playlists (7)"), Rect::new(56, 8, 24, 4));
            })
            .expect("draw");

        let frame = dump(terminal.backend().buffer());
        // Print so `cargo test -- --nocapture` shows the rendered output
        // for human inspection. The assertion below is just an anchor
        // that catches "did anything render at all" — the human review
        // is the real verification.
        println!("\n--- 01-chips snapshot (80x12) ---\n{frame}\n--- end ---\n");
        assert!(
            frame.contains("space") && frame.contains("Songs") && frame.contains("Tracks"),
            "snapshot should include key chip, section chip, and card title"
        );
    }
}
