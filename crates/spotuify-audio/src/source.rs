//! Visualization source selection.
//!
//! `VizSourceKind` is the wire-format enum carried over IPC and stored in
//! config. `VizSource` is the in-memory enum tracking what is *actually*
//! running (which may differ from what the user configured, e.g. user
//! asked for `Sink` but the embedded backend is not active so we fall
//! back to `Loopback`).

use serde::{Deserialize, Serialize};

/// User-facing source selection. `Auto` lets the daemon pick.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VizSourceKind {
    /// Let the daemon pick based on active backend.
    #[default]
    Auto,
    /// Tap inside the embedded librespot sink chain.
    Sink,
    /// System-audio loopback (cpal monitor / WASAPI / BlackHole / PipeWire).
    Loopback,
    /// Disabled.
    None,
}

impl VizSourceKind {
    /// Parse from a config string. Unknown strings fall back to `Auto`.
    pub fn from_config_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "sink" => Self::Sink,
            "loopback" => Self::Loopback,
            "none" | "off" | "disabled" => Self::None,
            _ => Self::Auto,
        }
    }

    pub fn as_config_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Sink => "sink",
            Self::Loopback => "loopback",
            Self::None => "none",
        }
    }
}

/// In-memory active-source state. `LoopbackCpal` / `LoopbackPipewire`
/// disambiguate which loopback implementation is in use so doctor and
/// telemetry can report it precisely.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum VizSource {
    Sink,
    LoopbackCpal,
    LoopbackPipewire,
    None,
}

impl VizSource {
    pub fn is_active(self) -> bool {
        !matches!(self, Self::None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_kinds() {
        assert_eq!(VizSourceKind::from_config_str("sink"), VizSourceKind::Sink);
        assert_eq!(
            VizSourceKind::from_config_str("loopback"),
            VizSourceKind::Loopback
        );
        assert_eq!(VizSourceKind::from_config_str("none"), VizSourceKind::None);
        assert_eq!(VizSourceKind::from_config_str("off"), VizSourceKind::None);
        assert_eq!(VizSourceKind::from_config_str("auto"), VizSourceKind::Auto);
    }

    #[test]
    fn unknown_falls_back_to_auto() {
        assert_eq!(
            VizSourceKind::from_config_str("nonsense"),
            VizSourceKind::Auto
        );
    }

    #[test]
    fn config_str_roundtrip() {
        for kind in [
            VizSourceKind::Auto,
            VizSourceKind::Sink,
            VizSourceKind::Loopback,
            VizSourceKind::None,
        ] {
            assert_eq!(
                VizSourceKind::from_config_str(kind.as_config_str()),
                kind,
                "roundtrip failed for {:?}",
                kind
            );
        }
    }

    #[test]
    fn is_active_reflects_state() {
        assert!(VizSource::Sink.is_active());
        assert!(VizSource::LoopbackCpal.is_active());
        assert!(VizSource::LoopbackPipewire.is_active());
        assert!(!VizSource::None.is_active());
    }
}
