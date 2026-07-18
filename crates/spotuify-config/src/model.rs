use std::fmt;
use std::path::{Path, PathBuf};

use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Serialize};
use spotuify_core::ProviderId;
use thiserror::Error;
use toml_edit::DocumentMut;

use crate::document::init_config;
use crate::{ConfigPath, ConfigValue, Result};

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read or write config at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid TOML config at {path}; secret values were omitted from the error")]
    Parse { path: PathBuf },
    #[error("invalid editable TOML config at {path}; secret values were omitted from the error")]
    EditParse { path: PathBuf },
    #[error("invalid config path `{path}`: {message}")]
    InvalidPath { path: String, message: String },
    #[error("invalid config: {0}")]
    Invalid(String),
    #[error("provider `{provider}` config is invalid; secret values were omitted from the error")]
    ProviderDecode { provider: ProviderId },
    #[error("timed out waiting for config lock at {0}")]
    LockTimeout(PathBuf),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigWarning {
    DeprecatedKey {
        legacy_path: String,
        canonical_path: String,
        ignored: bool,
    },
}

impl fmt::Display for ConfigWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeprecatedKey {
                legacy_path,
                canonical_path,
                ignored,
            } => {
                write!(
                    formatter,
                    "deprecated config key `{legacy_path}`; move it to `{canonical_path}`"
                )?;
                if *ignored {
                    formatter.write_str(" (canonical value takes precedence)")?;
                }
                Ok(())
            }
        }
    }
}

/// Process environment values with defined precedence over file config.
/// Explicit construction keeps loader tests deterministic.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct EnvOverrides {
    pub spotify_client_id: Option<String>,
    pub spotify_client_secret: Option<String>,
    pub spotify_redirect_uri: Option<String>,
    pub lastfm_api_key: Option<String>,
    pub lastfm_user: Option<String>,
}

impl fmt::Debug for EnvOverrides {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnvOverrides")
            .field("spotify_client_id", &self.spotify_client_id)
            .field(
                "spotify_client_secret",
                &self.spotify_client_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("spotify_redirect_uri", &self.spotify_redirect_uri)
            .field(
                "lastfm_api_key",
                &self.lastfm_api_key.as_ref().map(|_| "<redacted>"),
            )
            .field("lastfm_user", &self.lastfm_user)
            .finish()
    }
}

impl EnvOverrides {
    pub fn from_process() -> Self {
        Self {
            spotify_client_id: nonblank_env("SPOTUIFY_CLIENT_ID"),
            spotify_client_secret: nonblank_env("SPOTUIFY_CLIENT_SECRET"),
            spotify_redirect_uri: nonblank_env("SPOTUIFY_REDIRECT_URI"),
            lastfm_api_key: nonblank_env("SPOTUIFY_LASTFM_API_KEY"),
            lastfm_user: nonblank_env("SPOTUIFY_LASTFM_USER"),
        }
    }
}

/// Ordered, typed, non-persistent configuration overlays. Later entries win.
#[derive(Clone, Default, PartialEq)]
pub struct ConfigOverrides {
    entries: Vec<(ConfigPath, toml::Value)>,
}

impl fmt::Debug for ConfigOverrides {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let entries = self
            .entries
            .iter()
            .map(|(path, value)| {
                (
                    path.canonical(),
                    if path.is_secret() {
                        "<redacted>".to_string()
                    } else {
                        value.to_string()
                    },
                )
            })
            .collect::<Vec<_>>();
        formatter
            .debug_tuple("ConfigOverrides")
            .field(&entries)
            .finish()
    }
}

impl ConfigOverrides {
    pub fn parse<I, S>(values: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut entries = Vec::new();
        for raw in values {
            let raw = raw.as_ref().trim();
            if raw.is_empty() {
                continue;
            }
            let (key, value) = raw.split_once('=').ok_or_else(|| {
                ConfigError::Invalid("config override must use key=value syntax".to_string())
            })?;
            let path = ConfigPath::parse(key)?;
            let parsed = toml::from_str::<toml::Table>(&format!("value = {value}"))
                .ok()
                .and_then(|mut table| table.remove("value"))
                .ok_or_else(|| {
                    ConfigError::Invalid(format!(
                        "config override for `{}` has an invalid TOML value",
                        path.canonical()
                    ))
                })?;
            entries.push((path, parsed));
        }
        Ok(Self { entries })
    }

