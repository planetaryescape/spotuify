use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use spotuify_core::BackendKind;

use crate::error::{SpotifyError, SpotifyResult};

#[derive(Clone, Debug)]
pub struct Config {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    pub config_path: PathBuf,
    pub player: PlayerConfig,
    pub cache: CacheConfig,
    pub analytics: AnalyticsConfig,
    pub notifications: NotificationsConfig,
    pub discord: DiscordConfig,
    /// Phase 17 — visualization config. Default-off; users opt in via
    /// `[viz] enabled = true`.
    pub viz: VizConfig,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct NotificationsSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    on_track_change: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    on_pause: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    on_resume: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    on_skip: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    on_error: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
    pub(crate) fn from_file(file: &FileConfig) -> Self {
        let section = file.notifications.clone().unwrap_or_default();
        let defaults = Self::default();
        Self {
            enabled: section.enabled.unwrap_or(defaults.enabled),
            summary: blank_to_none(section.summary).unwrap_or(defaults.summary),
            body: blank_to_none(section.body).unwrap_or(defaults.body),
            on_track_change: section.on_track_change.unwrap_or(defaults.on_track_change),
            on_pause: section.on_pause.unwrap_or(defaults.on_pause),
            on_resume: section.on_resume.unwrap_or(defaults.on_resume),
            on_skip: section.on_skip.unwrap_or(defaults.on_skip),
            on_error: section.on_error.unwrap_or(defaults.on_error),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct DiscordSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    application_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiscordConfig {
    pub enabled: bool,
    pub application_id: Option<String>,
}

impl DiscordConfig {
    pub(crate) fn from_file(file: &FileConfig) -> Self {
        let section = file.discord.clone().unwrap_or_default();
        Self {
            enabled: section.enabled.unwrap_or(false),
            application_id: blank_to_none(section.application_id),
        }
    }
}

/// TOML-side representation of the `[analytics]` section. All fields
/// optional so partial sections work.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct AnalyticsSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    store_raw_queries: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retention_progress_days: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retention_events_days: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retention_operations_days: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    daily_rollup_hour: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hook_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hook_timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allow_file_credentials: Option<bool>,
}

/// Phase 10 analytics + Phase 11 headless-Linux flag. Defaults match
/// blueprint values; users can override per-key via TOML.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalyticsConfig {
    /// When false, raw search queries are dropped on persistence
    /// (only the normalised query hash is kept). Default: true.
    pub store_raw_queries: bool,
    /// Days to retain raw `playback_progress` samples before prune.
    pub retention_progress_days: u32,
    /// Days to retain `analytics_events` before prune.
    pub retention_events_days: u32,
    /// Days to retain `operations` rows before prune.
    pub retention_operations_days: u32,
    /// Local hour (0..=23) at which the daily habit rollup runs.
    pub daily_rollup_hour: u8,
    /// Optional shell command fired on `listen_qualified` events;
    /// bridges to ListenBrainz / Last.fm / Discord recipes.
    pub hook_command: Option<String>,
    /// Hard timeout on `hook_command` execution to keep the daemon
    /// from blocking on a misbehaving scrobbler.
    pub hook_timeout_ms: u64,
    /// Phase 11 headless-Linux opt-in: when true and Secret Service
    /// is unavailable, fall back to an age-encrypted credentials file.
    pub allow_file_credentials: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct CacheSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    cover_cache_mb: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cover_cache_ttl_days: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
    pub(crate) fn from_file(file: &FileConfig) -> Self {
        let section = file.cache.clone().unwrap_or_default();
        let defaults = Self::default();
        Self {
            cover_cache_mb: section.cover_cache_mb.unwrap_or(defaults.cover_cache_mb),
            cover_cache_ttl_days: section
                .cover_cache_ttl_days
                .filter(|days| *days > 0)
                .unwrap_or(defaults.cover_cache_ttl_days),
        }
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
        }
    }
}

