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
        StateRole::Active => (BG, GREEN),
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
        ButtonRole::Affirm => (BG, GREEN),
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

/// Focused card: same shape, GREEN border + GREEN title chip. Used for
/// the focused group in search, the focused panel in modals.
pub fn focused_card_block(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(GREEN).add_modifier(Modifier::BOLD))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(BG)
                .bg(GREEN)
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

    #[test]
    fn chips_and_cards_render_recognisably_at_realistic_width() {
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
