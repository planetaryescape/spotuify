#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;

use serde::Deserialize;
use spotuify_core::ProviderId;
use tempfile::tempdir;

use crate::{
    get_config_value_at, get_effective_config_value_at, init_config_at, load_str,
    load_str_with_overrides, migrate_legacy_config_at, set_config_value_at, ConfigError,
    ConfigOverrides, ConfigPath, ConfigWarning, EnvOverrides, MigrationOutcome, PlayerSettings,
};

#[derive(Debug, Deserialize)]
struct SpotifyAdapterConfig {
    client_id: String,
    redirect_uri: String,
    #[serde(default)]
    player: PlayerSettings,
}

fn test_path() -> &'static Path {
    Path::new("/tmp/spotuify-config-test.toml")
}

#[test]
fn canonical_provider_tables_keep_order_and_deserialize_in_adapter() {
    let loaded = load_str(
        test_path(),
        r#"
[providers]
default = "spotify"

[providers.archive]
type = "filesystem"
root = "/music"

[providers.spotify]
type = "spotify"
client_id = "canonical-id"
redirect_uri = "http://localhost/callback"

[providers.spotify.player]
backend = "embedded"
bitrate = 160
"#,
        &EnvOverrides::default(),
    )
    .expect("canonical config should load");

    assert_eq!(
        loaded
            .config
            .providers
            .iter()
            .map(|provider| provider.id.as_str())
            .collect::<Vec<_>>(),
        ["archive", "spotify"]
    );
    assert_eq!(
        loaded
            .config
            .default_provider
            .as_ref()
            .map(|provider| provider.as_str()),
        Some("spotify")
    );
    let spotify: SpotifyAdapterConfig = loaded
        .config
        .default_provider()
        .expect("default provider")
        .deserialize()
        .expect("adapter owns provider deserialization");
    assert_eq!(spotify.client_id, "canonical-id");
    assert_eq!(spotify.redirect_uri, "http://localhost/callback");
    assert_eq!(spotify.player.bitrate, 160);
    assert!(!toml::to_string(&spotify.player)
        .expect("player settings should serialize")
        .contains("backend"));
}

#[test]
fn legacy_backend_is_validated_but_not_a_resolved_setting() {
    let error = load_str(
        test_path(),
        r#"
[providers]
default = "spotify"

[providers.spotify]
type = "spotify"
client_id = "canonical-id"

[providers.spotify.player]
backend = "connect"
"#,
        &EnvOverrides::default(),
    )
    .expect_err("removed backend selectors must still be rejected");

    assert!(matches!(
        error,
        ConfigError::ProviderDecode { ref provider } if provider.as_str() == "spotify"
    ));
}

#[test]
fn legacy_spotify_shape_loads_with_explicit_warnings() {
    let loaded = load_str(
        test_path(),
        r#"
client_id = "legacy-id"
redirect_uri = "http://localhost/legacy"

[player]
backend = "embedded"
bitrate = 96
event_hook = "notify-hook"

[spotifyd]
device_name = "legacy-device"
"#,
        &EnvOverrides::default(),
    )
    .expect("legacy config should load");

    assert_eq!(
        loaded
            .config
            .default_provider
            .as_ref()
            .map(|provider| provider.as_str()),
        Some("spotify")
    );
    let spotify: SpotifyAdapterConfig = loaded.config.providers[0]
        .deserialize()
        .expect("legacy values resolve to a provider table");
    assert_eq!(spotify.client_id, "legacy-id");
    assert_eq!(spotify.player.bitrate, 96);
    assert_eq!(spotify.player.device_name.as_deref(), Some("legacy-device"));
    assert_eq!(
        loaded.config.analytics.hook_command.as_deref(),
        Some("notify-hook")
    );
    assert!(loaded.warnings.iter().any(|warning| matches!(
        warning,
        ConfigWarning::DeprecatedKey { legacy_path, canonical_path, ignored: false }
            if legacy_path == "client_id" && canonical_path == "providers.spotify.client_id"
    )));
    assert!(loaded.warnings.iter().any(|warning| matches!(
        warning,
        ConfigWarning::DeprecatedKey { legacy_path, canonical_path, ignored: false }
            if legacy_path == "spotifyd.device_name"
                && canonical_path == "providers.spotify.player.device_name"
    )));
}