impl AnalyticsConfig {
    pub(crate) fn from_file(file: &FileConfig) -> Self {
        let section = file.analytics.clone().unwrap_or_default();
        let defaults = Self::default();
        Self {
            store_raw_queries: section
                .store_raw_queries
                .unwrap_or(defaults.store_raw_queries),
            retention_progress_days: section
                .retention_progress_days
                .unwrap_or(defaults.retention_progress_days),
            retention_events_days: section
                .retention_events_days
                .unwrap_or(defaults.retention_events_days),
            retention_operations_days: section
                .retention_operations_days
                .unwrap_or(defaults.retention_operations_days),
            daily_rollup_hour: section
                .daily_rollup_hour
                .filter(|h| *h <= 23)
                .unwrap_or(defaults.daily_rollup_hour),
            hook_command: blank_to_none(section.hook_command),
            hook_timeout_ms: section.hook_timeout_ms.unwrap_or(defaults.hook_timeout_ms),
            allow_file_credentials: section
                .allow_file_credentials
                .unwrap_or(defaults.allow_file_credentials),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct FileConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    redirect_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    player: Option<PlayerSection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache: Option<CacheSection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    analytics: Option<AnalyticsSection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notifications: Option<NotificationsSection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    discord: Option<DiscordSection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    viz: Option<VizSection>,
}

/// Phase 17 — TOML representation of the `[viz]` section. All fields
/// optional; `VizConfig::from_file` fills defaults.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub(crate) struct VizSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_fps: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    smoothing: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    noise_gate: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    color_scheme: Option<String>,
}

/// Phase 17 — fully-resolved `[viz]` config.
#[derive(Clone, Debug, PartialEq)]
pub struct VizConfig {
    pub enabled: bool,
    /// One of "auto", "sink", "loopback", "none". Validated; unknown
    /// strings fall back to "auto".
    pub source: String,
    /// Target FPS for the FFT ticker. Clamped to [1, 60]. Default 30.
    pub target_fps: u8,
    /// EMA smoothing factor 0.0..=0.95. Default 0.5.
    pub smoothing: f32,
    /// Noise gate threshold 0.0..=1.0. Default 0.005.
    pub noise_gate: f32,
    /// One of "spotify-green", "rainbow", "monochrome". Default
    /// "spotify-green".
    pub color_scheme: String,
}

impl Default for VizConfig {
    fn default() -> Self {
        Self {
            // Visualizer is part of the player identity; ship it ON.
            // Users on a Connect-only backend won't see bars move
            // (no PCM samples) but the spectrum area still draws a
            // flat baseline so the layout doesn't shift. Disable
            // explicitly with `[viz] enabled = false`.
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
    pub(crate) fn from_file(file: &FileConfig) -> Self {
        let section = file.viz.clone().unwrap_or_default();
        let mut cfg = Self::default();
        if let Some(v) = section.enabled {
            cfg.enabled = v;
        }
        if let Some(s) = section.source.filter(|s| !s.trim().is_empty()) {
            let lower = s.trim().to_ascii_lowercase();
            cfg.source = match lower.as_str() {
                "auto" | "sink" | "loopback" | "none" => lower,
                _ => "auto".to_string(),
            };
        }
        if let Some(fps) = section.target_fps {
            cfg.target_fps = fps.clamp(1, 60);
        }
        if let Some(sm) = section.smoothing {
            cfg.smoothing = sm.clamp(0.0, 0.95);
        }
        if let Some(g) = section.noise_gate {
            cfg.noise_gate = g.clamp(0.0, 1.0);
        }
        if let Some(c) = section.color_scheme.filter(|s| !s.trim().is_empty()) {
            let lower = c.trim().to_ascii_lowercase();
            cfg.color_scheme = match lower.as_str() {
                "spotify-green" | "rainbow" | "monochrome" => lower,
                _ => "spotify-green".to_string(),
            };
        }
        cfg
    }
}

/// TOML-side representation of the `[player]` section. All fields are
/// Optional so the section can be partially specified; defaults apply
/// in `PlayerConfig::from_file`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct PlayerSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bitrate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    normalization: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    audio_cache_mib: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pulse_props: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    event_hook: Option<String>,
}

/// Fully-resolved `[player]` config with defaults filled in. The
/// daemon, CLI, and player crate all consume this shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerConfig {
    pub backend: BackendKind,
    pub bitrate: u32,
    pub device_name: Option<String>,
    pub normalization: bool,
    pub audio_cache_mib: u32,
    pub pulse_props: bool,
    pub event_hook: Option<String>,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            backend: BackendKind::default(),
            bitrate: 320,
            device_name: None,
            normalization: false,
            audio_cache_mib: 0,
            pulse_props: true,
            event_hook: None,
        }
    }
}

impl PlayerConfig {
    /// Lift a `[player]` section into a fully-defaulted PlayerConfig.
    /// Falls back to all defaults if the section is missing entirely.
    /// Invalid values are *not* rejected here (use `validate` for
    /// load-time checks); they degrade silently to defaults so a typo
    /// in `event_hook` can't brick the daemon.
    pub(crate) fn from_file(file: &FileConfig) -> Self {
        let section = file.player.clone().unwrap_or_default();
        let backend = section
            .backend
            .as_deref()
            .and_then(|raw| BackendKind::parse(raw).ok())
            .unwrap_or_default();
        let bitrate = section
            .bitrate
            .filter(|b| matches!(b, 96 | 160 | 320))
            .unwrap_or(320);
        Self {
            backend,
            bitrate,
            device_name: blank_to_none(section.device_name),
            normalization: section.normalization.unwrap_or(false),
            audio_cache_mib: section.audio_cache_mib.unwrap_or(0),
            pulse_props: section.pulse_props.unwrap_or(true),
            event_hook: blank_to_none(section.event_hook),
        }
    }

    /// Validate a `[player]` section without mutating state. Returns
    /// the first error encountered — used by `Config::load` so users
    /// see config bugs at startup rather than as silent fallbacks.
    pub(crate) fn validate(file: &FileConfig) -> Result<()> {
        let Some(section) = file.player.as_ref() else {
            return Ok(());
        };
        if let Some(raw) = section.backend.as_deref() {
            BackendKind::parse(raw)
                .map_err(|err| anyhow!("config player.backend invalid: {err}"))?;
        }
        if let Some(bitrate) = section.bitrate {
            if !matches!(bitrate, 96 | 160 | 320) {
                bail!("config player.bitrate invalid: {bitrate} (expected one of 96, 160, 320)");
            }
        }
        Ok(())
    }
}