    pub fn from_process() -> Result<Self> {
        match std::env::var("SPOTUIFY_CONFIG_OVERRIDES") {
            Ok(raw) => Self::parse(raw.lines()),
            Err(std::env::VarError::NotPresent) => Ok(Self::default()),
            Err(std::env::VarError::NotUnicode(_)) => Err(ConfigError::Invalid(
                "SPOTUIFY_CONFIG_OVERRIDES must be valid UTF-8".to_string(),
            )),
        }
    }

    fn apply(&self, root: &mut toml::Table) -> Result<()> {
        for (path, value) in &self.entries {
            let target = resolve_overlay_path(root, path)?;
            set_toml_path(root, &target, value.clone())?;
        }
        Ok(())
    }
}

fn resolve_overlay_path(root: &toml::Table, path: &ConfigPath) -> Result<ConfigPath> {
    if !path.is_legacy_provider_alias() || !path.canonical().starts_with("providers.spotify.") {
        return Ok(path.clone());
    }
    let Some(providers) = root.get("providers").and_then(toml::Value::as_table) else {
        return Ok(path.clone());
    };
    let provider_kind = |id: &str| {
        providers
            .get(id)
            .and_then(toml::Value::as_table)
            .and_then(|table| table.get("type"))
            .and_then(toml::Value::as_str)
    };
    if provider_kind("spotify").is_some_and(|kind| kind == "spotify") {
        return Ok(path.clone());
    }
    let spotify_ids = providers
        .iter()
        .filter(|(_, value)| {
            value
                .as_table()
                .and_then(|table| table.get("type"))
                .and_then(toml::Value::as_str)
                .is_some_and(|kind| kind == "spotify")
        })
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    let target = match spotify_ids.as_slice() {
        [only] => only,
        [] if provider_kind("spotify").is_none() => return Ok(path.clone()),
        [] => {
            return Err(ConfigError::Invalid(
                "legacy Spotify override conflicts with non-Spotify provider `spotify`".to_string(),
            ));
        }
        _ => {
            return Err(ConfigError::Invalid(
                "legacy Spotify override is ambiguous; use providers.<id> explicitly".to_string(),
            ));
        }
    };
    ConfigPath::parse(&path.canonical().replacen(
        "providers.spotify.",
        &format!("providers.{target}."),
        1,
    ))
}

fn nonblank_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .and_then(|value| nonblank(Some(value)))
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppConfig {
    pub path: PathBuf,
    pub default_provider: Option<ProviderId>,
    /// File order is retained so config UIs and diagnostics remain stable.
    pub providers: Vec<ProviderEntry>,
    pub cache: CacheConfig,
    pub analytics: AnalyticsConfig,
    pub notifications: NotificationsConfig,
    pub discord: DiscordConfig,
    pub viz: VizConfig,
}

impl AppConfig {
    pub fn provider(&self, id: &ProviderId) -> Option<&ProviderEntry> {
        self.providers.iter().find(|provider| &provider.id == id)
    }

    pub fn default_provider(&self) -> Option<&ProviderEntry> {
        self.default_provider
            .as_ref()
            .and_then(|id| self.provider(id))
    }

