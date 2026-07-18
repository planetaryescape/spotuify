use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use fs2::FileExt;
use tempfile::NamedTempFile;
use toml_edit::{value, DocumentMut, Item, Table, Value};

use crate::model::validate_legacy_backend;
use crate::{ConfigError, ConfigOverrides, ConfigPath, ConfigValue, EnvOverrides, Result};

const LOCK_TIMEOUT: Duration = Duration::from_secs(10);
const LOCK_RETRY: Duration = Duration::from_millis(25);

pub const CONFIG_TEMPLATE: &str = r#"# spotuify configuration

[providers]
default = "spotify"

[providers.spotify]
type = "spotify"
client_id = ""
redirect_uri = "http://127.0.0.1:8888/callback"

[providers.spotify.player]
bitrate = 320
normalization = false
audio_cache_mib = 0
pulse_props = true

[cache]
cover_cache_mb = 200
cover_cache_ttl_days = 30

[analytics]
store_raw_queries = true
retention_progress_days = 90
retention_events_days = 365
retention_operations_days = 90
daily_rollup_hour = 3
hook_timeout_ms = 5000
allow_file_credentials = false

[notifications]
enabled = false
summary = "{track}"
body = "{artist} - {album}"
on_track_change = true
on_pause = false
on_resume = false
on_skip = false
on_error = true

[discord]
enabled = false

[viz]
enabled = true
source = "auto"
target_fps = 30
smoothing = 0.5
noise_gate = 0.005
color_scheme = "spotify-green"
"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MigrationOutcome {
    Migrated,
    RemovedDuplicate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigrationChange {
    pub legacy_path: String,
    pub canonical_path: String,
    pub outcome: MigrationOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigrationReport {
    pub path: PathBuf,
    pub changed: bool,
    pub changes: Vec<MigrationChange>,
    /// Legacy tables retained because they contain fields this crate does not own.
    pub retained_legacy_tables: Vec<String>,
}

pub fn config_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("SPOTUIFY_CONFIG") {
        if path.is_empty() {
            return Err(ConfigError::Invalid(
                "SPOTUIFY_CONFIG cannot be blank".to_string(),
            ));
        }
        return Ok(PathBuf::from(path));
    }
    Ok(spotuify_protocol::paths::config_dir().join("spotuify.toml"))
}

pub fn init_config() -> Result<PathBuf> {
    let path = config_path()?;
    init_config_at(&path)?;
    Ok(path)
}

pub fn init_config_at(path: &Path) -> Result<()> {
    ensure_parent(path)?;
    let _lock = ConfigLock::acquire(path)?;
    if !path.exists() {
        atomic_write(path, CONFIG_TEMPLATE)?;
    } else {
        secure_file(path)?;
    }
    Ok(())
}

pub fn get_config_value(path: &ConfigPath) -> Result<Option<ConfigValue>> {
    get_config_value_at(&config_path()?, path)
}

pub fn get_config_value_at(file_path: &Path, path: &ConfigPath) -> Result<Option<ConfigValue>> {
    let document = read_document(file_path)?;
    for candidate in document_read_candidates(&document, path)? {
        let segments = candidate.split('.').collect::<Vec<_>>();
        if let Some(item) = item_at(document.as_table(), &segments) {
            if let Some(rendered) = render_item(item) {
                return Ok(Some(ConfigValue::new(rendered, path.is_secret())));
            }
        }
    }
    Ok(None)
}

pub fn get_effective_config_value(path: &ConfigPath) -> Result<Option<ConfigValue>> {
    get_effective_config_value_at(
        &config_path()?,
        path,
        &EnvOverrides::from_process(),
        &ConfigOverrides::from_process()?,
    )
}

pub fn get_effective_config_value_at(
    file_path: &Path,
    path: &ConfigPath,
    env: &EnvOverrides,
    overrides: &ConfigOverrides,
) -> Result<Option<ConfigValue>> {
    let document = read_document(file_path)?;
    let effective_path = ConfigPath::parse(&resolve_document_path(&document, path)?)?;
    Ok(
        crate::load_from_with_overrides(file_path, env, overrides)?
            .effective_value(&effective_path),
    )
}

