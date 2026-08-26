//! Visual primitives shared across screens.
//!
//! Every chip / card / section header in the TUI flows through one of
//! the helpers here so the look stays coherent. Adding a colour role
//! or chip style means touching ONE place, not every renderer.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders};

// ---------------------------------------------------------------------
// Semantic tokens
//
// Screens consume roles from this module instead of defining colours.
// Album-derived accent roles are exposed separately through the runtime
// accessors below.
// ---------------------------------------------------------------------

pub mod tokens {
    use ratatui::style::Color;

    /// The colours the TUI ships with. Every role resolves here unless a
    /// user theme overrides it, so this stays the definition of "how
    /// spotuify looks".
    pub(super) mod builtin {
        use ratatui::style::Color;

        pub const BG: Color = Color::Rgb(8, 10, 12);
        pub const SURFACE: Color = Color::Rgb(22, 27, 30);
        pub const TEXT: Color = Color::Rgb(230, 238, 242);
        pub const TEXT_MUTED: Color = Color::Rgb(130, 140, 145);
        pub const BORDER: Color = Color::Rgb(25, 31, 35);
        pub const BORDER_STRONG: Color = Color::Rgb(45, 55, 60);
        pub const ACCENT: Color = Color::Rgb(120, 210, 240);
        pub const SUCCESS: Color = Color::Rgb(30, 215, 96);
        pub const SUCCESS_SOFT: Color = Color::Rgb(50, 130, 75);
        pub const WARN: Color = Color::Rgb(245, 185, 65);
        pub const DANGER: Color = Color::Rgb(245, 88, 88);
        pub const PROGRESS_FILLED: Color = SUCCESS;
        pub const PROGRESS_UNFILLED: Color = Color::Rgb(38, 45, 49);
        pub const SELECTION: Color = Color::Rgb(115, 230, 155);
        pub const CHIP_BG: Color = Color::Rgb(60, 72, 78);
        pub const CHIP_FG: Color = Color::Rgb(240, 248, 252);
    }

    macro_rules! token {
        ($name:ident, $field:ident) => {
            pub fn $name() -> Color {
                super::active_theme().$field
            }
        };
    }

    token!(bg, bg);
    token!(surface, surface);
    token!(text, text);
    token!(text_muted, text_muted);
    token!(border, border);
    token!(border_strong, border_strong);
    token!(accent, accent);
    token!(success, success);
    token!(success_soft, success_soft);
    token!(warn, warn);
    token!(danger, danger);
    token!(progress_filled, progress_filled);
    token!(progress_unfilled, progress_unfilled);
    token!(selection, selection);
    token!(chip_bg, chip_bg);
    token!(chip_fg, chip_fg);

    // Categorical media-kind hues. These are a legend, not decoration:
    // they colour the one-glyph type indicator so a mixed list (search,
    // library) is scannable by type at a glance. Kept fixed (not
    // album-adaptive, not themeable) on purpose so the category, not the
    // current cover or theme, determines the colour; the glyph is small
    // enough that fixed hues do not fight the accent used for larger
    // surfaces.
    pub fn kind_podcast() -> Color {
        Color::Rgb(180, 128, 255)
    }

    pub fn kind_album() -> Color {
        Color::Rgb(91, 179, 255)
    }

    pub fn kind_artist() -> Color {
        Color::Rgb(255, 177, 66)
    }
}

// `accent` and `progress_filled` are deliberately not re-exported: the
// album-adaptive accessors of the same name below shadow them for
// screens, and only this module wants the un-adapted role.
pub use tokens::{
    bg, border, border_strong, chip_bg, chip_fg, danger, kind_album, kind_artist, kind_podcast,
    progress_unfilled, selection, success, success_soft, surface, text, text_muted, warn,
};

/// Every colour role resolved to a concrete terminal colour. Computed once
/// when the theme changes rather than per token read, so a frame that reads
/// a token 300 times does no hex parsing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ThemePalette {
    pub bg: Color,
    pub surface: Color,
    pub text: Color,
    pub text_muted: Color,
    pub border: Color,
    pub border_strong: Color,
    pub accent: Color,
    pub success: Color,
    pub success_soft: Color,
    pub warn: Color,
    pub danger: Color,
    pub progress_filled: Color,
    pub progress_unfilled: Color,
    pub selection: Color,
    pub chip_bg: Color,
    pub chip_fg: Color,
}