    /// Mirror a live audio-output mutation into the accepted in-memory
    /// snapshot. Persistence remains the document layer's responsibility.
    pub fn set_default_player_audio_output(&mut self, device: Option<String>) -> Result<()> {
        let provider_id = self
            .default_provider
            .clone()
            .ok_or_else(|| ConfigError::Invalid("no default provider is configured".to_string()))?;
        let provider = self
            .providers
            .iter_mut()
            .find(|provider| provider.id == provider_id)
            .ok_or_else(|| {
                ConfigError::Invalid(format!(
                    "default provider `{provider_id}` is not configured"
                ))
            })?;
        let player = provider
            .raw
            .entry("player".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .ok_or_else(|| ConfigError::ProviderDecode {
                provider: provider_id.clone(),
            })?;
        match device {
            Some(device) => {
                player.insert(
                    "audio_output_device".to_string(),
                    toml::Value::String(device),
                );
            }
            None => {
                player.remove("audio_output_device");
            }
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq)]
pub struct ProviderEntry {
    pub id: ProviderId,
    pub kind: String,
    raw: toml::Table,
}

impl ProviderEntry {
    pub fn raw_table(&self) -> &toml::Table {
        &self.raw
    }

    pub fn deserialize<T: DeserializeOwned>(&self) -> Result<T> {
        toml::Value::Table(self.raw.clone())
            .try_into()
            .map_err(|_| ConfigError::ProviderDecode {
                provider: self.id.clone(),
            })
    }

    pub fn player_settings(&self) -> Result<PlayerSettings> {
        let settings = self
            .raw
            .get("player")
            .cloned()
            .map(toml::Value::try_into::<PlayerSettings>)
            .transpose()
            .map_err(|_| ConfigError::ProviderDecode {
                provider: self.id.clone(),
            })?
            .unwrap_or_default();
        settings.validate()?;
        Ok(settings)
    }
}

impl fmt::Debug for ProviderEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let keys = self.raw.keys().map(String::as_str).collect::<Vec<_>>();
        formatter
            .debug_struct("ProviderEntry")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("keys", &keys)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoadedConfig {
    pub config: AppConfig,
    pub warnings: Vec<ConfigWarning>,
    effective: toml::Table,
}

impl LoadedConfig {
    pub fn effective_value(&self, path: &ConfigPath) -> Option<ConfigValue> {
        let mut value = None;
        let mut table = &self.effective;
        let segments = path.segments().collect::<Vec<_>>();
        for (index, segment) in segments.iter().enumerate() {
            let current = table.get(*segment)?;
            if index + 1 == segments.len() {
                value = Some(match current {
                    toml::Value::String(value) => value.clone(),
                    _ => current.to_string(),
                });
                break;
            }
            table = current.as_table()?;
        }
        value.map(|value| ConfigValue::new(value, path.is_secret()))
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct CacheConfig {
    pub cover_cache_mb: u64,
    pub cover_cache_ttl_days: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            cover_cache_mb: 200,
            cover_cache_ttl_days: 30,
        }
    }
}

impl CacheConfig {
    fn normalize(mut self) -> Self {
        if self.cover_cache_ttl_days == 0 {
            self.cover_cache_ttl_days = Self::default().cover_cache_ttl_days;
        }
        self
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct AnalyticsConfig {
    pub store_raw_queries: bool,
    pub retention_progress_days: u32,
    pub retention_events_days: u32,
    pub retention_operations_days: u32,
    pub daily_rollup_hour: u8,
    pub hook_command: Option<String>,
    pub hook_timeout_ms: u64,
    pub allow_file_credentials: bool,
    pub lastfm_api_key: Option<String>,
    pub lastfm_user: Option<String>,
}

impl fmt::Debug for AnalyticsConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnalyticsConfig")
            .field("store_raw_queries", &self.store_raw_queries)
            .field("retention_progress_days", &self.retention_progress_days)
            .field("retention_events_days", &self.retention_events_days)
            .field("retention_operations_days", &self.retention_operations_days)
            .field("daily_rollup_hour", &self.daily_rollup_hour)
            .field("hook_command", &self.hook_command)
            .field("hook_timeout_ms", &self.hook_timeout_ms)
            .field("allow_file_credentials", &self.allow_file_credentials)
            .field(
                "lastfm_api_key",
                &self.lastfm_api_key.as_ref().map(|_| "<redacted>"),
            )
            .field("lastfm_user", &self.lastfm_user)
            .finish()
    }
}

impl Default for AnalyticsConfig {
    fn default() -> Self {
        Self {
            store_raw_queries: true,
            retention_progress_days: 90,
            retention_events_days: 365,
            retention_operations_days: 90,
            daily_rollup_hour: 3,
            hook_command: None,
            hook_timeout_ms: 5_000,
            allow_file_credentials: false,
            lastfm_api_key: None,
            lastfm_user: None,
        }
    }
}

impl AnalyticsConfig {
    fn normalize(mut self, env: &EnvOverrides) -> Self {
        let defaults = Self::default();
        if self.daily_rollup_hour > 23 {
            self.daily_rollup_hour = defaults.daily_rollup_hour;
        }
        if self.hook_timeout_ms == 0 {
            self.hook_timeout_ms = defaults.hook_timeout_ms;
        }
        self.hook_command = nonblank(self.hook_command);
        self.lastfm_api_key = env
            .lastfm_api_key
            .clone()
            .or_else(|| nonblank(self.lastfm_api_key));
        self.lastfm_user = env
            .lastfm_user
            .clone()
            .or_else(|| nonblank(self.lastfm_user));
        self
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct NotificationsConfig {
    pub enabled: bool,
    pub summary: String,
    pub body: String,
    pub on_track_change: bool,
    pub on_pause: bool,
    pub on_resume: bool,
    pub on_skip: bool,
    pub on_error: bool,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            summary: "{track}".to_string(),
            body: "{artist} - {album}".to_string(),
            on_track_change: true,
            on_pause: false,
            on_resume: false,
            on_skip: false,
            on_error: true,
        }
    }
}

impl NotificationsConfig {
    fn normalize(mut self) -> Self {
        let defaults = Self::default();
        self.summary = nonblank(Some(self.summary)).unwrap_or(defaults.summary);
        self.body = nonblank(Some(self.body)).unwrap_or(defaults.body);
        self
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct DiscordConfig {
    pub enabled: bool,
    pub application_id: Option<String>,
}

impl DiscordConfig {
    fn normalize(mut self) -> Self {
        self.application_id = nonblank(self.application_id);
        self
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct VizConfig {
    pub enabled: bool,
    pub source: String,
    pub target_fps: u8,
    pub smoothing: f32,
    pub noise_gate: f32,
    pub color_scheme: String,
}

impl Default for VizConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            source: "auto".to_string(),
            target_fps: 30,
            smoothing: 0.5,
            noise_gate: 0.005,
            color_scheme: "spotify-green".to_string(),
        }
    }
}

impl VizConfig {
    fn normalize(mut self) -> Self {
        self.source = match self.source.trim().to_ascii_lowercase().as_str() {
            "auto" | "sink" | "loopback" | "none" => self.source.trim().to_ascii_lowercase(),
            _ => Self::default().source,
        };
        self.target_fps = self.target_fps.clamp(1, 60);
        self.smoothing = self.smoothing.clamp(0.0, 0.95);
        self.noise_gate = self.noise_gate.clamp(0.0, 1.0);
        self.color_scheme = match self.color_scheme.trim().to_ascii_lowercase().as_str() {
            "spotify-green" | "rainbow" | "monochrome" => {
                self.color_scheme.trim().to_ascii_lowercase()
            }
            _ => Self::default().color_scheme,
        };
        self
    }
}

/// Provider-neutral player settings suitable for nesting in an adapter's
/// provider config. Provider-specific playback settings stay in the adapter.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PlayerSettings {
    pub bitrate: u32,
    pub device_name: Option<String>,
    pub audio_output_device: Option<String>,
    pub normalization: bool,
    pub audio_cache_mib: u32,
    pub pulse_props: bool,
}

impl Default for PlayerSettings {
    fn default() -> Self {
        Self {
            bitrate: 320,
            device_name: None,
            audio_output_device: None,
            normalization: false,
            audio_cache_mib: 0,
            pulse_props: true,
        }
    }
}

impl<'de> Deserialize<'de> for PlayerSettings {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(default)]
        struct Wire {
            backend: Option<String>,
            bitrate: u32,
            device_name: Option<String>,
            audio_output_device: Option<String>,
            normalization: bool,
            audio_cache_mib: u32,
            pulse_props: bool,
        }

        impl Default for Wire {
            fn default() -> Self {
                let settings = PlayerSettings::default();
                Self {
                    backend: None,
                    bitrate: settings.bitrate,
                    device_name: settings.device_name,
                    audio_output_device: settings.audio_output_device,
                    normalization: settings.normalization,
                    audio_cache_mib: settings.audio_cache_mib,
                    pulse_props: settings.pulse_props,
                }
            }
        }

        let wire = Wire::deserialize(deserializer)?;
        if let Some(value) = wire.backend.as_deref() {
            validate_legacy_backend(value).map_err(D::Error::custom)?;
        }
        Ok(Self {
            bitrate: wire.bitrate,
            device_name: wire.device_name,
            audio_output_device: wire.audio_output_device,
            normalization: wire.normalization,
            audio_cache_mib: wire.audio_cache_mib,
            pulse_props: wire.pulse_props,
        })
    }
}