pub fn set_config_value(path: &ConfigPath, raw: &str) -> Result<()> {
    set_config_value_at(&config_path()?, path, raw)
}

pub fn set_config_value_at(file_path: &Path, path: &ConfigPath, raw: &str) -> Result<()> {
    ensure_parent(file_path)?;
    let _lock = ConfigLock::acquire(file_path)?;
    let mut document = read_document_or_template(file_path)?;
    let item = parse_config_item(path, raw)?;
    let target = resolve_document_path(&document, path)?;
    let segments = target.split('.').collect::<Vec<_>>();
    match item {
        Some(item) => {
            set_item(document.as_table_mut(), &segments, item, path.canonical())?;
            for alias in document_aliases_to_remove(path, &target) {
                let alias_segments = alias.split('.').collect::<Vec<_>>();
                remove_item(document.as_table_mut(), &alias_segments);
            }
        }
        None => {
            let mut candidates = document_aliases_to_remove(path, &target);
            candidates.insert(0, target);
            for candidate in candidates {
                let candidate_segments = candidate.split('.').collect::<Vec<_>>();
                remove_item(document.as_table_mut(), &candidate_segments);
            }
        }
    }
    let prospective = document.to_string();
    crate::load_str(file_path, &prospective, &EnvOverrides::default())?;
    atomic_write(file_path, &prospective)
}

fn document_read_candidates(document: &DocumentMut, path: &ConfigPath) -> Result<Vec<String>> {
    let target = resolve_document_path(document, path)?;
    let mut candidates = vec![target.clone()];
    candidates.extend(path.read_candidates().into_iter().filter(|candidate| {
        target == path.canonical() || !candidate.starts_with("providers.spotify.")
    }));
    let mut seen = std::collections::HashSet::new();
    Ok(candidates
        .into_iter()
        .filter(|candidate| seen.insert(candidate.clone()))
        .collect())
}

fn document_aliases_to_remove(path: &ConfigPath, target: &str) -> Vec<String> {
    path.read_candidates()
        .into_iter()
        .filter(|candidate| candidate != target)
        .filter(|candidate| {
            target == path.canonical() || !candidate.starts_with("providers.spotify.")
        })
        .collect()
}

fn resolve_document_path(document: &DocumentMut, path: &ConfigPath) -> Result<String> {
    if !path.is_legacy_provider_alias() || !path.canonical().starts_with("providers.spotify.") {
        return Ok(path.canonical().to_string());
    }

    let Some(providers) = document.get("providers").and_then(Item::as_table) else {
        return Ok(path.canonical().to_string());
    };
    let provider_kind = |id: &str| {
        providers
            .get(id)
            .and_then(Item::as_table)
            .and_then(|table| table.get("type"))
            .and_then(Item::as_value)
            .and_then(Value::as_str)
    };
    if provider_kind("spotify").is_some_and(|kind| kind == "spotify") {
        return Ok(path.canonical().to_string());
    }
    let spotify_ids = providers
        .iter()
        .filter(|(_, item)| {
            item.as_table()
                .and_then(|table| table.get("type"))
                .and_then(Item::as_value)
                .and_then(Value::as_str)
                .is_some_and(|kind| kind == "spotify")
        })
        .map(|(id, _)| id.to_string())
        .collect::<Vec<_>>();
    let target = match spotify_ids.as_slice() {
        [only] => only,
        [] if provider_kind("spotify").is_none() => return Ok(path.canonical().to_string()),
        [] => {
            return Err(ConfigError::Invalid(
                "legacy Spotify config path conflicts with non-Spotify provider `spotify`"
                    .to_string(),
            ));
        }
        _ => {
            return Err(ConfigError::Invalid(
                "legacy Spotify config path is ambiguous; use providers.<id> explicitly"
                    .to_string(),
            ));
        }
    };
    Ok(path
        .canonical()
        .replacen("providers.spotify.", &format!("providers.{target}."), 1))
}