impl From<PlayerConfig> for PlayerSection {
    fn from(value: PlayerConfig) -> Self {
        Self {
            backend: Some(value.backend.label().to_string()),
            bitrate: Some(value.bitrate),
            device_name: value.device_name,
            normalization: Some(value.normalization),
            audio_cache_mib: Some(value.audio_cache_mib),
            pulse_props: Some(value.pulse_props),
            event_hook: value.event_hook,
        }
    }
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            client_id: None,
            client_secret: None,
            redirect_uri: None,
            player: None,
            cache: None,
            analytics: None,
            notifications: None,
            discord: None,
            viz: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigKey {
    ClientId,
    ClientSecret,
    RedirectUri,
    // Phase 9 — player backend.
    PlayerBackend,
    PlayerBitrate,
    PlayerDeviceName,
    PlayerNormalization,
    PlayerAudioCacheMib,
    PlayerPulseProps,
    PlayerEventHook,
    AnalyticsHookCommand,
    AnalyticsHookTimeoutMs,
    CacheCoverCacheMb,
    CacheCoverCacheTtlDays,
    NotificationsEnabled,
    NotificationsSummary,
    NotificationsBody,
    NotificationsOnTrackChange,
    NotificationsOnPause,
    NotificationsOnResume,
    NotificationsOnSkip,
    NotificationsOnError,
}

impl ConfigKey {
    pub fn parse(value: &str) -> SpotifyResult<Self> {
        match value {
            "client_id" | "client-id" => Ok(Self::ClientId),
            "client_secret" | "client-secret" => Ok(Self::ClientSecret),
            "redirect_uri" | "redirect-uri" => Ok(Self::RedirectUri),
            "player.backend" => Ok(Self::PlayerBackend),
            "player.bitrate" => Ok(Self::PlayerBitrate),
            "player.device_name" | "player.device-name" => Ok(Self::PlayerDeviceName),
            "player.normalization" => Ok(Self::PlayerNormalization),
            "player.audio_cache_mib" | "player.audio-cache-mib" => Ok(Self::PlayerAudioCacheMib),
            "player.pulse_props" | "player.pulse-props" => Ok(Self::PlayerPulseProps),
            "player.event_hook" | "player.event-hook" => Ok(Self::PlayerEventHook),
            "analytics.hook_command" | "analytics.hook-command" => Ok(Self::AnalyticsHookCommand),
            "analytics.hook_timeout_ms" | "analytics.hook-timeout-ms" => {
                Ok(Self::AnalyticsHookTimeoutMs)
            }
            "cache.cover_cache_mb" | "cache.cover-cache-mb" => Ok(Self::CacheCoverCacheMb),
            "cache.cover_cache_ttl_days" | "cache.cover-cache-ttl-days" => {
                Ok(Self::CacheCoverCacheTtlDays)
            }
            "notifications.enabled" => Ok(Self::NotificationsEnabled),
            "notifications.summary" => Ok(Self::NotificationsSummary),
            "notifications.body" => Ok(Self::NotificationsBody),
            "notifications.on_track_change" | "notifications.on-track-change" => {
                Ok(Self::NotificationsOnTrackChange)
            }
            "notifications.on_pause" | "notifications.on-pause" => Ok(Self::NotificationsOnPause),
            "notifications.on_resume" | "notifications.on-resume" => {
                Ok(Self::NotificationsOnResume)
            }
            "notifications.on_skip" | "notifications.on-skip" => Ok(Self::NotificationsOnSkip),
            "notifications.on_error" | "notifications.on-error" => Ok(Self::NotificationsOnError),
            _ => Err(SpotifyError::InvalidInput {
                message: format!(
                    "unknown config key `{value}`; expected one of: {}",
                    Self::valid_keys().join(", ")
                ),
            }),
        }
    }

    pub fn valid_keys() -> &'static [&'static str] {
        &[
            "client_id",
            "client_secret",
            "redirect_uri",
            "player.backend",
            "player.bitrate",
            "player.device_name",
            "player.normalization",
            "player.audio_cache_mib",
            "player.pulse_props",
            "player.event_hook",
            "analytics.hook_command",
            "analytics.hook_timeout_ms",
            "cache.cover_cache_mb",
            "cache.cover_cache_ttl_days",
            "notifications.enabled",
            "notifications.summary",
            "notifications.body",
            "notifications.on_track_change",
            "notifications.on_pause",
            "notifications.on_resume",
            "notifications.on_skip",
            "notifications.on_error",
        ]
    }
}

impl Config {
    pub fn load() -> SpotifyResult<Self> {
        let config_path = config_path()?;
        ensure_config_exists(&config_path)?;
        // Phase 13 (P13-G) — every load is a chance to drop the
        // .gitignore. Idempotent (only writes when absent), so it's
        // safe to call on every invocation.
        if let Some(parent) = config_path.parent() {
            write_gitignore_if_absent(parent);
        }

        let file = read_config_file(&config_path)?;
        PlayerConfig::validate(&file)
            .with_context(|| format!("invalid [player] section in {}", config_path.display()))?;
        let player = PlayerConfig::from_file(&file);
        let cache = CacheConfig::from_file(&file);
        let analytics = AnalyticsConfig::from_file(&file);
        let notifications = NotificationsConfig::from_file(&file);
        let discord = DiscordConfig::from_file(&file);
        let viz = VizConfig::from_file(&file);

        let client_id = std::env::var("SPOTUIFY_CLIENT_ID")
            .ok()
            .or(file.client_id)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow!("client_id missing in {}", config_path.display()))?;
        let client_secret = std::env::var("SPOTUIFY_CLIENT_SECRET")
            .ok()
            .or(file.client_secret)
            .filter(|value| !value.trim().is_empty());
        let redirect_uri = std::env::var("SPOTUIFY_REDIRECT_URI")
            .ok()
            .or(file.redirect_uri)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(default_redirect_uri);

        Ok(Self {
            client_id,
            client_secret,
            redirect_uri,
            config_path,
            player,
            cache,
            analytics,
            notifications,
            discord,
            viz,
        })
    }

    pub fn redacted_client_id(&self) -> String {
        let len = self.client_id.chars().count();
        if len <= 8 {
            return "present".to_string();
        }

        let start: String = self.client_id.chars().take(4).collect();
        let end: String = self
            .client_id
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("{start}...{end}")
    }
}

pub fn config_path() -> SpotifyResult<PathBuf> {
    if let Some(path) = std::env::var_os("SPOTUIFY_CONFIG") {
        return Ok(PathBuf::from(path));
    }

    Ok(dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .map(|dir| dir.join("spotuify/spotuify.toml"))
        .ok_or_else(|| anyhow!("could not resolve config directory"))?)
}

pub fn init_config() -> SpotifyResult<PathBuf> {
    let path = config_path()?;
    if !path.exists() {
        write_template(&path)?;
    }
    Ok(path)
}