#[test]
fn precedence_is_environment_then_canonical_then_flat_then_spotifyd_then_defaults() {
    let loaded = load_str(
        test_path(),
        r#"
client_id = "flat-id"
redirect_uri = "http://localhost/flat"

[player]
device_name = "flat-device"

[spotifyd]
device_name = "spotifyd-device"

[providers]
default = "spotify"

[providers.spotify]
type = "spotify"
client_id = "canonical-id"
redirect_uri = "http://localhost/canonical"

[providers.spotify.player]
device_name = "canonical-device"
"#,
        &EnvOverrides {
            spotify_client_id: Some("environment-id".to_string()),
            ..EnvOverrides::default()
        },
    )
    .expect("mixed config should load");

    let spotify: SpotifyAdapterConfig = loaded.config.providers[0]
        .deserialize()
        .expect("resolved provider config");
    assert_eq!(spotify.client_id, "environment-id");
    assert_eq!(spotify.redirect_uri, "http://localhost/canonical");
    assert_eq!(
        spotify.player.device_name.as_deref(),
        Some("canonical-device")
    );
    assert_eq!(spotify.player.bitrate, 320);
    assert!(loaded.warnings.iter().any(|warning| matches!(
        warning,
        ConfigWarning::DeprecatedKey { legacy_path, ignored: true, .. }
            if legacy_path == "client_id"
    )));
}

#[test]
fn generic_config_loads_without_any_provider() {
    let loaded = load_str(
        test_path(),
        r#"
[cache]
cover_cache_mb = 512

[viz]
enabled = false
target_fps = 24
"#,
        &EnvOverrides::default(),
    )
    .expect("provider-less generic config should load");

    assert!(loaded.config.default_provider.is_none());
    assert!(loaded.config.providers.is_empty());
    assert_eq!(loaded.config.cache.cover_cache_mb, 512);
    assert!(!loaded.config.viz.enabled);
    assert_eq!(loaded.config.viz.target_fps, 24);
}

#[test]
fn legacy_out_of_range_values_are_clamped_not_rejected() {
    // These values loaded on origin/main (normalize clamped them). A regression
    // hard-errored the entire config load, which would abort daemon startup.
    let loaded = load_str(
        test_path(),
        r#"
[analytics]
daily_rollup_hour = 25

[viz]
target_fps = 0
smoothing = 0.99
noise_gate = 1.5
source = "typo"
"#,
        &EnvOverrides::default(),
    )
    .expect("legacy config with repairable values must still load");

    assert_eq!(loaded.config.analytics.daily_rollup_hour, 3);
    assert_eq!(loaded.config.viz.target_fps, 1);
    assert!((loaded.config.viz.smoothing - 0.95).abs() < f32::EPSILON);
    assert!((loaded.config.viz.noise_gate - 1.0).abs() < f32::EPSILON);
    assert_eq!(loaded.config.viz.source, "auto");
}

#[test]
fn sole_provider_is_default_but_multiple_without_explicit_default_is_rejected() {
    let sole = load_str(
        test_path(),
        "[providers.local]\ntype = \"fake\"\n",
        &EnvOverrides::default(),
    )
    .expect("sole provider should infer default");
    assert_eq!(sole.config.default_provider.unwrap().as_str(), "local");

    let error = load_str(
        test_path(),
        "[providers.one]\ntype = \"fake\"\n[providers.two]\ntype = \"fake\"\n",
        &EnvOverrides::default(),
    )
    .expect_err("multiple providers require an explicit default");
    assert!(error.to_string().contains("providers.default is required"));

    let malformed = load_str(
        test_path(),
        "providers = \"not-a-table\"\n",
        &EnvOverrides::default(),
    )
    .expect_err("malformed provider roots must fail closed");
    assert!(malformed.to_string().contains("providers must be a table"));
}

#[test]
fn multiple_spotify_adapters_are_rejected_before_runtime_construction() {
    let error = load_str(
        test_path(),
        r#"
[providers]
default = "work"
[providers.work]
type = "spotify"
[providers.personal]
type = "spotify"
"#,
        &EnvOverrides::default(),
    )
    .expect_err("two adapters cannot both own the spotify URI namespace");

    let message = error.to_string();
    assert!(message.contains("URI scheme `spotify`"));
    assert!(message.contains("work"));
    assert!(message.contains("personal"));
}