pub fn migrate_legacy_config() -> Result<MigrationReport> {
    migrate_legacy_config_at(&config_path()?)
}

pub fn migrate_legacy_config_at(path: &Path) -> Result<MigrationReport> {
    ensure_parent(path)?;
    let _lock = ConfigLock::acquire(path)?;
    let was_missing = !path.exists();
    let leading_trivia = if was_missing {
        String::new()
    } else {
        fs::read_to_string(path)
            .map(|contents| leading_document_trivia(&contents).to_string())
            .map_err(|source| ConfigError::Io {
                path: path.to_path_buf(),
                source,
            })?
    };
    let mut document = read_document_or_template(path)?;
    let mut changes = Vec::new();
    let has_legacy = ["client_id", "client_secret", "redirect_uri"]
        .into_iter()
        .chain([
            "player.backend",
            "player.bitrate",
            "player.device_name",
            "player.audio_output_device",
            "player.normalization",
            "player.audio_cache_mib",
            "player.pulse_props",
            "player.event_hook",
            "spotifyd.device_name",
        ])
        .any(|path| {
            let segments = path.split('.').collect::<Vec<_>>();
            item_at(document.as_table(), &segments).is_some()
        });
    let spotify_target = if has_legacy {
        resolve_document_path(&document, &ConfigPath::parse("client_id")?)?
            .strip_prefix("providers.")
            .and_then(|path| path.strip_suffix(".client_id"))
            .ok_or_else(|| ConfigError::Invalid("failed to resolve Spotify provider path".into()))?
            .to_string()
    } else {
        "spotify".to_string()
    };

    for key in ["client_id", "client_secret", "redirect_uri"] {
        move_path(
            &mut document,
            key,
            &format!("providers.{spotify_target}.{key}"),
            &mut changes,
        )?;
    }
    for key in [
        "backend",
        "bitrate",
        "device_name",
        "audio_output_device",
        "normalization",
        "audio_cache_mib",
        "pulse_props",
    ] {
        move_path(
            &mut document,
            &format!("player.{key}"),
            &format!("providers.{spotify_target}.player.{key}"),
            &mut changes,
        )?;
    }
    move_path(
        &mut document,
        "player.event_hook",
        "analytics.hook_command",
        &mut changes,
    )?;
    move_path(
        &mut document,
        "spotifyd.device_name",
        &format!("providers.{spotify_target}.player.device_name"),
        &mut changes,
    )?;

    if !changes.is_empty() {
        set_if_missing(
            &mut document,
            "providers.default",
            value(spotify_target.clone()),
        )?;
        set_if_missing(
            &mut document,
            &format!("providers.{spotify_target}.type"),
            value("spotify"),
        )?;
        let prospective = restore_leading_document_trivia(document.to_string(), &leading_trivia);
        crate::load_str(path, &prospective, &EnvOverrides::default())?;
        atomic_write(path, &prospective)?;
    } else if was_missing {
        atomic_write(path, &document.to_string())?;
    } else {
        secure_file(path)?;
    }

    let retained_legacy_tables = ["player", "spotifyd"]
        .into_iter()
        .filter(|table| document.as_table().contains_key(table))
        .map(ToOwned::to_owned)
        .collect();
    Ok(MigrationReport {
        path: path.to_path_buf(),
        changed: !changes.is_empty(),
        changes,
        retained_legacy_tables,
    })
}

fn leading_document_trivia(contents: &str) -> &str {
    let mut end = 0;
    for line in contents.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            end += line.len();
        } else {
            break;
        }
    }
    &contents[..end]
}

fn restore_leading_document_trivia(mut rendered: String, leading: &str) -> String {
    if leading.is_empty() || rendered.starts_with(leading) {
        return rendered;
    }
    if let Some(index) = rendered.find(leading) {
        rendered.replace_range(index..index + leading.len(), "");
    }
    format!("{leading}{rendered}")
}