impl ThemePalette {
    /// The colours spotuify ships with — what `terminal-default` means.
    pub const BUILTIN: Self = Self {
        bg: tokens::builtin::BG,
        surface: tokens::builtin::SURFACE,
        text: tokens::builtin::TEXT,
        text_muted: tokens::builtin::TEXT_MUTED,
        border: tokens::builtin::BORDER,
        border_strong: tokens::builtin::BORDER_STRONG,
        accent: tokens::builtin::ACCENT,
        success: tokens::builtin::SUCCESS,
        success_soft: tokens::builtin::SUCCESS_SOFT,
        warn: tokens::builtin::WARN,
        danger: tokens::builtin::DANGER,
        progress_filled: tokens::builtin::PROGRESS_FILLED,
        progress_unfilled: tokens::builtin::PROGRESS_UNFILLED,
        selection: tokens::builtin::SELECTION,
        chip_bg: tokens::builtin::CHIP_BG,
        chip_fg: tokens::builtin::CHIP_FG,
    };

    /// Map a theme's seven roles onto all sixteen. A theme names the
    /// foreground colours; the surfaces between background and text are
    /// derived by blending, at the same ratios the built-in palette uses,
    /// so a theme cannot produce an unreadable chrome by accident.
    ///
    /// Returns `None` for the `terminal-default` sentinel — nothing to map.
    pub fn from_spec(spec: &spotuify_core::ThemeSpec) -> Option<Self> {
        let rgb = |value: Option<&str>| {
            value
                .and_then(spotuify_core::hex_rgb)
                .map(|[r, g, b]| (r, g, b))
        };
        let accent = rgb(spec.accent.as_deref())?;
        let bright_fg = rgb(spec.bright_fg.as_deref())?;
        let fg = rgb(spec.fg.as_deref())?;
        let green = rgb(spec.green.as_deref())?;
        let yellow = rgb(spec.yellow.as_deref())?;
        let red = rgb(spec.red.as_deref())?;
        let bg = rgb(spec.bg.as_deref());

        // Without a `bg` the terminal keeps its own background, so painted
        // surfaces stay transparent — but the derived borders still need a
        // base to blend from, and black is the safe assumption for a
        // terminal dark enough to run these themes.
        let base = bg.unwrap_or((0, 0, 0));
        let derived = |t: f32| color(blend_rgb(base, fg, t));
        let transparent_or = |color: Color| if bg.is_some() { color } else { Color::Reset };

        Some(Self {
            bg: transparent_or(color(base)),
            surface: transparent_or(derived(0.13)),
            text: color(bright_fg),
            text_muted: color(fg),
            border: derived(0.16),
            border_strong: derived(0.34),
            accent: color(accent),
            success: color(green),
            success_soft: color(blend_rgb(green, base, 0.4)),
            warn: color(yellow),
            danger: color(red),
            progress_filled: color(green),
            progress_unfilled: derived(0.26),
            selection: color(accent),
            chip_bg: derived(0.47),
            chip_fg: color(bright_fg),
        })
    }
}

fn color((r, g, b): (u8, u8, u8)) -> Color {
    Color::Rgb(r, g, b)
}

thread_local! {
    /// Colours for the frame currently being drawn. `ui::render` sets this
    /// from `App::theme` at the top of every frame, next to the album
    /// palette; every `tokens::*` accessor reads it. Rendering is
    /// single-threaded, so a thread-local beats threading a palette through
    /// several hundred call sites.
    static ACTIVE_THEME: std::cell::Cell<ThemePalette> =
        const { std::cell::Cell::new(ThemePalette::BUILTIN) };
}

/// Adopt a resolved theme for subsequent frames. The sentinel (and any
/// theme with a colour we cannot parse) restores the built-in palette.
pub fn set_active_theme(spec: &spotuify_core::ThemeSpec) {
    set_active_theme_palette(ThemePalette::from_spec(spec).unwrap_or(ThemePalette::BUILTIN));
}

pub fn set_active_theme_palette(palette: ThemePalette) {
    ACTIVE_THEME.with(|cell| cell.set(palette));
}

pub fn active_theme() -> ThemePalette {
    ACTIVE_THEME.with(std::cell::Cell::get)
}