pub fn get_config_value(key: ConfigKey) -> SpotifyResult<Option<String>> {
    let path = config_path()?;
    let file = if path.exists() {
        read_config_file(&path)?
    } else {
        FileConfig::default()
    };

    let resolved = PlayerConfig::from_file(&file);
    let resolved_cache = CacheConfig::from_file(&file);
    let resolved_analytics = AnalyticsConfig::from_file(&file);
    let resolved_notifications = NotificationsConfig::from_file(&file);

    Ok(match key {
        ConfigKey::ClientId => blank_to_none(file.client_id),
        ConfigKey::ClientSecret => blank_to_none(file.client_secret),
        ConfigKey::RedirectUri => {
            blank_to_none(file.redirect_uri).or_else(|| Some(default_redirect_uri()))
        }
        ConfigKey::PlayerBackend => Some(resolved.backend.label().to_string()),
        ConfigKey::PlayerBitrate => Some(resolved.bitrate.to_string()),
        ConfigKey::PlayerDeviceName => resolved.device_name,
        ConfigKey::PlayerNormalization => Some(resolved.normalization.to_string()),
        ConfigKey::PlayerAudioCacheMib => Some(resolved.audio_cache_mib.to_string()),
        ConfigKey::PlayerPulseProps => Some(resolved.pulse_props.to_string()),
        ConfigKey::PlayerEventHook => resolved.event_hook,
        ConfigKey::AnalyticsHookCommand => resolved_analytics.hook_command,
        ConfigKey::AnalyticsHookTimeoutMs => Some(resolved_analytics.hook_timeout_ms.to_string()),
        ConfigKey::CacheCoverCacheMb => Some(resolved_cache.cover_cache_mb.to_string()),
        ConfigKey::CacheCoverCacheTtlDays => Some(resolved_cache.cover_cache_ttl_days.to_string()),
        ConfigKey::NotificationsEnabled => Some(resolved_notifications.enabled.to_string()),
        ConfigKey::NotificationsSummary => Some(resolved_notifications.summary),
        ConfigKey::NotificationsBody => Some(resolved_notifications.body),
        ConfigKey::NotificationsOnTrackChange => {
            Some(resolved_notifications.on_track_change.to_string())
        }
        ConfigKey::NotificationsOnPause => Some(resolved_notifications.on_pause.to_string()),
        ConfigKey::NotificationsOnResume => Some(resolved_notifications.on_resume.to_string()),
        ConfigKey::NotificationsOnSkip => Some(resolved_notifications.on_skip.to_string()),
        ConfigKey::NotificationsOnError => Some(resolved_notifications.on_error.to_string()),
    })
}

pub fn set_config_value(key: ConfigKey, value: &str) -> SpotifyResult<PathBuf> {
    let path = init_config()?;
    let mut file = read_config_file(&path)?;

    match key {
        ConfigKey::ClientId => file.client_id = blank_to_none(Some(value.to_string())),
        ConfigKey::ClientSecret => file.client_secret = blank_to_none(Some(value.to_string())),
        ConfigKey::RedirectUri => file.redirect_uri = blank_to_none(Some(value.to_string())),
        ConfigKey::PlayerBackend => {
            let parsed = BackendKind::parse(value)
                .map_err(|err| anyhow!("invalid value for player.backend: {err}"))?;
            player_section_mut(&mut file).backend = Some(parsed.label().to_string());
        }
        ConfigKey::PlayerBitrate => {
            let parsed: u32 = value.trim().parse().with_context(|| {
                format!("expected an integer for player.bitrate, got `{value}`")
            })?;
            if !matches!(parsed, 96 | 160 | 320) {
                return Err(SpotifyError::InvalidInput {
                    message: format!("player.bitrate must be one of 96, 160, 320 (got `{parsed}`)"),
                });
            }
            player_section_mut(&mut file).bitrate = Some(parsed);
        }
        ConfigKey::PlayerDeviceName => {
            player_section_mut(&mut file).device_name = blank_to_none(Some(value.to_string()));
        }
        ConfigKey::PlayerNormalization => {
            player_section_mut(&mut file).normalization = Some(parse_bool(value)?);
        }
        ConfigKey::PlayerAudioCacheMib => {
            let parsed: u32 = value.trim().parse().with_context(|| {
                format!("expected a non-negative integer for player.audio_cache_mib, got `{value}`")
            })?;
            player_section_mut(&mut file).audio_cache_mib = Some(parsed);
        }
        ConfigKey::PlayerPulseProps => {
            player_section_mut(&mut file).pulse_props = Some(parse_bool(value)?);
        }
        ConfigKey::PlayerEventHook => {
            player_section_mut(&mut file).event_hook = blank_to_none(Some(value.to_string()));
        }
        ConfigKey::AnalyticsHookCommand => {
            analytics_section_mut(&mut file).hook_command = blank_to_none(Some(value.to_string()));
        }
        ConfigKey::AnalyticsHookTimeoutMs => {
            let parsed: u64 = value.trim().parse().with_context(|| {
                format!("expected a positive integer for analytics.hook_timeout_ms, got `{value}`")
            })?;
            if parsed == 0 {
                return Err(SpotifyError::InvalidInput {
                    message: "analytics.hook_timeout_ms must be greater than 0".to_string(),
                });
            }
            analytics_section_mut(&mut file).hook_timeout_ms = Some(parsed);
        }
        ConfigKey::CacheCoverCacheMb => {
            let parsed: u64 = value.trim().parse().with_context(|| {
                format!("expected a non-negative integer for cache.cover_cache_mb, got `{value}`")
            })?;
            cache_section_mut(&mut file).cover_cache_mb = Some(parsed);
        }
        ConfigKey::CacheCoverCacheTtlDays => {
            let parsed: u64 = value.trim().parse().with_context(|| {
                format!("expected a positive integer for cache.cover_cache_ttl_days, got `{value}`")
            })?;
            if parsed == 0 {
                return Err(SpotifyError::InvalidInput {
                    message: "cache.cover_cache_ttl_days must be greater than 0".to_string(),
                });
            }
            cache_section_mut(&mut file).cover_cache_ttl_days = Some(parsed);
        }
        ConfigKey::NotificationsEnabled => {
            notifications_section_mut(&mut file).enabled = Some(parse_bool(value)?);
        }
        ConfigKey::NotificationsSummary => {
            notifications_section_mut(&mut file).summary = blank_to_none(Some(value.to_string()));
        }
        ConfigKey::NotificationsBody => {
            notifications_section_mut(&mut file).body = blank_to_none(Some(value.to_string()));
        }
        ConfigKey::NotificationsOnTrackChange => {
            notifications_section_mut(&mut file).on_track_change = Some(parse_bool(value)?);
        }
        ConfigKey::NotificationsOnPause => {
            notifications_section_mut(&mut file).on_pause = Some(parse_bool(value)?);
        }
        ConfigKey::NotificationsOnResume => {
            notifications_section_mut(&mut file).on_resume = Some(parse_bool(value)?);
        }
        ConfigKey::NotificationsOnSkip => {
            notifications_section_mut(&mut file).on_skip = Some(parse_bool(value)?);
        }
        ConfigKey::NotificationsOnError => {
            notifications_section_mut(&mut file).on_error = Some(parse_bool(value)?);
        }
    }

    write_config_file(&path, &file)?;
    Ok(path)
}