fn parse_config_item(path: &ConfigPath, raw: &str) -> Result<Option<Item>> {
    let canonical = path.canonical();
    let trimmed = raw.trim();
    if is_optional_string(canonical) && trimmed.is_empty() {
        return Ok(None);
    }

    let parsed = if is_boolean(canonical) {
        value(
            trimmed
                .parse::<bool>()
                .map_err(|_| ConfigError::Invalid(format!("{canonical} must be true or false")))?,
        )
    } else if is_integer(canonical) {
        let parsed = trimmed
            .parse::<i64>()
            .map_err(|_| ConfigError::Invalid(format!("{canonical} must be an integer")))?;
        validate_integer(canonical, parsed)?;
        value(parsed)
    } else if is_float(canonical) {
        let parsed = trimmed
            .parse::<f64>()
            .map_err(|_| ConfigError::Invalid(format!("{canonical} must be a number")))?;
        validate_float(canonical, parsed)?;
        value(parsed)
    } else if is_string(canonical) {
        if trimmed.is_empty() && is_required_string(canonical) {
            return Err(ConfigError::Invalid(format!("{canonical} cannot be blank")));
        }
        validate_string(canonical, trimmed)?;
        value(trimmed)
    } else {
        Item::Value(
            trimmed
                .parse::<Value>()
                .unwrap_or_else(|_| Value::from(raw)),
        )
    };
    Ok(Some(parsed))
}

fn is_optional_string(path: &str) -> bool {
    matches!(
        path,
        "analytics.hook_command"
            | "analytics.lastfm_api_key"
            | "analytics.lastfm_user"
            | "notifications.summary"
            | "notifications.body"
            | "discord.application_id"
    ) || path.ends_with(".client_secret")
        || path.ends_with(".redirect_uri")
        || path.ends_with(".player.device_name")
        || path.ends_with(".player.audio_output_device")
}

fn is_required_string(path: &str) -> bool {
    path == "providers.default" || path.ends_with(".type") || path.ends_with(".client_id")
}

fn is_string(path: &str) -> bool {
    is_optional_string(path)
        || is_required_string(path)
        || matches!(
            path,
            "notifications.summary" | "notifications.body" | "viz.source" | "viz.color_scheme"
        )
        || path.ends_with(".player.backend")
}

fn is_boolean(path: &str) -> bool {
    matches!(
        path,
        "analytics.store_raw_queries"
            | "analytics.allow_file_credentials"
            | "notifications.enabled"
            | "notifications.on_track_change"
            | "notifications.on_pause"
            | "notifications.on_resume"
            | "notifications.on_skip"
            | "notifications.on_error"
            | "discord.enabled"
            | "viz.enabled"
    ) || path.ends_with(".player.normalization")
        || path.ends_with(".player.pulse_props")
}

fn is_integer(path: &str) -> bool {
    matches!(
        path,
        "cache.cover_cache_mb"
            | "cache.cover_cache_ttl_days"
            | "analytics.retention_progress_days"
            | "analytics.retention_events_days"
            | "analytics.retention_operations_days"
            | "analytics.daily_rollup_hour"
            | "analytics.hook_timeout_ms"
            | "viz.target_fps"
    ) || path.ends_with(".player.bitrate")
        || path.ends_with(".player.audio_cache_mib")
}

fn is_float(path: &str) -> bool {
    matches!(path, "viz.smoothing" | "viz.noise_gate")
}

fn validate_integer(path: &str, value: i64) -> Result<()> {
    if path.ends_with(".player.bitrate") && !matches!(value, 96 | 160 | 320) {
        return Err(ConfigError::Invalid(
            "player bitrate must be one of 96, 160, 320".to_string(),
        ));
    }
    if value < 0 {
        return Err(ConfigError::Invalid(format!("{path} cannot be negative")));
    }
    let max = if path.ends_with(".player.bitrate")
        || path.ends_with(".player.audio_cache_mib")
        || matches!(
            path,
            "analytics.retention_progress_days"
                | "analytics.retention_events_days"
                | "analytics.retention_operations_days"
        ) {
        u32::MAX as i64
    } else if matches!(path, "analytics.daily_rollup_hour" | "viz.target_fps") {
        u8::MAX as i64
    } else {
        i64::MAX
    };
    if value > max {
        return Err(ConfigError::Invalid(format!(
            "{path} exceeds its supported integer range"
        )));
    }
    if path == "analytics.daily_rollup_hour" && value > 23 {
        return Err(ConfigError::Invalid(
            "analytics.daily_rollup_hour must be between 0 and 23".to_string(),
        ));
    }
    if path == "cache.cover_cache_ttl_days" && value == 0 {
        return Err(ConfigError::Invalid(
            "cache.cover_cache_ttl_days must be greater than zero".to_string(),
        ));
    }
    if path == "viz.target_fps" && !(1..=60).contains(&value) {
        return Err(ConfigError::Invalid(
            "viz.target_fps must be between 1 and 60".to_string(),
        ));
    }
    Ok(())
}

