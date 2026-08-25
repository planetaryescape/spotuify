//! Terminal colour themes.
//!
//! A theme is seven colour roles borrowed from the cliamp theme format
//! (MIT, <https://github.com/bjarneo/cliamp>) so the ecosystem of themes
//! people already wrote for cliamp drops straight into spotuify. This
//! module owns the *value*: the roles, their hex validation, and the
//! `terminal-default` sentinel. Reading `.toml` files off disk and
//! merging built-ins with user overrides is `spotuify-config`'s job,
//! because that is the crate that knows where the config directory is.

use serde::{Deserialize, Serialize};

/// The theme that means "leave the built-in palette alone". It carries no
/// hex values, so the TUI keeps the colours it ships with.
pub const TERMINAL_DEFAULT_THEME: &str = "terminal-default";

/// Where a theme came from. A user file shadows a built-in of the same
/// name, and clients show the badge so an override is visible.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeSource {
    Builtin,
    User,
}

impl ThemeSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::User => "user",
        }
    }
}

/// A resolved theme. Every colour is `#RRGGBB`; only `bg` may be absent
/// in a real theme (meaning "keep the terminal's own background"), and
/// all seven are absent for [`TERMINAL_DEFAULT_THEME`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ThemeSpec {
    pub name: String,
    pub source: ThemeSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bright_fg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub green: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yellow: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub red: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ThemeError {
    #[error("theme `{theme}`: {role} is required")]
    MissingRole { theme: String, role: String },
    #[error("theme `{theme}`: {role} must be #RRGGBB, got `{value}`")]
    BadHex {
        theme: String,
        role: String,
        value: String,
    },
}

impl ThemeSpec {
    /// The sentinel theme: no colours, so the built-in palette applies.
    pub fn terminal_default() -> Self {
        Self {
            name: TERMINAL_DEFAULT_THEME.to_string(),
            source: ThemeSource::Builtin,
            bg: None,
            accent: None,
            bright_fg: None,
            fg: None,
            green: None,
            yellow: None,
            red: None,
        }
    }

    pub fn is_terminal_default(&self) -> bool {
        self.accent.is_none()
            && self.bright_fg.is_none()
            && self.fg.is_none()
            && self.green.is_none()
    }

