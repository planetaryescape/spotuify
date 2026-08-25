//! Built-in and user theme files.
//!
//! Themes use the cliamp format (MIT, <https://github.com/bjarneo/cliamp>):
//! a flat TOML file of `role = "#RRGGBB"` pairs. The nine built-ins below
//! are copied from that project; users drop more into
//! `<config_dir>/themes/*.toml`, and a user file shadows a built-in of the
//! same name.
//!
//! Only this crate touches the filesystem for themes. The daemon resolves
//! the active theme once and hands clients the finished [`ThemeSpec`], so
//! no client ever reads a theme file.

use std::path::PathBuf;

use serde::Deserialize;
use spotuify_core::{ThemeSource, ThemeSpec, TERMINAL_DEFAULT_THEME};

/// Themes shipped in the binary. `include_str!` rather than a runtime read
/// so a fresh install has a full palette list before any file exists.
const BUILTIN_THEMES: &[(&str, &str)] = &[
    ("catppuccin", include_str!("../themes/catppuccin.toml")),
    ("dracula", include_str!("../themes/dracula.toml")),
    ("everforest", include_str!("../themes/everforest.toml")),
    ("gruvbox", include_str!("../themes/gruvbox.toml")),
    ("kanagawa", include_str!("../themes/kanagawa.toml")),
    ("nord", include_str!("../themes/nord.toml")),
    ("rose-pine", include_str!("../themes/rose-pine.toml")),
    ("tokyo-night", include_str!("../themes/tokyo-night.toml")),
    ("winamp", include_str!("../themes/winamp.toml")),
];

#[derive(Debug, thiserror::Error)]
pub(crate) enum ThemeLoadError {
    #[error("theme `{theme}` is not valid TOML: {source}")]
    Syntax {
        theme: String,
        #[source]
        source: toml::de::Error,
    },
    #[error(transparent)]
    Invalid(#[from] spotuify_core::ThemeError),
}

/// Every theme spotuify can apply right now, plus the files it had to skip.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ThemeCatalog {
    /// `terminal-default` first, then everything else by name.
    pub themes: Vec<ThemeSpec>,
    /// User files that could not be read or parsed. Skipping is deliberate:
    /// one broken theme must not hide the other twenty, and the daemon logs
    /// these rather than failing to start.
    pub warnings: Vec<String>,
}

impl ThemeCatalog {
    pub fn get(&self, name: &str) -> Option<&ThemeSpec> {
        let wanted = spotuify_core::canonical_theme_name(name);
        self.themes.iter().find(|theme| theme.name == wanted)
    }