#[test]
fn fake_default_with_one_secondary_spotify_adapter_is_valid() {
    let loaded = load_str(
        test_path(),
        r#"
[providers]
default = "local"
[providers.local]
type = "fake"
[providers.work]
type = "spotify"
"#,
        &EnvOverrides::default(),
    )
    .expect("one Spotify URI owner remains unambiguous beside a fake default");

    assert_eq!(loaded.config.default_provider.unwrap().as_str(), "local");
    assert_eq!(loaded.config.providers.len(), 2);
}

#[test]
fn dual_fake_config_keeps_distinct_namespaces_and_adapter_settings() {
    #[derive(Debug, Deserialize)]
    struct FakeAdapterConfig {
        dataset: String,
    }

    let loaded = load_str(
        test_path(),
        r#"
[providers]
default = "fake-b"

[providers.fake-a]
type = "fake"
dataset = "standard"

[providers.fake-b]
type = "fake"
dataset = "empty"
"#,
        &EnvOverrides::default(),
    )
    .expect("two fake adapters with an explicit default should load");

    assert_eq!(
        loaded
            .config
            .providers
            .iter()
            .map(|provider| (provider.id.as_str(), provider.kind.as_str()))
            .collect::<Vec<_>>(),
        [("fake-a", "fake"), ("fake-b", "fake")]
    );
    assert_eq!(
        loaded
            .config
            .default_provider
            .as_ref()
            .map(ProviderId::as_str),
        Some("fake-b")
    );
    let first: FakeAdapterConfig = loaded.config.providers[0]
        .deserialize()
        .expect("first fake settings");
    let second: FakeAdapterConfig = loaded.config.providers[1]
        .deserialize()
        .expect("second fake settings");
    assert_eq!(first.dataset, "standard");
    assert_eq!(second.dataset, "empty");
}

#[test]
fn typed_overrides_are_last_wins_below_process_env_and_never_mutate_source() {
    let source = r#"
client_id = "legacy"
[providers.spotify]
type = "spotify"
client_id = "canonical"
redirect_uri = "http://localhost/callback"
[providers.spotify.player]
bitrate = 320
"#;
    let overrides = ConfigOverrides::parse([
        "player.bitrate=160",
        "providers.spotify.player.bitrate=96",
        "client_id=\"overlay\"",
    ])
    .expect("typed overrides");
    let loaded = load_str_with_overrides(
        test_path(),
        source,
        &EnvOverrides {
            spotify_client_id: Some("environment".to_string()),
            ..EnvOverrides::default()
        },
        &overrides,
    )
    .expect("overlay load");
    let spotify: SpotifyAdapterConfig = loaded.config.providers[0].deserialize().unwrap();
    assert_eq!(spotify.client_id, "environment");
    assert_eq!(spotify.player.bitrate, 96);
    assert_eq!(source.matches("bitrate = 320").count(), 1);
}

#[test]
fn spotify_environment_targets_the_configured_adapter_id_not_its_kind_name() {
    let loaded = load_str(
        test_path(),
        r#"
[providers]
default = "work"
[providers.work]
type = "spotify"
client_id = "file-id"
redirect_uri = "http://localhost/callback"
[providers.local]
type = "fake"
"#,
        &EnvOverrides {
            spotify_client_id: Some("environment-id".to_string()),
            ..EnvOverrides::default()
        },
    )
    .unwrap();
    assert_eq!(
        loaded
            .config
            .providers
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        ["work", "local"]
    );
    let work: SpotifyAdapterConfig = loaded.config.providers[0].deserialize().unwrap();
    assert_eq!(work.client_id, "environment-id");
}

#[test]
fn credentials_never_leak_into_non_spotify_provider_named_spotify() {
    let loaded = load_str(
        test_path(),
        r#"
[providers]
default = "spotify"
[providers.spotify]
type = "fake"
[providers.work]
type = "spotify"
client_id = "file-id"
redirect_uri = "http://localhost/callback"
"#,
        &EnvOverrides {
            spotify_client_id: Some("environment-id".to_string()),
            ..EnvOverrides::default()
        },
    )
    .expect("sole Spotify adapter is an unambiguous environment target");

    let fake = loaded
        .config
        .providers
        .iter()
        .find(|provider| provider.id.as_str() == "spotify")
        .unwrap();
    assert!(fake.raw_table().get("client_id").is_none());
    let work: SpotifyAdapterConfig = loaded
        .config
        .providers
        .iter()
        .find(|provider| provider.id.as_str() == "work")
        .unwrap()
        .deserialize()
        .unwrap();
    assert_eq!(work.client_id, "environment-id");
}

