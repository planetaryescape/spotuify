//! Provider-neutral configuration foundation.
//!
//! The document layer preserves user formatting and unknown provider tables.
//! The resolved layer exposes generic application settings and ordered raw
//! provider entries; each adapter remains responsible for deserializing and
//! validating its own entry.

mod document;
mod model;
mod path;

pub use document::{
    config_path, get_config_value, get_config_value_at, get_effective_config_value,
    get_effective_config_value_at, init_config, init_config_at, migrate_legacy_config,
    migrate_legacy_config_at, set_config_value, set_config_value_at, MigrationChange,
    MigrationOutcome, MigrationReport, CONFIG_TEMPLATE,
};
pub use model::{
    load, load_from, load_from_with_overrides, load_str, load_str_with_overrides, AnalyticsConfig,
    AppConfig, CacheConfig, ConfigError, ConfigOverrides, ConfigWarning, DiscordConfig,
    EnvOverrides, LoadedConfig, NotificationsConfig, PlayerSettings, ProviderEntry, VizConfig,
};
pub use path::{ConfigPath, ConfigValue};

pub type Result<T> = std::result::Result<T, ConfigError>;

/// Stable settings roster used by `spotuify config show` and visual editors.
pub const EDITABLE_CONFIG_PATHS: &[&str] = &[
    "providers.default",
    "providers.spotify.type",
    "providers.spotify.client_id",
    "providers.spotify.client_secret",
    "providers.spotify.redirect_uri",
    "providers.spotify.player.bitrate",
    "providers.spotify.player.device_name",
    "providers.spotify.player.audio_output_device",
    "providers.spotify.player.normalization",
    "providers.spotify.player.audio_cache_mib",
    "providers.spotify.player.pulse_props",
    "analytics.hook_command",
    "analytics.hook_timeout_ms",
    "analytics.store_raw_queries",
    "analytics.retention_progress_days",
    "analytics.retention_events_days",
    "analytics.retention_operations_days",
    "analytics.daily_rollup_hour",
    "analytics.allow_file_credentials",
    "analytics.lastfm_api_key",
    "analytics.lastfm_user",
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
    "discord.enabled",
    "discord.application_id",
    "viz.enabled",
    "viz.source",
    "viz.target_fps",
    "viz.smoothing",
    "viz.noise_gate",
    "viz.color_scheme",
];

#[cfg(test)]
mod tests;