fn validate_string(path: &str, value: &str) -> Result<()> {
    if path.ends_with(".player.backend") {
        validate_legacy_backend(value).map_err(ConfigError::Invalid)?;
    }
    if path == "providers.default" || path.ends_with(".type") {
        spotuify_core::ProviderId::new(value)
            .map_err(|error| ConfigError::Invalid(error.to_string()))?;
    }
    if path == "viz.source" && !matches!(value, "auto" | "sink" | "loopback" | "none") {
        return Err(ConfigError::Invalid(
            "viz.source must be one of auto, sink, loopback, none".to_string(),
        ));
    }
    if path == "viz.color_scheme" && !matches!(value, "spotify-green" | "rainbow" | "monochrome") {
        return Err(ConfigError::Invalid(
            "viz.color_scheme must be one of spotify-green, rainbow, monochrome".to_string(),
        ));
    }
    Ok(())
}

fn validate_float(path: &str, value: f64) -> Result<()> {
    let valid = match path {
        "viz.smoothing" => (0.0..=0.95).contains(&value),
        "viz.noise_gate" => (0.0..=1.0).contains(&value),
        _ => true,
    };
    if !valid {
        return Err(ConfigError::Invalid(format!(
            "{path} is outside its supported range"
        )));
    }
    Ok(())
}

fn move_path(
    document: &mut DocumentMut,
    legacy: &str,
    canonical: &str,
    changes: &mut Vec<MigrationChange>,
) -> Result<()> {
    let legacy_segments = legacy.split('.').collect::<Vec<_>>();
    let Some(legacy_item) = item_at(document.as_table(), &legacy_segments).cloned() else {
        return Ok(());
    };
    let canonical_segments = canonical.split('.').collect::<Vec<_>>();
    let canonical_exists = item_at(document.as_table(), &canonical_segments).is_some();
    if !canonical_exists {
        set_item(
            document.as_table_mut(),
            &canonical_segments,
            legacy_item,
            canonical,
        )?;
    }
    remove_item(document.as_table_mut(), &legacy_segments);
    changes.push(MigrationChange {
        legacy_path: legacy.to_string(),
        canonical_path: canonical.to_string(),
        outcome: if canonical_exists {
            MigrationOutcome::RemovedDuplicate
        } else {
            MigrationOutcome::Migrated
        },
    });
    Ok(())
}

fn set_if_missing(document: &mut DocumentMut, path: &str, item: Item) -> Result<()> {
    let segments = path.split('.').collect::<Vec<_>>();
    if item_at(document.as_table(), &segments).is_none() {
        set_item(document.as_table_mut(), &segments, item, path)?;
    }
    Ok(())
}

fn item_at<'a>(table: &'a Table, segments: &[&str]) -> Option<&'a Item> {
    let (first, tail) = segments.split_first()?;
    let item = table.get(first)?;
    if tail.is_empty() {
        Some(item)
    } else {
        item.as_table().and_then(|table| item_at(table, tail))
    }
}

fn set_item(table: &mut Table, segments: &[&str], item: Item, path: &str) -> Result<()> {
    let Some((first, tail)) = segments.split_first() else {
        return Err(ConfigError::InvalidPath {
            path: path.to_string(),
            message: "path cannot be blank".to_string(),
        });
    };
    if tail.is_empty() {
        table.insert(first, item);
        return Ok(());
    }
    if !table.contains_key(first) {
        table.insert(first, Item::Table(Table::new()));
    }
    let child = table
        .get_mut(first)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| ConfigError::InvalidPath {
            path: path.to_string(),
            message: format!("`{first}` is a value, not a table"),
        })?;
    set_item(child, tail, item, path)
}