#[test]
fn legacy_overlay_alias_never_targets_a_fake_provider_named_spotify() {
    let source = r#"
[providers]
default = "spotify"
[providers.spotify]
type = "fake"
[providers.work]
type = "spotify"
client_id = "file-id"
redirect_uri = "http://localhost/callback"
"#;
    let loaded = load_str_with_overrides(
        test_path(),
        source,
        &EnvOverrides::default(),
        &ConfigOverrides::parse(["client_id=\"overlay-id\""]).unwrap(),
    )
    .unwrap();

    let fake = loaded
        .config
        .providers
        .iter()
        .find(|provider| provider.id.as_str() == "spotify")
        .unwrap();
    assert!(fake.raw_table().get("client_id").is_none());
    let work: SpotifyAdapterConfig = loaded
        .config
        .providers
        .iter()
        .find(|provider| provider.id.as_str() == "work")
        .unwrap()
        .deserialize()
        .unwrap();
    assert_eq!(work.client_id, "overlay-id");
}

#[test]
fn legacy_spotify_keys_reject_a_non_spotify_provider_namespace_collision() {
    let error = load_str(
        test_path(),
        r#"
client_id = "legacy-secret"
[providers.spotify]
type = "fake"
[providers.work]
type = "spotify"
client_id = "work-id"
redirect_uri = "http://localhost/callback"
"#,
        &EnvOverrides::default(),
    )
    .expect_err("legacy credentials must not be merged into a fake adapter");

    assert!(error.to_string().contains("conflict"));
    assert!(!error.to_string().contains("legacy-secret"));
}

#[test]
fn legacy_spotify_keys_follow_a_sole_custom_spotify_adapter() {
    let loaded = load_str(
        test_path(),
        r#"
client_id = "legacy-id"
redirect_uri = "http://localhost/callback"
[providers.work]
type = "spotify"
"#,
        &EnvOverrides::default(),
    )
    .expect("sole custom Spotify adapter is an unambiguous legacy target");

    assert_eq!(loaded.config.default_provider.unwrap().as_str(), "work");
    let work: SpotifyAdapterConfig = loaded.config.providers[0].deserialize().unwrap();
    assert_eq!(work.client_id, "legacy-id");
    assert!(loaded.warnings.iter().any(|warning| matches!(
        warning,
        ConfigWarning::DeprecatedKey { canonical_path, .. }
            if canonical_path == "providers.work.client_id"
    )));
}

#[test]
fn legacy_editor_alias_targets_custom_spotify_and_never_fake_namespace() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("spotuify.toml");
    fs::write(
        &path,
        r#"[providers]
default = "spotify"
[providers.spotify]
type = "fake"
[providers.work]
type = "spotify"
client_id = "old-work-id"
redirect_uri = "http://localhost/callback"
"#,
    )
    .unwrap();
    let alias = ConfigPath::parse("client_id").unwrap();

    set_config_value_at(&path, &alias, "new-work-id").unwrap();

    let edited = fs::read_to_string(&path).unwrap();
    let parsed = toml::from_str::<toml::Table>(&edited).unwrap();
    assert!(parsed["providers"]["spotify"].get("client_id").is_none());
    assert_eq!(
        parsed["providers"]["work"]["client_id"].as_str(),
        Some("new-work-id")
    );
    assert_eq!(
        get_config_value_at(&path, &alias)
            .unwrap()
            .unwrap()
            .expose(),
        "new-work-id"
    );
    assert_eq!(
        get_effective_config_value_at(
            &path,
            &alias,
            &EnvOverrides::default(),
            &ConfigOverrides::default(),
        )
        .unwrap()
        .unwrap()
        .expose(),
        "new-work-id"
    );
}

#[test]
fn ambiguous_legacy_editor_alias_fails_without_writing() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("spotuify.toml");
    let original = r#"[providers]