fn ensure_config_exists(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }

    write_template(path)?;
    bail!(
        "created {}; add your Spotify client_id and client_secret, then rerun spotuify",
        path.display()
    )
}

fn read_config_file(path: &Path) -> Result<FileConfig> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    // Phase 13 (P13-H) — merge `SPOTUIFY_CONFIG_OVERRIDES` (set by the
    // CLI's `-o key.path=value` flag) into the parsed TOML before
    // deserialisation. Round-trips through `toml::Value` so the override
    // applies to whichever section/field the user named without
    // touching the file on disk.
    let mut value: toml::Value =
        toml::from_str(&contents).with_context(|| format!("could not parse {}", path.display()))?;
    apply_dotpath_overrides(&mut value);
    let merged = toml::to_string(&value)
        .with_context(|| format!("re-emit failed for {}", path.display()))?;
    toml::from_str(&merged)
        .with_context(|| format!("could not parse merged config {}", path.display()))
}

/// Phase 13 (P13-H) — apply CLI dot-path overrides from
/// `SPOTUIFY_CONFIG_OVERRIDES`. The env-var format is one
/// `key.path=value` per line. Values are parsed as TOML literals so
/// `bitrate=160` becomes an integer, `name="foo"` becomes a string,
/// `autostart=true` becomes a bool, etc.
pub(crate) fn apply_dotpath_overrides(root: &mut toml::Value) {
    let raw = match std::env::var("SPOTUIFY_CONFIG_OVERRIDES") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => return,
    };
    for line in raw.lines() {
        if let Err(err) = apply_single_override(root, line) {
            tracing::warn!(input = %line, error = %err, "skipping invalid `-o` override");
        }
    }
}

fn apply_single_override(root: &mut toml::Value, raw: &str) -> Result<()> {
    let (path, value_str) = raw
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("expected key.path=value; got `{raw}`"))?;
    if path.trim().is_empty() {
        anyhow::bail!("empty key in `{raw}`");
    }
    // Parse the right-hand side as TOML so `=` plays well with both
    // bare literals (160, true, "name") and quoted strings.
    let parsed: toml::Value = toml::from_str(&format!("__rhs__ = {value_str}"))
        .with_context(|| format!("value `{value_str}` is not valid TOML"))?;
    let rhs = parsed
        .get("__rhs__")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("internal: failed to extract rhs"))?;
    let parts: Vec<&str> = path.split('.').collect();
    let mut cursor = root;
    for (i, key) in parts.iter().enumerate() {
        if i == parts.len() - 1 {
            // Last segment: insert.
            let table = cursor
                .as_table_mut()
                .ok_or_else(|| anyhow::anyhow!("cannot set `{path}`: parent is not a table"))?;
            table.insert(key.to_string(), rhs);
            return Ok(());
        }
        // Intermediate segment: navigate/create.
        let entry_is_table = cursor
            .as_table()
            .map(|t| t.get(*key).map(|v| v.is_table()).unwrap_or(false))
            .unwrap_or(false);
        if !entry_is_table {
            // Create or overwrite-as-table.
            let table = cursor.as_table_mut().ok_or_else(|| {
                anyhow::anyhow!("cannot navigate into `{path}`: parent is not a table")
            })?;
            table.insert(
                key.to_string(),
                toml::Value::Table(toml::value::Table::new()),
            );
        }
        cursor = cursor
            .get_mut(*key)
            .ok_or_else(|| anyhow::anyhow!("internal navigation failure at `{key}`"))?;
    }
    Ok(())
}

fn write_config_file(path: &Path, file: &FileConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let contents = toml::to_string_pretty(file).context("failed to encode config")?;
    fs::write(path, contents).with_context(|| format!("failed to write {}", path.display()))
}

fn write_template(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        // Phase 13 (P13-G) — hedge against dotfile-sync token leaks
        // (chezmoi/dotbot etc) by dropping a .gitignore on first init.
        write_gitignore_if_absent(parent);
    }
    fs::write(path, CONFIG_TEMPLATE).with_context(|| format!("failed to create {}", path.display()))
}

/// Phase 13 (P13-G) — write a `.gitignore` in the config dir if absent.
/// Pattern lifted from spotatui (`core/config.rs:99-115`). Safe to call
/// on every daemon start; only writes when missing.
pub fn write_gitignore_if_absent(config_dir: &Path) {
    let path = config_dir.join(".gitignore");
    if path.exists() {
        return;
    }
    const GITIGNORE: &str = "# Auto-generated by spotuify on first run.\n\
                             # Hedges against dotfile-sync tools accidentally\n\
                             # uploading secrets to a public repo.\n\
                             *.json\n\
                             credentials.*\n\
                             *.encrypted\n\
                             *.log\n\
                             cache/\n";
    if let Err(err) = fs::write(&path, GITIGNORE) {
        tracing::warn!(path = %path.display(), error = %err, "failed to write auto-.gitignore");
    }
}

fn player_section_mut(file: &mut FileConfig) -> &mut PlayerSection {
    file.player.get_or_insert_with(PlayerSection::default)
}

fn cache_section_mut(file: &mut FileConfig) -> &mut CacheSection {
    file.cache.get_or_insert_with(CacheSection::default)
}

fn analytics_section_mut(file: &mut FileConfig) -> &mut AnalyticsSection {
    file.analytics.get_or_insert_with(AnalyticsSection::default)
}

fn notifications_section_mut(file: &mut FileConfig) -> &mut NotificationsSection {
    file.notifications
        .get_or_insert_with(NotificationsSection::default)
}

fn default_redirect_uri() -> String {
    "http://127.0.0.1:8888/callback".to_string()
}

fn blank_to_none(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_string();
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    })
}