fn remove_item(table: &mut Table, segments: &[&str]) -> Option<Item> {
    let (first, tail) = segments.split_first()?;
    if tail.is_empty() {
        return table.remove(first);
    }
    let child = table.get_mut(first)?.as_table_mut()?;
    let removed = remove_item(child, tail);
    if child.is_empty() {
        table.remove(first);
    }
    removed
}

fn render_item(item: &Item) -> Option<String> {
    match item {
        Item::Value(Value::String(value)) => Some(value.value().clone()),
        Item::Value(value) => {
            let mut value = value.clone();
            value.decor_mut().clear();
            Some(value.to_string())
        }
        Item::ArrayOfTables(_) | Item::Table(_) | Item::None => None,
    }
}

fn read_document(path: &Path) -> Result<DocumentMut> {
    match fs::read_to_string(path) {
        Ok(contents) => parse_document(path, &contents),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(source) => Err(ConfigError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn read_document_or_template(path: &Path) -> Result<DocumentMut> {
    if path.exists() {
        read_document(path)
    } else {
        parse_document(path, CONFIG_TEMPLATE)
    }
}

fn parse_document(path: &Path, contents: &str) -> Result<DocumentMut> {
    contents
        .parse::<DocumentMut>()
        .map_err(|_| ConfigError::EditParse {
            path: path.to_path_buf(),
        })
}

fn ensure_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let existed = parent.exists();
    if existed {
        let metadata = fs::symlink_metadata(parent).map_err(|source| ConfigError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
        if !metadata.file_type().is_dir() {
            return Err(ConfigError::Invalid(format!(
                "config parent {} is not a directory",
                parent.display()
            )));
        }
    }
    fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    if !existed {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|source| {
            ConfigError::Io {
                path: parent.to_path_buf(),
                source,
            }
        })?;
    }
    Ok(())
}

fn secure_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.file_type().is_file() {
        return Err(ConfigError::Invalid(format!(
            "config path {} is not a regular file",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
            ConfigError::Io {
                path: path.to_path_buf(),
                source,
            }
        })?;
    }
    Ok(())
}

fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = NamedTempFile::new_in(parent).map_err(|source| ConfigError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| ConfigError::Io {
                path: temporary.path().to_path_buf(),
                source,
            })?;
    }
    temporary
        .write_all(contents.as_bytes())
        .map_err(|source| ConfigError::Io {
            path: temporary.path().to_path_buf(),
            source,
        })?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|source| ConfigError::Io {
            path: temporary.path().to_path_buf(),
            source,
        })?;
    temporary.persist(path).map_err(|error| ConfigError::Io {
        path: path.to_path_buf(),
        source: error.error,
    })?;
    secure_file(path)?;
    #[cfg(unix)]
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| ConfigError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    Ok(())
}

struct ConfigLock {
    file: File,
}

impl ConfigLock {
    fn acquire(config_path: &Path) -> Result<Self> {
        let lock_path = lock_path(config_path);
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&lock_path).map_err(|source| ConfigError::Io {
            path: lock_path.clone(),
            source,
        })?;
        secure_file(&lock_path)?;
        let started = Instant::now();
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(Self { file }),
                Err(source) if source.kind() == std::io::ErrorKind::WouldBlock => {
                    if started.elapsed() >= LOCK_TIMEOUT {
                        return Err(ConfigError::LockTimeout(lock_path));
                    }
                    thread::sleep(LOCK_RETRY);
                }
                Err(source) => {
                    return Err(ConfigError::Io {
                        path: lock_path,
                        source,
                    });
                }
            }
        }
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn lock_path(config_path: &Path) -> PathBuf {
    let file_name = config_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("spotuify.toml");
    config_path.with_file_name(format!(".{file_name}.lock"))
}