pub(crate) fn validate_legacy_backend(value: &str) -> std::result::Result<(), String> {
    if value == "embedded" {
        Ok(())
    } else {
        Err(format!(
            "unknown player backend `{value}`; only `embedded` is supported"
        ))
    }
}

impl PlayerSettings {
    pub fn validate(&self) -> Result<()> {
        if !matches!(self.bitrate, 96 | 160 | 320) {
            return Err(ConfigError::Invalid(format!(
                "player bitrate must be one of 96, 160, 320 (got {})",
                self.bitrate
            )));
        }
        Ok(())
    }

    pub fn effective_device_name(&self) -> String {
        if let Some(name) = nonblank(self.device_name.clone()) {
            return name;
        }
        let instance = spotuify_protocol::paths::app_instance_name();
        let fallback = std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .ok()
            .and_then(|value| nonblank(Some(value)))
            .unwrap_or_else(|| instance.clone());
        if instance == "spotuify" || fallback.contains(&instance) {
            fallback
        } else {
            format!("{fallback}-{instance}")
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct GenericSections {
    cache: CacheConfig,
    analytics: AnalyticsConfig,
    notifications: NotificationsConfig,
    discord: DiscordConfig,
    viz: VizConfig,
}

pub fn load() -> Result<LoadedConfig> {
    let path = init_config()?;
    load_from_with_overrides(
        &path,
        &EnvOverrides::from_process(),
        &ConfigOverrides::from_process()?,
    )
}

pub fn load_from(path: &Path, env: &EnvOverrides) -> Result<LoadedConfig> {
    load_from_with_overrides(path, env, &ConfigOverrides::default())
}

pub fn load_from_with_overrides(
    path: &Path,
    env: &EnvOverrides,
    overrides: &ConfigOverrides,
) -> Result<LoadedConfig> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(ConfigError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    load_str_with_overrides(path, &contents, env, overrides)
}

pub fn load_str(path: &Path, contents: &str, env: &EnvOverrides) -> Result<LoadedConfig> {
    load_str_with_overrides(path, contents, env, &ConfigOverrides::default())
}

pub fn load_str_with_overrides(
    path: &Path,
    contents: &str,
    env: &EnvOverrides,
    overrides: &ConfigOverrides,
) -> Result<LoadedConfig> {
    let document = if contents.trim().is_empty() {
        DocumentMut::new()
    } else {
        contents
            .parse::<DocumentMut>()
            .map_err(|_| ConfigError::EditParse {
                path: path.to_path_buf(),
            })?
    };
    let mut root = if contents.trim().is_empty() {
        toml::Table::new()
    } else {
        toml::from_str::<toml::Table>(contents).map_err(|_| ConfigError::Parse {
            path: path.to_path_buf(),
        })?
    };
    overrides.apply(&mut root)?;
    let mut generic = toml::Value::Table(root.clone())
        .try_into::<GenericSections>()
        .map_err(|_| ConfigError::Parse {
            path: path.to_path_buf(),
        })?;
    if generic.analytics.hook_command.is_none() {
        generic.analytics.hook_command = root
            .get("player")
            .and_then(toml::Value::as_table)
            .and_then(|player| player.get("event_hook"))
            .and_then(toml::Value::as_str)
            .map(ToOwned::to_owned);
    }

    validate_generic(&generic)?;
    let (default_provider, providers, mut warnings) = resolve_providers(&document, &root, env)?;
    warnings.sort_by_key(ToString::to_string);
    warnings.dedup();
    let config = AppConfig {
        path: path.to_path_buf(),
        default_provider,
        providers,
        cache: generic.cache.normalize(),
        analytics: generic.analytics.normalize(env),
        notifications: generic.notifications.normalize(),
        discord: generic.discord.normalize(),
        viz: generic.viz.normalize(),
    };
    let effective = effective_table(&config)?;
    Ok(LoadedConfig {
        config,
        warnings,
        effective,
    })
}

fn resolve_providers(
    document: &DocumentMut,
    root: &toml::Table,
    env: &EnvOverrides,
) -> Result<(Option<ProviderId>, Vec<ProviderEntry>, Vec<ConfigWarning>)> {
    let mut warnings = Vec::new();
    let mut provider_values = match root.get("providers") {
        None => toml::Table::new(),
        Some(toml::Value::Table(table)) => table.clone(),
        Some(_) => {
            return Err(ConfigError::Invalid(
                "providers must be a table".to_string(),
            ));
        }
    };
    let default_provider = provider_values
        .remove("default")
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| ConfigError::Invalid("providers.default must be a string".into()))
                .and_then(provider_id)
        })
        .transpose()?;