default = "left"
[providers.left]
type = "spotify"
client_id = "left-id"
[providers.right]
type = "spotify"
client_id = "right-id"
"#;
    fs::write(&path, original).unwrap();

    let error = set_config_value_at(&path, &ConfigPath::parse("client_id").unwrap(), "new-id")
        .expect_err("legacy alias must not guess between adapters");

    assert!(error.to_string().contains("ambiguous"));
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn migration_targets_a_sole_custom_spotify_provider() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("spotuify.toml");
    fs::write(
        &path,
        r#"client_id = "legacy-id"
redirect_uri = "http://localhost/callback"
[providers.work]
type = "spotify"
"#,
    )
    .unwrap();

    migrate_legacy_config_at(&path).unwrap();

    let migrated = fs::read_to_string(&path).unwrap();
    let parsed = toml::from_str::<toml::Table>(&migrated).unwrap();
    assert_eq!(parsed["providers"]["default"].as_str(), Some("work"));
    assert_eq!(
        parsed["providers"]["work"]["client_id"].as_str(),
        Some("legacy-id")
    );
    assert!(parsed["providers"].get("spotify").is_none());
}

#[test]
fn failed_prospective_edit_preserves_bytes_and_success_removes_every_alias() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("spotuify.toml");
    let original = r#"client_id = "legacy"
[player]
bitrate = 96
[providers]
default = "spotify"
[providers.spotify]
type = "spotify"
client_id = "canonical"
redirect_uri = "http://localhost/callback"
[providers.spotify.player]
bitrate = 160
"#;
    fs::write(&path, original).unwrap();
    let bitrate = ConfigPath::parse("providers.spotify.player.bitrate").unwrap();
    assert!(set_config_value_at(&path, &bitrate, "4294967296").is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), original);

    set_config_value_at(&path, &bitrate, "320").unwrap();
    let edited = fs::read_to_string(&path).unwrap();
    assert!(edited.contains("bitrate = 320"));
    assert!(!edited.contains("[player]"));
    assert!(!edited.contains("bitrate = 96"));
}

#[test]
fn dynamic_secret_names_fail_closed() {
    for path in [
        "providers.custom.oauth-credential",
        "providers.custom.private-key",
        "providers.custom.authorization",
        "providers.custom.session-token",
    ] {
        assert!(ConfigPath::parse(path).unwrap().is_secret(), "{path}");
    }
    assert!(!ConfigPath::parse("providers.custom.endpoint")
        .unwrap()
        .is_secret());
}

#[test]
fn effective_getter_includes_defaults_and_overlays_while_raw_getter_stays_file_only() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("spotuify.toml");
    fs::write(
        &path,
        "[providers.local]\ntype = \"fake\"\ncustom_token = \"disk-secret\"\n",
    )
    .unwrap();
    let overrides = ConfigOverrides::parse(["viz.target_fps=24"]).unwrap();
    let loaded =
        crate::load_from_with_overrides(&path, &EnvOverrides::default(), &overrides).unwrap();

    let fps = ConfigPath::parse("viz.target_fps").unwrap();
    assert!(get_config_value_at(&path, &fps).unwrap().is_none());
    assert_eq!(loaded.effective_value(&fps).unwrap().expose(), "24");
    let token = ConfigPath::parse("providers.local.custom_token").unwrap();
    assert_eq!(
        loaded.effective_value(&token).unwrap().to_string(),
        "<redacted>"
    );
    assert!(fs::read_to_string(&path).unwrap().contains("disk-secret"));
}

#[test]
fn set_uses_canonical_alias_and_preserves_comments_order_and_unknown_fields() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("spotuify.toml");
    fs::write(
        &path,
        r#"# keep this header
[providers]
default = "archive"

# keep archive first
[providers.archive]
type = "filesystem"
custom_unknown = "untouched"

[providers.spotify]
type = "spotify"
client_id = "secret-looking-but-not-secret"
"#,
    )
    .expect("write fixture");

    let alias = ConfigPath::parse("player.bitrate").expect("legacy alias");
    assert!(alias.was_alias());
    assert_eq!(alias.canonical(), "providers.spotify.player.bitrate");
    set_config_value_at(&path, &alias, "160").expect("set canonical value");

    let edited = fs::read_to_string(&path).expect("read edited config");
    assert!(edited.starts_with("# keep this header"));
    assert!(edited.contains("# keep archive first"));
    assert!(edited.contains("custom_unknown = \"untouched\""));
    assert!(edited.find("[providers.archive]") < edited.find("[providers.spotify]"));
    assert!(edited.contains("[providers.spotify.player]"));
    assert!(edited.contains("bitrate = 160"));

    let value = get_config_value_at(&path, &alias)
        .expect("get alias")
        .expect("value exists");
    assert_eq!(value.expose(), "160");
    assert_eq!(value.to_string(), "160");
}