    /// Every required role paired with its value, for validation and for
    /// rendering a colour strip in the same order everywhere.
    pub fn roles(&self) -> [(&'static str, Option<&str>); 6] {
        [
            ("accent", self.accent.as_deref()),
            ("bright_fg", self.bright_fg.as_deref()),
            ("fg", self.fg.as_deref()),
            ("green", self.green.as_deref()),
            ("yellow", self.yellow.as_deref()),
            ("red", self.red.as_deref()),
        ]
    }

    /// Every colour a theme carries, `bg` first, for the table columns and
    /// swatch strips that have to agree on order across clients.
    pub fn columns(&self) -> [(&'static str, Option<&str>); 7] {
        let [accent, bright_fg, fg, green, yellow, red] = self.roles();
        [
            ("bg", self.bg.as_deref()),
            accent,
            bright_fg,
            fg,
            green,
            yellow,
            red,
        ]
    }

    /// Reject a theme that is neither the sentinel nor complete. Matches
    /// cliamp: the six foreground roles are mandatory, `bg` is optional
    /// but must still be well-formed when present.
    pub fn validate(&self) -> Result<(), ThemeError> {
        if self.is_terminal_default() {
            return Ok(());
        }
        for (role, value) in self.roles() {
            let Some(value) = value else {
                return Err(ThemeError::MissingRole {
                    theme: self.name.clone(),
                    role: role.to_string(),
                });
            };
            self.check_hex(role, value)?;
        }
        if let Some(bg) = self.bg.as_deref() {
            self.check_hex("bg", bg)?;
        }
        Ok(())
    }

    fn check_hex(&self, role: &str, value: &str) -> Result<(), ThemeError> {
        if hex_rgb(value).is_some() {
            return Ok(());
        }
        Err(ThemeError::BadHex {
            theme: self.name.clone(),
            role: role.to_string(),
            value: value.to_string(),
        })
    }
}

/// Decode `#RRGGBB` into channels. Returns `None` for anything else —
/// short forms, named colours, and `rgb()` are deliberately unsupported
/// so a theme file means the same thing here as it does in cliamp.
pub fn hex_rgb(value: &str) -> Option<[u8; 3]> {
    let digits = value.strip_prefix('#')?;
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |range: std::ops::Range<usize>| u8::from_str_radix(&digits[range], 16).ok();
    Some([channel(0..2)?, channel(2..4)?, channel(4..6)?])
}

/// Normalise a theme name the way the config loader does: trim and
/// lowercase, so `config set tui.theme " Nord "` and `spotuify theme Nord`
/// address the same file.
pub fn canonical_theme_name(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str) -> ThemeSpec {
        ThemeSpec {
            name: name.to_string(),
            source: ThemeSource::User,
            bg: Some("#000000".to_string()),
            accent: Some("#00FF00".to_string()),
            bright_fg: Some("#FFFFFF".to_string()),
            fg: Some("#969696".to_string()),
            green: Some("#29CE10".to_string()),
            yellow: Some("#D6B521".to_string()),
            red: Some("#EF3110".to_string()),
        }
    }

    #[test]
    fn hex_rgb_accepts_only_six_digit_css_hex() {
        assert_eq!(hex_rgb("#00FF00"), Some([0, 255, 0]));
        assert_eq!(hex_rgb("#00ff00"), Some([0, 255, 0]));
        assert_eq!(hex_rgb("00FF00"), None);
        assert_eq!(hex_rgb("#0F0"), None);
        assert_eq!(hex_rgb("#00FF0G"), None);
        assert_eq!(hex_rgb("#00FF000"), None);
        assert_eq!(hex_rgb("green"), None);
    }

    #[test]
    fn a_complete_theme_validates_and_bg_is_optional() {
        spec("winamp").validate().expect("complete theme");
        let mut no_bg = spec("winamp");
        no_bg.bg = None;
        no_bg.validate().expect("bg is optional");
    }

    #[test]
    fn a_missing_role_names_the_role_it_wants() {
        let mut broken = spec("half");
        broken.yellow = None;
        let error = broken.validate().expect_err("incomplete theme");
        assert_eq!(
            error,
            ThemeError::MissingRole {
                theme: "half".to_string(),
                role: "yellow".to_string(),
            }
        );
        assert!(error.to_string().contains("yellow"));
    }

    #[test]
    fn a_malformed_hex_names_the_role_and_the_value() {
        let mut broken = spec("bad");
        broken.red = Some("red".to_string());
        assert_eq!(
            broken.validate().expect_err("bad hex"),
            ThemeError::BadHex {
                theme: "bad".to_string(),
                role: "red".to_string(),
                value: "red".to_string(),
            }
        );

        let mut bad_bg = spec("bad-bg");
        bad_bg.bg = Some("#12345".to_string());
        assert!(matches!(
            bad_bg.validate(),
            Err(ThemeError::BadHex { role, .. }) if role == "bg"
        ));
    }

    #[test]
    fn the_sentinel_carries_no_colours_and_round_trips() {
        let sentinel = ThemeSpec::terminal_default();
        assert!(sentinel.is_terminal_default());
        sentinel.validate().expect("sentinel is always valid");
        let json = serde_json::to_string(&sentinel).expect("serialize");
        // Absent roles must not become `null`s a client has to special-case.
        assert_eq!(
            json, r#"{"name":"terminal-default","source":"builtin"}"#,
            "sentinel should serialize as name + source only"
        );
        let parsed: ThemeSpec = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, sentinel);
        assert!(!spec("winamp").is_terminal_default());
    }

    #[test]
    fn canonical_names_are_trimmed_and_lowercased() {
        assert_eq!(canonical_theme_name("  Tokyo-Night "), "tokyo-night");
        assert_eq!(canonical_theme_name("NORD"), "nord");
    }
}
