//! Terminal-safe glyph and text primitives shared by TUI renderers.

use ratatui::text::Line;

const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn spinner_frame(tick: u128) -> &'static str {
    SPINNER_FRAMES[(tick % SPINNER_FRAMES.len() as u128) as usize]
}

pub fn volume_bar(percent: u8, width: usize) -> String {
    let filled = (usize::from(percent) * width).div_ceil(100).min(width);
    "█".repeat(filled) + &"░".repeat(width - filled)
}

/// Emoji are enabled for normal modern terminals. Opt out when `NO_COLOR`
/// requests a plain presentation, or when `TERM` is absent/known to expose a
/// limited fixed-width console where emoji cell width cannot be trusted.
pub fn emoji_glyphs_enabled() -> bool {
    emoji_glyphs_enabled_for(
        std::env::var_os("NO_COLOR").is_some(),
        std::env::var("TERM").ok().as_deref(),
    )
}

fn emoji_glyphs_enabled_for(no_color: bool, term: Option<&str>) -> bool {
    if no_color {
        return false;
    }
    let Some(term) = term.filter(|term| !term.is_empty()) else {
        return false;
    };
    !matches!(term.to_ascii_lowercase().as_str(), "dumb" | "linux")
}

#[derive(Clone, Copy)]
pub enum SpeakerLevel {
    Muted,
    Low,
    Medium,
    High,
}

pub fn speaker_glyph(level: SpeakerLevel) -> &'static str {
    speaker_glyph_for(level, emoji_glyphs_enabled())
}

fn speaker_glyph_for(level: SpeakerLevel, emoji: bool) -> &'static str {
    match (level, emoji) {
        (SpeakerLevel::Muted, true) => "🔇",
        (SpeakerLevel::Low, true) => "🔈",
        (SpeakerLevel::Medium, true) => "🔉",
        (SpeakerLevel::High, true) => "🔊",
        (SpeakerLevel::Muted, false) => "x",
        (SpeakerLevel::Low, false) => "-",
        (SpeakerLevel::Medium, false) => "=",
        (SpeakerLevel::High, false) => "#",
    }
}

pub fn speaker_glyph_width() -> u16 {
    Line::from(speaker_glyph(SpeakerLevel::High)).width() as u16
}

pub fn device_kind_glyph(kind: &str) -> &'static str {
    device_kind_glyph_for(kind, emoji_glyphs_enabled())
}

fn device_kind_glyph_for(kind: &str, emoji: bool) -> &'static str {
    let kind = kind.to_ascii_lowercase();
    let glyphs = if kind.contains("smartphone") || kind.contains("phone") || kind.contains("tablet")
    {
        ("📱", "P")
    } else if kind.contains("computer") || kind.contains("laptop") {
        ("🖥", "C")
    } else if kind.contains("tv") {
        ("📺", "T")
    } else if kind.contains("speaker") {
        ("🔊", "S")
    } else if kind.contains("car") {
        ("🚗", "A")
    } else if kind.contains("game") || kind.contains("console") {
        ("🎮", "G")
    } else if kind.contains("cast") {
        ("📡", "W")
    } else {
        ("🎧", "H")
    };
    if emoji {
        glyphs.0
    } else {
        glyphs.1
    }
}

#[derive(Clone, Copy)]
pub enum BannerGlyph {
    Lock,
    Key,
    Timer,
    Info,
    Restart,
    Download,
}

pub fn banner_glyph(glyph: BannerGlyph) -> &'static str {
    banner_glyph_for(glyph, emoji_glyphs_enabled())
}

fn banner_glyph_for(glyph: BannerGlyph, emoji: bool) -> &'static str {
    match (glyph, emoji) {
        (BannerGlyph::Lock, true) => "🔒",
        (BannerGlyph::Key, true) => "🔑",
        (BannerGlyph::Timer, true) => "⏱",
        (BannerGlyph::Info, true) => "ⓘ",
        (BannerGlyph::Restart, true) => "⟳",
        (BannerGlyph::Download, true) => "⤓",
        (BannerGlyph::Lock, false) => "!",
        (BannerGlyph::Key, false) => "K",
        (BannerGlyph::Timer, false) => "T",
        (BannerGlyph::Info, false) => "i",
        (BannerGlyph::Restart, false) => "R",
        (BannerGlyph::Download, false) => "D",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_spinner_and_volume_bar_preserve_existing_output() {
        assert_eq!(spinner_frame(0), "⠋");
        assert_eq!(spinner_frame(9), "⠏");
        assert_eq!(spinner_frame(10), "⠋");
        assert_eq!(volume_bar(0, 16), "░░░░░░░░░░░░░░░░");
        assert_eq!(volume_bar(1, 16), "█░░░░░░░░░░░░░░░");
        assert_eq!(volume_bar(50, 16), "████████░░░░░░░░");
        assert_eq!(volume_bar(100, 16), "████████████████");
    }

    #[test]
    fn fallback_glyphs_are_single_cell() {
        let glyphs = [
            speaker_glyph_for(SpeakerLevel::Muted, false),
            speaker_glyph_for(SpeakerLevel::Low, false),
            speaker_glyph_for(SpeakerLevel::Medium, false),
            speaker_glyph_for(SpeakerLevel::High, false),
            device_kind_glyph_for("smartphone", false),
            device_kind_glyph_for("computer", false),
            device_kind_glyph_for("tv", false),
            device_kind_glyph_for("speaker", false),
            device_kind_glyph_for("car", false),
            device_kind_glyph_for("game console", false),
            device_kind_glyph_for("cast", false),
            device_kind_glyph_for("headphones", false),
            banner_glyph_for(BannerGlyph::Lock, false),
            banner_glyph_for(BannerGlyph::Key, false),
            banner_glyph_for(BannerGlyph::Timer, false),
            banner_glyph_for(BannerGlyph::Info, false),
            banner_glyph_for(BannerGlyph::Restart, false),
            banner_glyph_for(BannerGlyph::Download, false),
        ];
        assert!(glyphs.iter().all(|glyph| Line::from(*glyph).width() == 1));
    }

    #[test]
    fn emoji_capability_rule_is_conservative_for_limited_terminals() {
        assert!(!emoji_glyphs_enabled_for(true, Some("xterm-ghostty")));
        assert!(!emoji_glyphs_enabled_for(false, None));
        assert!(!emoji_glyphs_enabled_for(false, Some("")));
        assert!(!emoji_glyphs_enabled_for(false, Some("dumb")));
        assert!(!emoji_glyphs_enabled_for(false, Some("linux")));
        assert!(emoji_glyphs_enabled_for(false, Some("xterm-ghostty")));
        assert!(emoji_glyphs_enabled_for(false, Some("xterm-kitty")));
    }
}