#[test]
fn secret_values_are_redacted_but_remain_available_to_authorized_callers() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("spotuify.toml");
    fs::write(
        &path,
        r#"
[providers.spotify]
client_secret = "do-not-print"
"#,
    )
    .expect("write fixture");
    let secret = ConfigPath::parse("client_secret").expect("legacy secret alias");
    let value = get_config_value_at(&path, &secret)
        .expect("get secret")
        .expect("secret exists");

    assert!(secret.is_secret());
    assert!(value.is_secret());
    assert_eq!(value.expose(), "do-not-print");
    assert_eq!(value.to_string(), "<redacted>");
    assert!(!format!("{value:?}").contains("do-not-print"));

    set_config_value_at(&path, &secret, "").expect("clear legacy secret through alias");
    assert!(get_config_value_at(&path, &secret)
        .expect("get cleared secret")
        .is_none());
    assert!(!fs::read_to_string(&path)
        .expect("read cleared config")
        .contains("do-not-print"));
}

#[test]
fn migration_moves_owned_keys_but_retains_unknown_legacy_and_provider_data() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("spotuify.toml");
    fs::write(
        &path,
        r#"# retain header
client_id = "legacy-id"
redirect_uri = "http://localhost/legacy"

[player]
bitrate = 96
event_hook = "legacy-hook"
adapter_extension = "keep-me"

[spotifyd]
device_name = "spotifyd-device"
zeroconf_port = 1234

[providers.archive]
type = "filesystem"
unknown = "still-here"
"#,
    )
    .expect("write fixture");

    let report = migrate_legacy_config_at(&path).expect("migrate legacy config");
    assert!(report.changed);
    assert!(report
        .retained_legacy_tables
        .contains(&"player".to_string()));
    assert!(report
        .retained_legacy_tables
        .contains(&"spotifyd".to_string()));
    assert!(report.changes.iter().any(|change| {
        change.legacy_path == "client_id"
            && change.canonical_path == "providers.spotify.client_id"
            && change.outcome == MigrationOutcome::Migrated
    }));

    let migrated = fs::read_to_string(&path).expect("read migrated config");
    assert!(migrated.starts_with("# retain header"));
    let migrated_document = migrated.parse::<toml_edit::DocumentMut>().unwrap();
    assert!(migrated_document.get("client_id").is_none());
    assert!(migrated.contains("adapter_extension = \"keep-me\""));
    assert!(migrated.contains("zeroconf_port = 1234"));
    assert!(migrated.contains("unknown = \"still-here\""));

    let loaded =
        load_str(&path, &migrated, &EnvOverrides::default()).expect("migrated config should load");
    assert!(loaded.warnings.is_empty());
    let spotify = loaded
        .config
        .providers
        .iter()
        .find(|provider| provider.id.as_str() == "spotify")
        .expect("migrated Spotify provider");
    let spotify: SpotifyAdapterConfig = spotify.deserialize().expect("Spotify adapter config");
    assert_eq!(spotify.client_id, "legacy-id");
    assert_eq!(spotify.player.bitrate, 96);
    assert_eq!(
        loaded.config.analytics.hook_command.as_deref(),
        Some("legacy-hook")
    );
}

#[test]
fn initialization_and_atomic_updates_keep_private_file_mode() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("nested").join("spotuify.toml");
    init_config_at(&path).expect("initialize config");
    let setting = ConfigPath::parse("notifications.enabled").expect("config path");
    set_config_value_at(&path, &setting, "true").expect("atomic update");

    assert!(fs::read_to_string(&path)
        .expect("read config")
        .contains("enabled = true"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
            0o600
        );
        let lock_path = path.with_file_name(".spotuify.toml.lock");
        assert_eq!(
            fs::metadata(lock_path)
                .expect("lock metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert_eq!(
        fs::read_dir(path.parent().expect("parent"))
            .expect("directory entries")
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".tmp"))
            .count(),
        0
    );
}