    /// Comma-separated names, for "unknown theme `x`; expected one of …".
    pub fn names(&self) -> String {
        self.themes
            .iter()
            .map(|theme| theme.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Where user themes live: `<config_dir>/themes`.
pub fn themes_dir() -> PathBuf {
    spotuify_protocol::paths::config_dir().join("themes")
}

/// Names a theme file may not take. `terminal-default` is the sentinel, and
/// `list` / `path` are `spotuify theme`'s own subcommands, so a theme with
/// either name could be listed but never applied. Refusing them at load
/// makes the collision visible instead of leaving the user to discover a
/// theme they cannot select.
const RESERVED_THEME_NAMES: &[&str] = &[TERMINAL_DEFAULT_THEME, "list", "path"];

/// Largest theme file worth reading. A theme is seven lines, roughly 200
/// bytes; the cap is what stops a stray multi-megabyte file (or a symlink
/// aimed at one) from being slurped into memory on the blocking pool.
const MAX_THEME_FILE_BYTES: u64 = 64 * 1024;

/// The colour keys a theme file may set. Unknown keys are ignored so a
/// theme written for a future spotuify (or another player) still loads.
#[derive(Debug, Default, Deserialize)]
struct ThemeFile {
    bg: Option<String>,
    accent: Option<String>,
    bright_fg: Option<String>,
    fg: Option<String>,
    green: Option<String>,
    yellow: Option<String>,
    red: Option<String>,
}

pub(crate) fn parse_theme(
    name: &str,
    source: ThemeSource,
    contents: &str,
) -> Result<ThemeSpec, ThemeLoadError> {
    let file: ThemeFile = toml::from_str(contents).map_err(|error| ThemeLoadError::Syntax {
        theme: name.to_string(),
        source: error,
    })?;
    let spec = ThemeSpec {
        name: spotuify_core::canonical_theme_name(name),
        source,
        bg: file.bg,
        accent: file.accent,
        bright_fg: file.bright_fg,
        fg: file.fg,
        green: file.green,
        yellow: file.yellow,
        red: file.red,
    };
    spec.validate()?;
    Ok(spec)
}

/// The themes compiled into the binary. A malformed shipped file is dropped
/// rather than taking the process down; `every_builtin_theme_parses_and_is_complete`
/// asserts the count, so one can never reach a release unnoticed.
pub(crate) fn builtin_themes() -> Vec<ThemeSpec> {
    BUILTIN_THEMES
        .iter()
        .filter_map(|(name, contents)| parse_theme(name, ThemeSource::Builtin, contents).ok())
        .collect()
}

/// Built-ins merged with `<config_dir>/themes/*.toml`, user files winning
/// on name collisions, sorted with the sentinel first.
pub fn load_themes() -> ThemeCatalog {
    load_themes_from(&themes_dir())
}

pub(crate) fn load_themes_from(dir: &std::path::Path) -> ThemeCatalog {
    let mut catalog = ThemeCatalog {
        themes: builtin_themes(),
        warnings: Vec::new(),
    };
    for (name, path) in user_theme_files(dir, &mut catalog.warnings) {
        match std::fs::read_to_string(&path)
            .map_err(|error| format!("{}: {error}", path.display()))
            .and_then(|contents| {
                parse_theme(&name, ThemeSource::User, &contents)
                    .map_err(|error| format!("{}: {error}", path.display()))
            }) {
            Ok(theme) => match catalog
                .themes
                .iter_mut()
                .find(|existing| existing.name == theme.name)
            {
                Some(existing) => *existing = theme,
                None => catalog.themes.push(theme),
            },
            Err(warning) => catalog.warnings.push(warning),
        }
    }
    catalog.themes.sort_by(|a, b| a.name.cmp(&b.name));
    catalog.themes.insert(0, ThemeSpec::terminal_default());
    catalog.warnings.sort();
    catalog
}

fn user_theme_files(
    dir: &std::path::Path,
    warnings: &mut Vec<String>,
) -> Vec<(String, std::path::PathBuf)> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // A missing themes directory is the normal case, not a problem.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            warnings.push(format!("{}: {error}", dir.display()));
            return Vec::new();
        }
    };
    let mut files = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "toml") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let name = spotuify_core::canonical_theme_name(stem);
        if RESERVED_THEME_NAMES.contains(&name.as_str()) {
            warnings.push(format!(
                "{}: `{name}` is a reserved theme name",
                path.display()
            ));
            continue;
        }
        // Follows symlinks, unlike `entry.metadata()`, because that is what
        // the `read_to_string` below will do.
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.len() > MAX_THEME_FILE_BYTES => {
                warnings.push(format!(
                    "{}: {} bytes exceeds the {MAX_THEME_FILE_BYTES} byte theme limit",
                    path.display(),
                    metadata.len()
                ));
                continue;
            }
            Ok(_) => {}
            Err(error) => {
                warnings.push(format!("{}: {error}", path.display()));
                continue;
            }
        }
        files.push((name, path));
    }
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    /// cliamp's `accessibility_test.go`, ported: every shipped theme must
    /// stay readable on its own background. A theme that fails this is a
    /// theme nobody can use, so it never ships.
    fn contrast_ratio(a: &str, b: &str) -> f64 {
        let lighter = relative_luminance(a);
        let darker = relative_luminance(b);
        let (lighter, darker) = if lighter < darker {
            (darker, lighter)
        } else {
            (lighter, darker)
        };
        (lighter + 0.05) / (darker + 0.05)
    }

    fn relative_luminance(hex: &str) -> f64 {
        let [r, g, b] = spotuify_core::hex_rgb(hex).expect("hex colour");
        let linear = |channel: u8| {
            let component = f64::from(channel) / 255.0;
            if component <= 0.040_45 {
                component / 12.92
            } else {
                ((component + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    }

    #[test]
    fn every_builtin_theme_parses_and_is_complete() {
        let themes = builtin_themes();
        assert_eq!(themes.len(), BUILTIN_THEMES.len());
        for theme in &themes {
            assert_eq!(theme.source, ThemeSource::Builtin);
            assert!(
                !theme.is_terminal_default(),
                "{} has no colours",
                theme.name
            );
            theme.validate().expect("shipped theme is valid");
        }
    }

    #[test]
    fn every_builtin_theme_meets_wcag_text_contrast() {
        for theme in builtin_themes() {
            let bg = theme
                .bg
                .as_deref()
                .unwrap_or_else(|| unreachable!("built-in theme {} must set bg", theme.name));
            for (role, value) in theme.roles() {
                let value = value.expect("validated theme has every role");
                let ratio = contrast_ratio(value, bg);
                assert!(
                    ratio >= 4.5,
                    "theme {} {role} contrast = {ratio:.2}:1, want at least 4.5:1",
                    theme.name
                );
            }
        }
    }

    #[test]
    fn a_theme_file_may_omit_bg_and_may_carry_unknown_keys() {
        let theme = parse_theme(
            "minimal",
            ThemeSource::User,
            r##"
            # a comment
            accent = "#00FF00"
            bright_fg = "#FFFFFF"
            fg = "#969696"
            green = "#29CE10"
            yellow = "#D6B521"
            red = "#EF3110"
            cursor = "#123456"
            "##,
        )
        .expect("theme without bg");
        assert_eq!(theme.bg, None);
        assert_eq!(theme.accent.as_deref(), Some("#00FF00"));
    }

    #[test]
    fn a_theme_file_missing_a_role_names_it() {
        let error = parse_theme(
            "half",
            ThemeSource::User,
            r##"accent = "#00FF00"
bright_fg = "#FFFFFF""##,
        )
        .expect_err("incomplete theme");
        assert!(error.to_string().contains("fg is required"), "{error}");
    }

    #[test]
    fn a_theme_file_with_bad_hex_is_rejected() {
        let error = parse_theme(
            "bad",
            ThemeSource::User,
            r##"accent = "lime"
bright_fg = "#FFFFFF"
fg = "#969696"
green = "#29CE10"
yellow = "#D6B521"
red = "#EF3110""##,
        )
        .expect_err("bad hex");
        assert!(error.to_string().contains("#RRGGBB"), "{error}");
    }

    #[test]
    fn theme_names_are_canonicalised_on_parse() {
        let theme = parse_theme(
            "  Tokyo-Night ",
            ThemeSource::User,
            r##"accent = "#00FF00"
bright_fg = "#FFFFFF"
fg = "#969696"
green = "#29CE10"
yellow = "#D6B521"
red = "#EF3110""##,
        )
        .expect("theme");
        assert_eq!(theme.name, "tokyo-night");
    }

    #[test]
    fn a_user_file_overrides_the_builtin_of_the_same_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("nord.toml"),
            r##"bg = "#111111"
accent = "#222222"
bright_fg = "#FFFFFF"
fg = "#969696"
green = "#29CE10"
yellow = "#D6B521"
red = "#EF3110""##,
        )
        .expect("write nord");

        let catalog = load_themes_from(dir.path());
        let nord = catalog.get("nord").expect("nord");
        assert_eq!(nord.source, ThemeSource::User);
        assert_eq!(nord.accent.as_deref(), Some("#222222"));
        assert_eq!(
            catalog.themes.len(),
            BUILTIN_THEMES.len() + 1,
            "override must replace, not duplicate: {}",
            catalog.names()
        );
        assert!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
    }

    #[test]
    fn a_broken_user_file_is_skipped_with_a_warning() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("broken.toml"), "accent = ").expect("write broken");
        std::fs::write(
            dir.path().join("ok.toml"),
            r##"accent = "#00FF00"
bright_fg = "#FFFFFF"
fg = "#969696"
green = "#29CE10"
yellow = "#D6B521"
red = "#EF3110""##,
        )
        .expect("write ok");

        let catalog = load_themes_from(dir.path());
        assert!(catalog.get("broken").is_none());
        assert!(catalog.get("ok").is_some(), "{}", catalog.names());
        assert_eq!(catalog.warnings.len(), 1, "{:?}", catalog.warnings);
        assert!(catalog.warnings[0].contains("broken.toml"));
    }

    #[test]
    fn the_sentinel_leads_the_list_and_cannot_be_shadowed() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("terminal-default.toml"),
            r##"accent = "#00FF00"
