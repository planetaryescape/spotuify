use serde::{Deserialize, Serialize};

/// Source used to resolve lyrics. `Native` means the active media provider's
/// own lyrics transport; `Lrclib` is the provider-independent fallback.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum LyricsProvider {
    /// The compatibility-stage wire label remains `spotify-mercury`; the
    /// neutral `native` label is accepted for future peers.
    #[serde(rename = "spotify-mercury", alias = "spotify", alias = "native")]
    Native,
    #[serde(rename = "lrclib")]
    Lrclib,
}

impl LyricsProvider {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Native => "spotify-mercury",
            Self::Lrclib => "lrclib",
        }
    }

    pub fn from_label(value: &str) -> Option<Self> {
        value.parse().ok()
    }
}

impl std::fmt::Display for LyricsProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl std::str::FromStr for LyricsProvider {
    type Err = LyricsProviderParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "spotify-mercury" | "spotify" | "native" => Ok(Self::Native),
            "lrclib" => Ok(Self::Lrclib),
            other => Err(LyricsProviderParseError {
                value: other.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricsProviderParseError {
    pub value: String,
}

impl std::fmt::Display for LyricsProviderParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown lyrics provider `{}`", self.value)
    }
}

impl std::error::Error for LyricsProviderParseError {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn native_provider_preserves_legacy_wire_and_accepts_neutral_alias() {
        assert_eq!(
            serde_json::to_string(&LyricsProvider::Native).unwrap(),
            "\"spotify-mercury\""
        );
        assert_eq!(
            serde_json::from_str::<LyricsProvider>("\"native\"").unwrap(),
            LyricsProvider::Native
        );
        assert_eq!(
            "spotify".parse::<LyricsProvider>().unwrap(),
            LyricsProvider::Native
        );
    }
}