fn parse_bool(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => bail!("expected true or false, got `{value}`"),
    }
}


fn expand_home(value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(value)
}

const CONFIG_TEMPLATE: &str = r#"# spotuify config
# Copy your Spotify app credentials from https://developer.spotify.com/dashboard.
client_id = ""
client_secret = ""
redirect_uri = "http://127.0.0.1:8888/callback"

[player]
# Backend that registers spotuify as a Spotify Connect device.
# Only "embedded" is supported (in-process librespot, Premium required).
backend = "embedded"
# Stream quality. One of 96, 160, 320.
bitrate = 320
# Optional: override the Connect device name. Defaults to the hostname.
# device_name = "spotuify"
# ReplayGain normalization.
normalization = false
# Disk cache for audio frames in MiB; 0 disables caching.
audio_cache_mib = 0
# Set PULSE_PROP_* env vars so spotuify appears nicely in pavucontrol (Linux only).
pulse_props = true
# Legacy alias for analytics.hook_command.
# event_hook = "/usr/local/bin/notify"

[cache]
# Shared cover-art cache cap and stale-while-revalidate TTL.
cover_cache_mb = 200
cover_cache_ttl_days = 30

[analytics]
# Optional shell hook for listen-qualified / playback lifecycle events.
# hook_command = "/usr/local/bin/spotuify-hook"
hook_timeout_ms = 5000

[notifications]
# Desktop notifications are opt-in.
enabled = false
summary = "{track}"
body = "{artist} - {album}"
on_track_change = true
on_pause = false
on_resume = false
on_skip = false
on_error = true
"#;