/// The one thing cover art contributes: its dominant colour.
///
/// Every role derived from it — the accent, the panel tint, the rail, the
/// soft selection fill — is blended against the *active theme* at read
/// time rather than stored here. Storing the blends would freeze whichever
/// theme was live when the artwork decoded, and a later theme change would
/// leave the now-playing panel tinted for the old one until the track
/// changed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UiPalette {
    /// `None` when no artwork is loaded, so every accent role falls back to
    /// the theme.
    pub dominant: Option<(u8, u8, u8)>,
}

impl UiPalette {
    pub const DEFAULT: Self = Self { dominant: None };

    pub fn from_cover(image: &image::DynamicImage) -> Option<Self> {
        Some(Self {
            dominant: Some(dominant_terminal_safe_rgb(image)?),
        })
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

fn cover_dominant() -> Option<(u8, u8, u8)> {
    ACTIVE_PALETTE.with(std::cell::Cell::get).dominant
}

/// Interface accent: the cover's when art is loaded, the theme's otherwise.
pub fn accent() -> Color {
    cover_dominant().map_or_else(tokens::accent, color)
}

/// The cover's accent, or `None` when no artwork is loaded.
///
/// For surfaces that have their own palette worth keeping when there is no
/// cover to tint them. The visualizer is the one: its bars carry three
/// intensity tiers, and flattening them to `accent()` — which falls back to
/// the theme's single accent — turns the whole panel one colour.
pub fn cover_accent() -> Option<Color> {
    cover_dominant().map(color)
}

/// Readable foreground for text drawn on an `accent()` background.
pub fn accent_foreground() -> Color {
    readable_on(rgb_components(accent()))
}

/// Muted accent for selection backgrounds.
pub fn soft_accent() -> Color {
    match cover_dominant() {
        Some(rgb) => color(blend_rgb(rgb_components(tokens::success_soft()), rgb, 0.48)),
        None => tokens::success_soft(),
    }
}

/// Seek fill: the cover's accent when art is loaded, the theme's otherwise.
pub fn progress_filled() -> Color {
    cover_dominant().map_or_else(tokens::progress_filled, color)
}

/// Rail marking the now-playing row and the player border.
pub fn now_playing_rail() -> Color {
    match cover_dominant() {
        Some(rgb) => color(blend_rgb(rgb, (245, 248, 250), 0.30)),
        None => tokens::selection(),
    }
}

/// Background of the now-playing panel, tinted by the cover when there is one.
pub fn panel_background() -> Color {
    match cover_dominant() {
        Some(rgb) => color(blend_rgb(rgb_components(tokens::surface()), rgb, 0.18)),
        None => tokens::surface(),
    }
}

/// Dark ink for text drawn on a light chip.
///
/// `bg()` used to fill this role, which breaks for a theme that sets no
/// background: there `bg()` is `Color::Reset`, and Reset as a *foreground*
/// is the terminal's own text colour — light on a dark terminal, which is
/// exactly unreadable on a yellow warning chip. This never returns Reset.
pub fn contrast_fg() -> Color {
    match tokens::bg() {
        Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
        // No themed background to borrow, so pick an ink that is dark in
        // any terminal rather than deferring to the terminal's foreground.
        _ => Color::Rgb(12, 12, 12),
    }
}

/// Channels of a semantic token. `Color::Reset` (a theme with no `bg`)
/// has none to read, so callers blending against it get black, the
/// terminal background these themes assume.
fn rgb_components(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    }
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
        contrast_fg()
    } else {
        chip_fg()
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
        Style::default().fg(chip_fg()).add_modifier(Modifier::BOLD),
    )
}

/// Section header chip: ` Title ` with the same inverted treatment as
/// the key chip but tinted with the accent colour so it reads as a
/// label, not a button.
pub fn section_chip(label: &str) -> Span<'static> {
    Span::styled(
        format!(" {label} "),
        Style::default()
            .fg(accent_foreground())
            .bg(accent())
            .add_modifier(Modifier::BOLD),
    )
}