    let legacy_present = has_legacy_spotify_config(root);
    let env_present = env.spotify_client_id.is_some()
        || env.spotify_client_secret.is_some()
        || env.spotify_redirect_uri.is_some();
    if legacy_present {
        let configured_spotify_ids = provider_values
            .iter()
            .filter(|(_, value)| {
                value
                    .as_table()
                    .and_then(|table| table.get("type"))
                    .and_then(toml::Value::as_str)
                    .is_some_and(|kind| kind == "spotify")
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        let target = if let Some(value) = provider_values.get("spotify") {
            let table = value.as_table().ok_or_else(|| {
                ConfigError::Invalid("providers.spotify must be a table".to_string())
            })?;
            if table
                .get("type")
                .and_then(toml::Value::as_str)
                .is_some_and(|kind| kind != "spotify")
            {
                return Err(ConfigError::Invalid(
                    "legacy Spotify keys conflict with non-Spotify provider `spotify`".to_string(),
                ));
            }
            "spotify".to_string()
        } else {
            match configured_spotify_ids.as_slice() {
                [only] => only.clone(),
                [] => "spotify".to_string(),
                _ => {
                    return Err(ConfigError::Invalid(
                        "legacy Spotify keys are ambiguous across configured Spotify providers"
                            .to_string(),
                    ));
                }
            }
        };
        let mut spotify = match provider_values.remove(&target) {
            Some(toml::Value::Table(table)) => table,
            Some(_) => {
                return Err(ConfigError::Invalid(format!(
                    "providers.{target} must be a table"
                )));
            }
            None => toml::Table::new(),
        };

        merge_legacy_root_key(root, &mut spotify, &target, "client_id", &mut warnings);
        merge_legacy_root_key(root, &mut spotify, &target, "client_secret", &mut warnings);
        merge_legacy_root_key(root, &mut spotify, &target, "redirect_uri", &mut warnings);
        merge_legacy_player(root, &mut spotify, &target, &mut warnings)?;
        merge_legacy_spotifyd(root, &mut spotify, &target, &mut warnings)?;
        if spotify.get("type").is_none() && legacy_present {
            spotify.insert(
                "type".to_string(),
                toml::Value::String("spotify".to_string()),
            );
        }
        provider_values.insert(target, toml::Value::Table(spotify));
    }

    if env_present {
        if provider_values.is_empty() {
            let mut spotify = toml::Table::new();
            spotify.insert(
                "type".to_string(),
                toml::Value::String("spotify".to_string()),
            );
            provider_values.insert("spotify".to_string(), toml::Value::Table(spotify));
        }
        let spotify_ids = provider_values
            .iter()
            .filter(|(_, value)| {
                value
                    .as_table()
                    .and_then(|table| table.get("type"))
                    .and_then(toml::Value::as_str)
                    .is_some_and(|kind| kind == "spotify")
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        let default_spotify = default_provider.as_ref().and_then(|default| {
            provider_values
                .get(default.as_str())
                .and_then(toml::Value::as_table)
                .and_then(|table| table.get("type"))
                .and_then(toml::Value::as_str)
                .is_some_and(|kind| kind == "spotify")
                .then(|| default.to_string())
        });
        let target = default_spotify
            .or_else(|| {
                provider_values
                    .get("spotify")
                    .and_then(toml::Value::as_table)
                    .and_then(|table| table.get("type"))
                    .and_then(toml::Value::as_str)
                    .is_some_and(|kind| kind == "spotify")
                    .then(|| "spotify".to_string())
            })
            .or_else(|| (spotify_ids.len() == 1).then(|| spotify_ids[0].clone()))
            .ok_or_else(|| {
                if spotify_ids.len() > 1 {
                    ConfigError::Invalid(
                        "Spotify environment overrides are ambiguous; set providers.default"
                            .to_string(),
                    )
                } else {
                    ConfigError::Invalid(
                        "Spotify environment overrides require a configured Spotify provider"
                            .to_string(),
                    )
                }
            })?;
        let spotify = provider_values
            .get_mut(&target)
            .and_then(toml::Value::as_table_mut)
            .ok_or_else(|| ConfigError::Invalid(format!("providers.{target} must be a table")))?;
        apply_env_override(spotify, "client_id", env.spotify_client_id.as_ref());
        apply_env_override(spotify, "client_secret", env.spotify_client_secret.as_ref());
        apply_env_override(spotify, "redirect_uri", env.spotify_redirect_uri.as_ref());
    }

    let mut order = document
        .get("providers")
        .and_then(toml_edit::Item::as_table)
        .map(|providers| {
            providers
                .iter()
                .filter(|(_, item)| item.is_table())
                .map(|(key, _)| key.to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for id in provider_values.keys() {
        if !order.iter().any(|existing| existing == id) {
            order.push(id.clone());
        }
    }

    let mut providers = Vec::with_capacity(provider_values.len());
    for id in order {
        let Some(value) = provider_values.remove(&id) else {
            continue;
        };
        let raw = value.as_table().cloned().ok_or_else(|| {
            ConfigError::Invalid(format!("providers.{id} must be a provider table"))
        })?;
        let provider_id = provider_id(&id)?;
        let kind = raw
            .get("type")
            .and_then(toml::Value::as_str)
            .and_then(|value| nonblank(Some(value.to_string())))
            .ok_or_else(|| ConfigError::Invalid(format!("providers.{id}.type is required")))?;
        if kind == "spotify" {
            if let Some(player) = raw.get("player") {
                let player = player.clone().try_into::<PlayerSettings>().map_err(|_| {
                    ConfigError::ProviderDecode {
                        provider: provider_id.clone(),
                    }
                })?;
                player.validate()?;
            }
        }
        providers.push(ProviderEntry {
            id: provider_id,
            kind,
            raw,
        });
    }

    let spotify_providers = providers
        .iter()
        .filter(|provider| provider.kind == "spotify")
        .map(|provider| provider.id.as_str())
        .collect::<Vec<_>>();
    if spotify_providers.len() > 1 {
        return Err(ConfigError::Invalid(format!(
            "URI scheme `spotify` cannot be owned by multiple providers: {}",
            spotify_providers.join(", ")
        )));
    }

    if let Some(default) = default_provider.as_ref() {
        if !providers.iter().any(|provider| &provider.id == default) {
            return Err(ConfigError::Invalid(format!(
                "providers.default references missing provider `{default}`"
            )));
        }
    }

    let effective_default = match (default_provider, providers.as_slice()) {
        (Some(default), _) => Some(default),
        (None, []) => None,
        (None, [only]) => Some(only.id.clone()),
        (None, _) => {
            return Err(ConfigError::Invalid(
                "providers.default is required when multiple providers are configured".to_string(),
            ));
        }
    };
    Ok((effective_default, providers, warnings))
}

fn set_toml_path(root: &mut toml::Table, path: &ConfigPath, value: toml::Value) -> Result<()> {
    let segments = path.segments().collect::<Vec<_>>();
    let Some((leaf, parents)) = segments.split_last() else {
        return Err(ConfigError::Invalid(
            "config override path is empty".to_string(),
        ));
    };
    let mut table = root;
    for segment in parents {
        if !table.contains_key(*segment) {
            table.insert(
                (*segment).to_string(),
                toml::Value::Table(toml::Table::new()),
            );
        }
        table = table
            .get_mut(*segment)
            .and_then(toml::Value::as_table_mut)
            .ok_or_else(|| {
                ConfigError::Invalid(format!(
                    "config override `{}` crosses non-table `{segment}`",
                    path.canonical()
                ))
            })?;
    }
    table.insert((*leaf).to_string(), value);
    Ok(())
}

fn validate_generic(generic: &GenericSections) -> Result<()> {
    if generic.analytics.daily_rollup_hour > 23 {
        return Err(ConfigError::Invalid(
            "analytics.daily_rollup_hour must be between 0 and 23".to_string(),
        ));
    }
    if generic.analytics.hook_timeout_ms == 0 {
        return Err(ConfigError::Invalid(
            "analytics.hook_timeout_ms must be greater than zero".to_string(),
        ));
    }
    if !(1..=60).contains(&generic.viz.target_fps) {
        return Err(ConfigError::Invalid(
            "viz.target_fps must be between 1 and 60".to_string(),
        ));
    }
    if !(0.0..=0.95).contains(&generic.viz.smoothing)
        || !(0.0..=1.0).contains(&generic.viz.noise_gate)
    {
        return Err(ConfigError::Invalid(
            "visualizer numeric setting is outside its supported range".to_string(),
        ));
    }
    if !matches!(
        generic.viz.source.as_str(),
        "auto" | "sink" | "loopback" | "none"
    ) {
        return Err(ConfigError::Invalid(
            "viz.source must be one of auto, sink, loopback, none".to_string(),
        ));
    }
    if !matches!(
        generic.viz.color_scheme.as_str(),
        "spotify-green" | "rainbow" | "monochrome"
    ) {
        return Err(ConfigError::Invalid(
            "viz.color_scheme must be one of spotify-green, rainbow, monochrome".to_string(),
        ));
    }
    Ok(())
}

fn effective_table(config: &AppConfig) -> Result<toml::Table> {
    let mut root = toml::Table::new();
    let mut providers = toml::Table::new();
    if let Some(default) = &config.default_provider {
        providers.insert(
            "default".to_string(),
            toml::Value::String(default.to_string()),
        );
    }
    for provider in &config.providers {
        providers.insert(
            provider.id.to_string(),
            toml::Value::Table(provider.raw.clone()),
        );
    }
    if !providers.is_empty() {
        root.insert("providers".to_string(), toml::Value::Table(providers));
    }
    for (name, value) in [
        ("cache", toml::Value::try_from(config.cache.clone())),
        ("analytics", toml::Value::try_from(config.analytics.clone())),
        (
            "notifications",
            toml::Value::try_from(config.notifications.clone()),
        ),
        ("discord", toml::Value::try_from(config.discord.clone())),
        ("viz", toml::Value::try_from(config.viz.clone())),
    ] {
        root.insert(
            name.to_string(),
            value.map_err(|error| ConfigError::Invalid(error.to_string()))?,
        );
    }
    Ok(root)
}

fn provider_id(value: impl Into<String>) -> Result<ProviderId> {
    ProviderId::new(value).map_err(|error| ConfigError::Invalid(error.to_string()))
}

fn has_legacy_spotify_config(root: &toml::Table) -> bool {
    const PLAYER_KEYS: &[&str] = &[
        "backend",
        "bitrate",
        "device_name",
        "audio_output_device",
        "normalization",
        "audio_cache_mib",
        "pulse_props",
        "event_hook",
    ];
    ["client_id", "client_secret", "redirect_uri"]
        .iter()
        .any(|key| root.contains_key(*key))
        || root
            .get("player")
            .and_then(toml::Value::as_table)
            .is_some_and(|player| PLAYER_KEYS.iter().any(|key| player.contains_key(*key)))
        || root
            .get("spotifyd")
            .and_then(toml::Value::as_table)
            .is_some_and(|spotifyd| spotifyd.contains_key("device_name"))
}

fn merge_legacy_root_key(
    root: &toml::Table,
    spotify: &mut toml::Table,
    provider_id: &str,
    key: &str,
    warnings: &mut Vec<ConfigWarning>,
) {
    let Some(value) = root.get(key) else {
        return;
    };
    let ignored = spotify.contains_key(key);
    warnings.push(ConfigWarning::DeprecatedKey {
        legacy_path: key.to_string(),
        canonical_path: format!("providers.{provider_id}.{key}"),
        ignored,
    });
    if !ignored {
        spotify.insert(key.to_string(), value.clone());
    }
}

fn merge_legacy_player(
    root: &toml::Table,
    spotify: &mut toml::Table,
    provider_id: &str,
    warnings: &mut Vec<ConfigWarning>,
) -> Result<()> {
    let Some(value) = root.get("player") else {
        return Ok(());
    };
    let legacy = value
        .as_table()
        .ok_or_else(|| ConfigError::Invalid("player must be a table".to_string()))?;
    let player = ensure_toml_table(spotify, "player")?;
    const PLAYER_KEYS: &[&str] = &[
        "backend",
        "bitrate",
        "device_name",
        "audio_output_device",
        "normalization",
        "audio_cache_mib",
        "pulse_props",
        "event_hook",
    ];
    for (key, value) in legacy {
        if !PLAYER_KEYS.contains(&key.as_str()) {
            continue;
        }
        let canonical = if key == "event_hook" {
            "analytics.hook_command".to_string()
        } else {
            format!("providers.{provider_id}.player.{key}")
        };
        let ignored = if key == "event_hook" {
            root.get("analytics")
                .and_then(toml::Value::as_table)
                .is_some_and(|analytics| analytics.contains_key("hook_command"))
        } else {
            player.contains_key(key)
        };
        warnings.push(ConfigWarning::DeprecatedKey {
            legacy_path: format!("player.{key}"),
            canonical_path: canonical,
            ignored,
        });
        if key != "event_hook" && !ignored {
            player.insert(key.clone(), value.clone());
        }
    }
    Ok(())
}

fn merge_legacy_spotifyd(
    root: &toml::Table,
    spotify: &mut toml::Table,
    provider_id: &str,
    warnings: &mut Vec<ConfigWarning>,
) -> Result<()> {
    let Some(value) = root.get("spotifyd") else {
        return Ok(());
    };
    let legacy = value
        .as_table()
        .ok_or_else(|| ConfigError::Invalid("spotifyd must be a table".to_string()))?;
    let Some(device_name) = legacy.get("device_name") else {
        return Ok(());
    };
    let player = ensure_toml_table(spotify, "player")?;
    let ignored = player.contains_key("device_name");
    warnings.push(ConfigWarning::DeprecatedKey {
        legacy_path: "spotifyd.device_name".to_string(),
        canonical_path: format!("providers.{provider_id}.player.device_name"),
        ignored,
    });
    if !ignored {
        player.insert("device_name".to_string(), device_name.clone());
    }
    Ok(())
}

fn ensure_toml_table<'a>(table: &'a mut toml::Table, key: &str) -> Result<&'a mut toml::Table> {
    if !table.contains_key(key) {
        table.insert(key.to_string(), toml::Value::Table(toml::Table::new()));
    }
    table
        .get_mut(key)
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| ConfigError::Invalid(format!("providers.spotify.{key} must be a table")))
}

fn apply_env_override(table: &mut toml::Table, key: &str, value: Option<&String>) {
    if let Some(value) = value.and_then(|value| nonblank(Some(value.clone()))) {
        table.insert(key.to_string(), toml::Value::String(value));
    }
}

fn nonblank(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_string();
        (!value.is_empty()).then_some(value)
    })
}