bright_fg = "#FFFFFF"
fg = "#969696"
green = "#29CE10"
yellow = "#D6B521"
red = "#EF3110""##,
        )
        .expect("write sentinel");

        let catalog = load_themes_from(dir.path());
        assert_eq!(catalog.themes[0].name, TERMINAL_DEFAULT_THEME);
        assert!(catalog.themes[0].is_terminal_default());
        assert_eq!(catalog.warnings.len(), 1, "{:?}", catalog.warnings);
        assert!(catalog.warnings[0].contains("reserved"));
    }

    /// A file is never the sentinel. Before this, a file holding only
    /// `yellow` and `red` parsed to an all-but-empty spec, looked like
    /// `terminal-default`, validated, and left the user on built-in colours
    /// with no error to explain it.
    #[test]
    fn a_file_with_only_some_roles_is_rejected_not_treated_as_the_sentinel() {
        let error = parse_theme(
            "scraps",
            ThemeSource::User,
            "yellow = \"#D6B521\"\nred = \"#EF3110\"\n",
        )
        .expect_err("a partial theme is not a theme");
        assert!(error.to_string().contains("accent is required"), "{error}");
    }

    #[test]
    fn a_file_with_only_a_background_is_rejected() {
        let error = parse_theme("just-bg", ThemeSource::User, "bg = \"#000000\"\n")
            .expect_err("bg alone is not a theme");
        assert!(error.to_string().contains("accent is required"), "{error}");
    }

    #[test]
    fn an_empty_file_is_rejected_rather_than_read_as_no_theme() {
        let error = parse_theme("blank", ThemeSource::User, "# nothing but a comment\n")
            .expect_err("an empty file is not a theme");
        assert!(error.to_string().contains("accent is required"), "{error}");
    }

    #[test]
    fn an_oversized_file_is_skipped_with_a_warning() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Valid TOML, just far more of it than a theme could ever need.
        let mut bloat = String::from("accent = \"#00FF00\"\n");
        bloat.push_str(&"# padding\n".repeat(20_000));
        assert!(bloat.len() as u64 > MAX_THEME_FILE_BYTES);
        std::fs::write(dir.path().join("huge.toml"), &bloat).expect("write huge");
        std::fs::write(
            dir.path().join("ok.toml"),
            "accent = \"#00FF00\"\nbright_fg = \"#FFFFFF\"\nfg = \"#969696\"\ngreen = \"#29CE10\"\nyellow = \"#D6B521\"\nred = \"#EF3110\"\n",
        )
        .expect("write ok");

        let catalog = load_themes_from(dir.path());
        assert!(catalog.get("huge").is_none(), "{}", catalog.names());
        assert!(
            catalog.get("ok").is_some(),
            "one big file must not hide the rest"
        );
        assert_eq!(catalog.warnings.len(), 1, "{:?}", catalog.warnings);
        assert!(
            catalog.warnings[0].contains("exceeds"),
            "{:?}",
            catalog.warnings
        );
    }

    #[test]
    fn cli_subcommand_names_are_reserved_so_no_theme_is_unreachable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let body = "accent = \"#00FF00\"\nbright_fg = \"#FFFFFF\"\nfg = \"#969696\"\ngreen = \"#29CE10\"\nyellow = \"#D6B521\"\nred = \"#EF3110\"\n";
        for name in ["list", "path"] {
            std::fs::write(dir.path().join(format!("{name}.toml")), body).expect("write");
        }

        let catalog = load_themes_from(dir.path());
        // `spotuify theme list` lists; it can never mean "apply the theme
        // called list", so such a theme must not exist in the first place.
        assert!(catalog.get("list").is_none(), "{}", catalog.names());
        assert!(catalog.get("path").is_none(), "{}", catalog.names());
        assert_eq!(catalog.warnings.len(), 2, "{:?}", catalog.warnings);
        assert!(catalog
            .warnings
            .iter()
            .all(|warning| warning.contains("reserved")));
    }

    #[test]
    fn a_missing_themes_directory_is_not_a_warning() {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog = load_themes_from(&dir.path().join("does-not-exist"));
        assert!(catalog.warnings.is_empty());
        assert_eq!(catalog.themes.len(), BUILTIN_THEMES.len() + 1);
    }
}
