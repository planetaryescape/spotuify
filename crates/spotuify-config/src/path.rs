use std::fmt;
use std::str::FromStr;

use crate::{ConfigError, Result};

/// A validated configuration dot-path.
///
/// Legacy Spotify paths parse successfully but always resolve to their
/// canonical provider-scoped destination.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ConfigPath {
    canonical: String,
    alias: Option<String>,
}

impl ConfigPath {
    pub fn parse(value: &str) -> Result<Self> {
        let input = value.trim();
        if input.is_empty() {
            return Err(ConfigError::InvalidPath {
                path: value.to_string(),
                message: "path cannot be blank".to_string(),
            });
        }

        let input_segments = input.split('.').collect::<Vec<_>>();
        if input_segments.iter().any(|segment| {
            segment.is_empty()
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        }) {
            return Err(ConfigError::InvalidPath {
                path: value.to_string(),
                message: "segments may contain only ASCII letters, digits, `_`, or `-`".to_string(),
            });
        }

        let canonical = canonicalize(input_segments.as_slice())?;
        let alias = (canonical != input).then(|| input.to_string());
        Ok(Self { canonical, alias })
    }

    pub fn canonical(&self) -> &str {
        &self.canonical
    }

    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.canonical.split('.')
    }

    pub fn was_alias(&self) -> bool {
        self.alias.is_some()
    }

    pub fn alias(&self) -> Option<&str> {
        self.alias.as_deref()
    }

    pub(crate) fn is_legacy_provider_alias(&self) -> bool {
        self.alias().is_some_and(|alias| {
            !alias.starts_with("providers.")
                && (matches!(
                    alias,
                    "client_id"
                        | "client-id"
                        | "client_secret"
                        | "client-secret"
                        | "redirect_uri"
                        | "redirect-uri"
                ) || alias.starts_with("player.")
                    && alias != "player.event_hook"
                    && alias != "player.event-hook"
                    || alias == "spotifyd.device_name"
                    || alias == "spotifyd.device-name")
        })
    }

    /// True when values at this path must be redacted in normal output.
    pub fn is_secret(&self) -> bool {
        self.segments().any(|segment| {
            let segment = segment.to_ascii_lowercase().replace('-', "_");
            [
                "secret",
                "password",
                "token",
                "credential",
                "private_key",
                "api_key",
                "authorization",
            ]
            .iter()
            .any(|marker| segment.contains(marker))
        })
    }

    pub(crate) fn read_candidates(&self) -> Vec<String> {
        let mut candidates = vec![self.canonical.clone()];
        match self.canonical.as_str() {
            "providers.spotify.client_id" => candidates.push("client_id".to_string()),
            "providers.spotify.client_secret" => candidates.push("client_secret".to_string()),
            "providers.spotify.redirect_uri" => candidates.push("redirect_uri".to_string()),
            "analytics.hook_command" => candidates.push("player.event_hook".to_string()),
            "providers.spotify.player.device_name" => {
                candidates.push("player.device_name".to_string());
                candidates.push("spotifyd.device_name".to_string());
            }
            canonical if canonical.starts_with("providers.spotify.player.") => {
                if let Some(field) = canonical.strip_prefix("providers.spotify.player.") {
                    candidates.push(format!("player.{field}"));
                }
            }
            _ => {}
        }
        if let Some(alias) = self.alias.as_ref() {
            candidates.push(alias.clone());
        }
        let hyphenated = candidates
            .iter()
            .map(|candidate| candidate.replace('_', "-"))
            .collect::<Vec<_>>();
        candidates.extend(hyphenated);
        let mut seen = std::collections::HashSet::new();
        candidates
            .into_iter()
            .filter(|candidate| seen.insert(candidate.clone()))
            .collect()
    }
}

impl fmt::Display for ConfigPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.canonical())
    }
}

impl FromStr for ConfigPath {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}

fn canonicalize(segments: &[&str]) -> Result<String> {
    let normalized = segments
        .iter()
        .map(|segment| segment.replace('-', "_"))
        .collect::<Vec<_>>();

    let canonical = match normalized.as_slice() {
        [key] if matches!(key.as_str(), "client_id" | "client_secret" | "redirect_uri") => {
            format!("providers.spotify.{key}")
        }
        [table, field] if table == "spotifyd" && field == "device_name" => {
            "providers.spotify.player.device_name".to_string()
        }
        [table, field] if table == "player" && field == "event_hook" => {
            "analytics.hook_command".to_string()
        }
        [table, tail @ ..] if table == "player" && !tail.is_empty() => {
            format!("providers.spotify.player.{}", tail.join("."))
        }
        [providers, provider, tail @ ..] if providers == "providers" && !tail.is_empty() => {
            let provider = provider.replace('_', "-");
            spotuify_core::ProviderId::new(provider.clone()).map_err(|error| {
                ConfigError::InvalidPath {
                    path: segments.join("."),
                    message: error.to_string(),
                }
            })?;
            format!("providers.{provider}.{}", tail.join("."))
        }
        _ => normalized.join("."),
    };
    Ok(canonical)
}

/// A config value that remains available to authorized callers while keeping
/// its normal `Display` and `Debug` representations redacted.
#[derive(Clone, Eq, PartialEq)]
pub struct ConfigValue {
    value: String,
    secret: bool,
}

impl ConfigValue {
    pub(crate) fn new(value: String, secret: bool) -> Self {
        Self { value, secret }
    }

    pub fn expose(&self) -> &str {
        &self.value
    }

    pub fn is_secret(&self) -> bool {
        self.secret
    }

    pub fn redacted(&self) -> &str {
        if self.secret {
            "<redacted>"
        } else {
            &self.value
        }
    }
}

impl fmt::Debug for ConfigValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ConfigValue")
            .field(&self.redacted())
            .finish()
    }
}

impl fmt::Display for ConfigValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.redacted())
    }
}