#[cfg(test)]
mod tests {
    use super::{
        apply_single_override, expand_home, get_config_value, parse_bool, set_config_value, Config,
        ConfigKey,
    };

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn restore_env(key: &str, value: Option<std::ffi::OsString>) {
        if let Some(value) = value {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn viz_config_default_ships_visualizer_enabled_so_users_see_it_without_opting_in() {
        let viz = super::VizConfig::default();
        assert!(
            viz.enabled,
            "VizConfig::default must ship visualizer on — it's the player's identity"
        );
    }

    #[test]
    fn keeps_absolute_paths() {
        assert_eq!(
            expand_home("/tmp/sample.conf"),
            std::path::PathBuf::from("/tmp/sample.conf")
        );
    }

    #[test]
    fn parses_bool_config_values() {
        assert!(parse_bool("on").expect("on should parse as true"));
        assert!(!parse_bool("false").expect("false should parse as false"));
        assert!(parse_bool("later").is_err());
    }

    #[test]
    fn dotpath_override_overwrites_existing_player_bitrate() {
        let mut value: toml::Value =
            toml::from_str("[player]\nbitrate = 320\n").expect("test TOML should parse");
        apply_single_override(&mut value, "player.bitrate=96")
            .expect("bitrate override should apply");
        assert_eq!(
            value["player"]["bitrate"]
                .as_integer()
                .expect("bitrate should be integer"),
            96
        );
    }

    #[test]
    fn dotpath_override_creates_missing_section() {
        let mut value: toml::Value = toml::from_str("").expect("empty TOML should parse");
        apply_single_override(&mut value, "notifications.enabled=true")
            .expect("missing section override should apply");
        assert!(value["notifications"]["enabled"]
            .as_bool()
            .expect("enabled should be bool"));
    }

    #[test]
    fn dotpath_override_supports_quoted_strings() {
        let mut value: toml::Value = toml::from_str("").expect("empty TOML should parse");
        apply_single_override(&mut value, "player.device_name=\"my-laptop\"")
            .expect("quoted string override should apply");
        assert_eq!(
            value["player"]["device_name"]
                .as_str()
                .expect("device_name should be string"),
            "my-laptop"
        );
    }

    #[test]
    fn dotpath_override_rejects_malformed_input() {
        // Missing `=` → bail. The CLI logs a warning and skips the
        // override rather than failing the whole config load.
        let mut value: toml::Value = toml::from_str("").expect("empty TOML should parse");
        assert!(apply_single_override(&mut value, "no-equals-sign").is_err());
    }

    #[test]
    fn dotpath_override_rejects_empty_key() {
        let mut value: toml::Value = toml::from_str("").expect("empty TOML should parse");
        assert!(apply_single_override(&mut value, "=42").is_err());
    }

    #[test]
    fn config_load_applies_dotpath_override_without_writing_config_file() {
        let _guard = ENV_LOCK.lock().expect("env lock poisoned");
        let temp = tempfile::TempDir::new().expect("tempdir");
        let config_path = temp.path().join("spotuify.toml");
        let original = r#"
client_id = "client"
client_secret = "secret"

[player]
bitrate = 320
"#;
        std::fs::write(&config_path, original).expect("write config");

        let old_config = std::env::var_os("SPOTUIFY_CONFIG");
        let old_overrides = std::env::var_os("SPOTUIFY_CONFIG_OVERRIDES");
        std::env::set_var("SPOTUIFY_CONFIG", &config_path);
        std::env::set_var("SPOTUIFY_CONFIG_OVERRIDES", "player.bitrate=96");

        let config = Config::load().expect("config should load with override");

        restore_env("SPOTUIFY_CONFIG", old_config);
        restore_env("SPOTUIFY_CONFIG_OVERRIDES", old_overrides);

        assert_eq!(config.player.bitrate, 96);
        assert_eq!(
            std::fs::read_to_string(&config_path).expect("read config"),
            original
        );
    }

    #[test]
    fn config_set_and_get_supports_preferred_analytics_hook_keys() {
        let _guard = ENV_LOCK.lock().expect("env lock poisoned");
        let temp = tempfile::TempDir::new().expect("tempdir");
        let config_path = temp.path().join("spotuify.toml");

        let old_config = std::env::var_os("SPOTUIFY_CONFIG");
        std::env::set_var("SPOTUIFY_CONFIG", &config_path);

        set_config_value(ConfigKey::AnalyticsHookCommand, "hook.sh")
            .expect("analytics hook should write");
        set_config_value(ConfigKey::AnalyticsHookTimeoutMs, "1234")
            .expect("analytics hook timeout should write");

        let hook =
            get_config_value(ConfigKey::AnalyticsHookCommand).expect("analytics hook should read");
        let timeout = get_config_value(ConfigKey::AnalyticsHookTimeoutMs)
            .expect("analytics hook timeout should read");

        restore_env("SPOTUIFY_CONFIG", old_config);

        assert_eq!(hook.as_deref(), Some("hook.sh"));
        assert_eq!(timeout.as_deref(), Some("1234"));
    }

    #[test]
    fn config_set_and_get_supports_notification_keys() {
        let _guard = ENV_LOCK.lock().expect("env lock poisoned");
        let temp = tempfile::TempDir::new().expect("tempdir");
        let config_path = temp.path().join("spotuify.toml");

        let old_config = std::env::var_os("SPOTUIFY_CONFIG");
        std::env::set_var("SPOTUIFY_CONFIG", &config_path);

        set_config_value(ConfigKey::NotificationsEnabled, "true")
            .expect("notification enabled should write");
        set_config_value(ConfigKey::NotificationsSummary, "{track}")
            .expect("notification summary should write");
        set_config_value(ConfigKey::NotificationsOnPause, "on")
            .expect("notification pause toggle should write");

        let enabled =
            get_config_value(ConfigKey::NotificationsEnabled).expect("enabled should read");
        let summary =
            get_config_value(ConfigKey::NotificationsSummary).expect("summary should read");
        let pause = get_config_value(ConfigKey::NotificationsOnPause).expect("pause should read");

        restore_env("SPOTUIFY_CONFIG", old_config);

        assert_eq!(enabled.as_deref(), Some("true"));
        assert_eq!(summary.as_deref(), Some("{track}"));
        assert_eq!(pause.as_deref(), Some("true"));
    }

    #[test]
    fn init_config_writes_gitignore_next_to_template() {
        let _guard = ENV_LOCK.lock().expect("env lock poisoned");
        let temp = tempfile::TempDir::new().expect("tempdir");
        let config_path = temp.path().join("spotuify").join("spotuify.toml");
        let old_config = std::env::var_os("SPOTUIFY_CONFIG");
        std::env::set_var("SPOTUIFY_CONFIG", &config_path);

        let path = super::init_config().expect("init config should create template");

        restore_env("SPOTUIFY_CONFIG", old_config);

        assert_eq!(path, config_path);
        let gitignore = std::fs::read_to_string(
            config_path
                .parent()
                .expect("config path should have parent")
                .join(".gitignore"),
        )
        .expect(".gitignore should be written");
        assert!(gitignore.contains("*.json"));
        assert!(gitignore.contains("credentials.*"));
        assert!(gitignore.contains("*.encrypted"));
    }
}

// ---------- Phase 9 — [player] config tests (red phase) ----------
//
// Asserts each new field's default *value* (not Default::default()
// self-equality), validates bitrate is one of {96, 160, 320},
// rejects unknown backend kinds with the typo present in the error,
// round-trips a fully-populated [player] section, and confirms the
// ConfigKey parse + setter path covers every new key.
#[cfg(test)]
mod player_config {
    use super::{
        AnalyticsConfig, ConfigKey, DiscordConfig, FileConfig, NotificationsConfig, PlayerConfig,
    };
    use spotuify_core::BackendKind;

    fn parse_file(toml: &str) -> FileConfig {
        toml::from_str(toml).expect("test TOML should parse")
    }

    #[test]
    fn empty_toml_yields_explicit_defaults() {
        let file = parse_file("");
        let player = PlayerConfig::from_file(&file);

        assert_eq!(
            player.backend,
            BackendKind::Embedded,
            "embedded librespot is the only supported backend post-Phase-0"
        );
        assert_eq!(player.bitrate, 320, "default bitrate is the highest tier");
        assert_eq!(
            player.device_name, None,
            "no device_name means use hostname"
        );
        assert!(!player.normalization, "ReplayGain off by default");
        assert_eq!(player.audio_cache_mib, 0, "audio cache disabled by default");
        assert!(player.pulse_props, "pulse_props on by default (Linux only)");
        assert_eq!(player.event_hook, None);
    }

    #[test]
    fn populated_player_section_parses_every_field() {
        let toml = r#"
[player]
backend = "embedded"
bitrate = 160
device_name = "studio"
normalization = true
audio_cache_mib = 256
pulse_props = false
event_hook = "/usr/local/bin/notify"
"#;
        let file = parse_file(toml);
        let player = PlayerConfig::from_file(&file);

        assert_eq!(player.backend, BackendKind::Embedded);
        assert_eq!(player.bitrate, 160);
        assert_eq!(player.device_name.as_deref(), Some("studio"));
        assert!(player.normalization);
        assert_eq!(player.audio_cache_mib, 256);
        assert!(!player.pulse_props);
        assert_eq!(player.event_hook.as_deref(), Some("/usr/local/bin/notify"));
    }

    #[test]
    fn bitrate_outside_known_tiers_is_rejected() {
        // Adversarial: 200 is plausible-looking but invalid. Catches the
        // bug where the parser silently accepts any u32.
        let toml = r#"
[player]
bitrate = 200
"#;
        let file = parse_file(toml);
        let err = PlayerConfig::validate(&file).expect_err("bitrate=200 must error");
        assert!(err.to_string().contains("200"), "err: {err}");
        assert!(err.to_string().contains("bitrate"), "err: {err}");
    }

    #[test]
    fn backend_typo_surfaces_the_typo_in_the_error() {
        // Adversarial: error must echo what the user typed so they can
        // fix the line without grepping. A generic "invalid backend"
        // message would fail this.
        let toml = r#"
[player]
backend = "embeded"
"#;
        let file = parse_file(toml);
        let err = PlayerConfig::validate(&file).expect_err("typo must error");
        assert!(
            err.to_string().contains("embeded"),
            "err message must echo the typo `embeded`, got: {err}"
        );
    }

    #[test]
    fn defaults_round_trip_through_toml() {
        // Adversarial: catches the bug where adding a field forgets
        // `#[serde(default)]` — round-trip would lose the value.
        let original = PlayerConfig {
            backend: BackendKind::Embedded,
            bitrate: 96,
            device_name: Some("kitchen".to_string()),
            normalization: true,
            audio_cache_mib: 128,
            pulse_props: false,
            event_hook: Some("hook.sh".to_string()),
        };
        let serialized = toml::to_string_pretty(&FileConfig {
            client_id: None,
            client_secret: None,
            redirect_uri: None,
            player: Some(original.clone().into()),
            cache: None,
            analytics: None,
            notifications: None,
            discord: None,
            viz: None,
        })
        .expect("player config should serialize");
        let parsed = parse_file(&serialized);
        let round_tripped = PlayerConfig::from_file(&parsed);

        assert_eq!(round_tripped, original);
    }

    #[test]
    fn config_key_parses_every_player_and_hook_key() {
        assert_eq!(
            ConfigKey::parse("player.backend").expect("player.backend should parse"),
            ConfigKey::PlayerBackend
        );
        assert_eq!(
            ConfigKey::parse("player.bitrate").expect("player.bitrate should parse"),
            ConfigKey::PlayerBitrate
        );
        assert_eq!(
            ConfigKey::parse("player.device_name").expect("player.device_name should parse"),
            ConfigKey::PlayerDeviceName
        );
        assert_eq!(
            ConfigKey::parse("player.device-name").expect("player.device-name should parse"),
            ConfigKey::PlayerDeviceName
        );
        assert_eq!(
            ConfigKey::parse("player.normalization").expect("player.normalization should parse"),
            ConfigKey::PlayerNormalization
        );
        assert_eq!(
            ConfigKey::parse("player.audio_cache_mib")
                .expect("player.audio_cache_mib should parse"),
            ConfigKey::PlayerAudioCacheMib
        );
        assert_eq!(
            ConfigKey::parse("player.pulse_props").expect("player.pulse_props should parse"),
            ConfigKey::PlayerPulseProps
        );
        assert_eq!(
            ConfigKey::parse("player.event_hook").expect("player.event_hook should parse"),
            ConfigKey::PlayerEventHook
        );
        assert_eq!(
            ConfigKey::parse("analytics.hook_command")
                .expect("analytics.hook_command should parse"),
            ConfigKey::AnalyticsHookCommand
        );
        assert_eq!(
            ConfigKey::parse("analytics.hook-timeout-ms")
                .expect("analytics.hook-timeout-ms should parse"),
            ConfigKey::AnalyticsHookTimeoutMs
        );
        assert_eq!(
            ConfigKey::parse("notifications.enabled").expect("notifications.enabled should parse"),
            ConfigKey::NotificationsEnabled
        );
        assert_eq!(
            ConfigKey::parse("notifications.on-track-change")
                .expect("notifications.on-track-change should parse"),
            ConfigKey::NotificationsOnTrackChange
        );
    }

    #[test]
    fn config_key_valid_keys_lists_every_player_and_hook_field() {
        // Adversarial: the error message in ConfigKey::parse is the
        // only discoverability surface for users. Locking the listing
        // catches the bug where someone adds a key but forgets the
        // help text.
        let valid = ConfigKey::valid_keys();
        for key in &[
            "player.backend",
            "player.bitrate",
            "player.device_name",
            "player.normalization",
            "player.audio_cache_mib",
            "player.pulse_props",
            "player.event_hook",
            "analytics.hook_command",
            "analytics.hook_timeout_ms",
            "notifications.enabled",
            "notifications.summary",
            "notifications.body",
            "notifications.on_track_change",
            "notifications.on_pause",
            "notifications.on_resume",
            "notifications.on_skip",
            "notifications.on_error",
        ] {
            assert!(
                valid.contains(key),
                "valid_keys missing {key}; got {valid:?}"
            );
        }
    }

    #[test]
    fn analytics_config_defaults_match_blueprint() {
        let cfg = AnalyticsConfig::default();
        assert!(cfg.store_raw_queries);
        assert_eq!(cfg.retention_progress_days, 90);
        assert_eq!(cfg.retention_events_days, 365);
        assert_eq!(cfg.retention_operations_days, 90);
        assert_eq!(cfg.daily_rollup_hour, 3);
        assert!(cfg.hook_command.is_none());
        assert_eq!(cfg.hook_timeout_ms, 5_000);
        assert!(!cfg.allow_file_credentials);
    }

    #[test]
    fn analytics_section_from_partial_toml_keeps_defaults_for_missing_keys() {
        let toml = r#"
[analytics]
store_raw_queries = false
hook_command = "scrobble.sh"
"#;
        let file = parse_file(toml);
        let cfg = AnalyticsConfig::from_file(&file);
        assert!(!cfg.store_raw_queries);
        assert_eq!(cfg.hook_command.as_deref(), Some("scrobble.sh"));
        // Unset fields fall back to defaults:
        assert_eq!(cfg.retention_progress_days, 90);
        assert_eq!(cfg.daily_rollup_hour, 3);
    }

    #[test]
    fn analytics_daily_rollup_hour_out_of_range_falls_back_to_default() {
        let toml = "[analytics]\ndaily_rollup_hour = 25\n";
        let file = parse_file(toml);
        let cfg = AnalyticsConfig::from_file(&file);
        assert_eq!(cfg.daily_rollup_hour, 3);
    }

    #[test]
    fn system_integration_sections_from_partial_toml_keep_defaults() {
        let toml = r#"
[notifications]
enabled = true
summary = "{track}"

[discord]
enabled = true
application_id = "123456"
"#;
        let file = parse_file(toml);
        let notifications = NotificationsConfig::from_file(&file);
        let discord = DiscordConfig::from_file(&file);

        assert!(notifications.enabled);
        assert_eq!(notifications.summary, "{track}");
        assert_eq!(notifications.body, "{artist} - {album}");
        assert!(notifications.on_track_change);
        assert!(discord.enabled);
        assert_eq!(discord.application_id.as_deref(), Some("123456"));
    }
}