/// State chip: short label coloured by semantic role. Used for device
/// state (`playing` / `idle` / `restricted`), log severity, etc.
pub fn state_chip(label: &str, role: StateRole) -> Span<'static> {
    let (fg, bg) = match role {
        StateRole::Active => (accent_foreground(), accent()),
        StateRole::Warn => (contrast_fg(), warn()),
        StateRole::Error => (chip_fg(), danger()),
        StateRole::Idle => (contrast_fg(), text_muted()),
        StateRole::Accent => (accent_foreground(), accent()),
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

/// Button chip: adaptive accent for affirmative actions and danger for
/// destructive ones.
pub fn button_chip(label: &str, role: ButtonRole) -> Span<'static> {
    let (fg, bg) = match role {
        ButtonRole::Affirm => (accent_foreground(), accent()),
        ButtonRole::Cancel => (chip_fg(), chip_bg()),
        ButtonRole::Danger => (chip_fg(), danger()),
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
        .border_style(Style::default().fg(border_strong()))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(accent_foreground())
                .bg(accent())
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(surface()))
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
        .style(Style::default().bg(surface()))
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
        set_active_theme(&spotuify_core::ThemeSpec::terminal_default());
        set_active_palette(UiPalette::DEFAULT);
        let (plain_accent, plain_panel, plain_rail) =
            (accent(), panel_background(), now_playing_rail());

        set_active_palette(UiPalette::from_cover(&solid_image([0, 0, 80])).expect("palette"));
        assert_ne!(accent(), plain_accent);
        assert_ne!(panel_background(), plain_panel);
        assert_ne!(now_playing_rail(), plain_rail);
        assert_eq!(accent_foreground(), chip_fg());
        set_active_palette(UiPalette::DEFAULT);
    }

    #[test]
    fn monochrome_light_covers_get_dark_readable_foreground() {
        set_active_theme(&spotuify_core::ThemeSpec::terminal_default());
        set_active_palette(UiPalette::from_cover(&solid_image([235, 235, 235])).expect("palette"));
        assert_eq!(accent_foreground(), contrast_fg());
        set_active_palette(UiPalette::DEFAULT);
    }

    fn theme_with(bg: Option<&str>) -> spotuify_core::ThemeSpec {
        spotuify_core::ThemeSpec {
            name: "probe".to_string(),
            source: spotuify_core::ThemeSource::Builtin,
            bg: bg.map(ToString::to_string),
            accent: Some("#00FF00".to_string()),
            bright_fg: Some("#FFFFFF".to_string()),
            fg: Some("#969696".to_string()),
            green: Some("#29CE10".to_string()),
            yellow: Some("#D6B521".to_string()),
            red: Some("#EF3110".to_string()),
        }
    }

    /// Chip text must never be `Color::Reset`. Reset as a foreground is the
    /// terminal's own text colour, which on a dark terminal is light — and
    /// light-on-yellow is the warning chip nobody can read.
    #[test]
    fn chip_ink_stays_dark_for_a_theme_with_no_background() {
        set_active_palette(UiPalette::DEFAULT);
        set_active_theme(&theme_with(None));
        assert_eq!(bg(), Color::Reset, "no `bg` means a transparent surface");
        assert_ne!(contrast_fg(), Color::Reset, "but ink is never transparent");

        for role in [StateRole::Warn, StateRole::Idle] {
            let chip = state_chip("x", role);
            assert_ne!(
                chip.style.fg,
                Some(Color::Reset),
                "{role:?} chip text must not fall back to the terminal foreground"
            );
        }

        // With a background the ink borrows it, which is what the built-in
        // palette always did.
        set_active_theme(&theme_with(Some("#101820")));
        assert_eq!(contrast_fg(), Color::Rgb(0x10, 0x18, 0x20));
        set_active_theme(&spotuify_core::ThemeSpec::terminal_default());
    }

    /// The blends live at read time, so switching theme with artwork loaded
    /// repaints the panel instead of keeping the old theme's tint until the
    /// track changes.
    #[test]
    fn cover_derived_roles_follow_a_theme_change_without_new_artwork() {
        set_active_palette(UiPalette::from_cover(&solid_image([0, 0, 80])).expect("palette"));

        set_active_theme(&spotuify_core::ThemeSpec::terminal_default());
        let (before_panel, before_soft) = (panel_background(), soft_accent());

        set_active_theme(&theme_with(Some("#402020")));
        assert_ne!(
            panel_background(),
            before_panel,
            "the panel tint blends the cover against the theme's surface"
        );
        assert_ne!(soft_accent(), before_soft);

        set_active_theme(&spotuify_core::ThemeSpec::terminal_default());
        set_active_palette(UiPalette::DEFAULT);
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
                    Paragraph::new(chips).style(Style::default().bg(bg())),
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
                    Paragraph::new(sections).style(Style::default().bg(bg())),
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
                    Paragraph::new(states).style(Style::default().bg(bg())),
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
                    Paragraph::new(buttons).style(Style::default().bg(bg())),
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
