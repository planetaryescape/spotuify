use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};

const BUSY_TIMEOUT: Duration = Duration::from_secs(30);
const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct AnalyticsStore {
    writer: SqlitePool,
    reader: SqlitePool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyticsEventKind {
    ActionFinished,
    SearchPerformed,
    SearchResultSelected,
    PlaybackStarted,
    PlaybackPaused,
    PlaybackResumed,
    PlaybackSkipped,
    PlaybackCompleted,
    ListenQualified,
    SpotifyApiFinished,
}

impl AnalyticsEventKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::ActionFinished => "action_finished",
            Self::SearchPerformed => "search_performed",
            Self::SearchResultSelected => "search_result_selected",
            Self::PlaybackStarted => "playback_started",
            Self::PlaybackPaused => "playback_paused",
            Self::PlaybackResumed => "playback_resumed",
            Self::PlaybackSkipped => "playback_skipped",
            Self::PlaybackCompleted => "playback_completed",
            Self::ListenQualified => "listen_qualified",
            Self::SpotifyApiFinished => "spotify_api_finished",
        }
    }
}

impl FromStr for AnalyticsEventKind {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "action_finished" => Ok(Self::ActionFinished),
            "search_performed" => Ok(Self::SearchPerformed),
            "search_result_selected" => Ok(Self::SearchResultSelected),
            "playback_started" => Ok(Self::PlaybackStarted),
            "playback_paused" => Ok(Self::PlaybackPaused),
            "playback_resumed" => Ok(Self::PlaybackResumed),
            "playback_skipped" => Ok(Self::PlaybackSkipped),
            "playback_completed" => Ok(Self::PlaybackCompleted),
            "listen_qualified" => Ok(Self::ListenQualified),
            "spotify_api_finished" => Ok(Self::SpotifyApiFinished),
            _ => anyhow::bail!("unknown analytics event kind `{value}`"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyticsSource {
    Cli,
    Tui,
    SpotifyApi,
    Daemon,
}

impl AnalyticsSource {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Tui => "tui",
            Self::SpotifyApi => "spotify_api",
            Self::Daemon => "daemon",
        }
    }
}

impl FromStr for AnalyticsSource {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "cli" => Ok(Self::Cli),
            "tui" => Ok(Self::Tui),
            "spotify_api" => Ok(Self::SpotifyApi),
            "daemon" => Ok(Self::Daemon),
            _ => anyhow::bail!("unknown analytics source `{value}`"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnalyticsEvent {
    pub kind: AnalyticsEventKind,
    pub occurred_at_ms: i64,
    pub source: AnalyticsSource,
    pub subject_uri: Option<String>,
    pub search_query: Option<String>,
    pub search_query_hash: Option<String>,
    pub payload: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredAnalyticsEvent {
    pub id: i64,
    pub kind: AnalyticsEventKind,
    pub occurred_at_ms: i64,
    pub received_at_ms: i64,
    pub source: AnalyticsSource,
    pub subject_uri: Option<String>,
    pub search_query: Option<String>,
    pub search_query_hash: Option<String>,
    pub payload: Value,
}

impl AnalyticsStore {
    pub async fn open_default() -> Result<Self> {
        Self::open(&analytics_db_path()?).await
    }

    pub async fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let db_url = format!("sqlite:{}", db_path.display());
        let write_opts = SqliteConnectOptions::from_str(&db_url)?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(BUSY_TIMEOUT)
            .pragma("foreign_keys", "ON");
        let writer = SqlitePoolOptions::new()
            .max_connections(1)
            .acquire_timeout(POOL_ACQUIRE_TIMEOUT)
            .connect_with(write_opts)
            .await?;
        let read_opts = SqliteConnectOptions::from_str(&db_url)?
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(BUSY_TIMEOUT)
            .pragma("foreign_keys", "ON")
            .read_only(true);
        let reader = SqlitePoolOptions::new()
            .max_connections(4)
            .acquire_timeout(POOL_ACQUIRE_TIMEOUT)
            .connect_with(read_opts)
            .await?;
        let store = Self { writer, reader };
        store.run_migrations().await?;
        Ok(store)
    }

    #[cfg(test)]
    pub async fn in_memory() -> Result<Self> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?
            .journal_mode(SqliteJournalMode::Wal)
            .pragma("foreign_keys", "ON");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        let store = Self {
            writer: pool.clone(),
            reader: pool,
        };
        store.run_migrations().await?;
        Ok(store)
    }

    pub async fn record_event(&self, event: &AnalyticsEvent) -> Result<i64> {
        let payload = serde_json::to_string(&event.payload)?;
        let result = sqlx::query(
            "INSERT INTO analytics_events (
                kind,
                occurred_at_ms,
                received_at_ms,
                source,
                subject_uri,
                search_query,
                search_query_hash,
                payload_json
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(event.kind.label())
        .bind(event.occurred_at_ms)
        .bind(now_ms())
        .bind(event.source.label())
        .bind(&event.subject_uri)
        .bind(&event.search_query)
        .bind(&event.search_query_hash)
        .bind(payload)
        .execute(&self.writer)
        .await?;
        Ok(result.last_insert_rowid())
    }

    pub async fn recent_events(&self, limit: u32) -> Result<Vec<StoredAnalyticsEvent>> {
        let rows = sqlx::query(
            "SELECT
                id,
                kind,
                occurred_at_ms,
                received_at_ms,
                source,
                subject_uri,
                search_query,
                search_query_hash,
                payload_json
            FROM analytics_events
            ORDER BY occurred_at_ms DESC, id DESC
            LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.reader)
        .await?;

        rows.into_iter()
            .map(|row| {
                let payload_json: String = row.get("payload_json");
                Ok(StoredAnalyticsEvent {
                    id: row.get("id"),
                    kind: row.get::<String, _>("kind").parse()?,
                    occurred_at_ms: row.get("occurred_at_ms"),
                    received_at_ms: row.get("received_at_ms"),
                    source: row.get::<String, _>("source").parse()?,
                    subject_uri: row.get("subject_uri"),
                    search_query: row.get("search_query"),
                    search_query_hash: row.get("search_query_hash"),
                    payload: serde_json::from_str(&payload_json)?,
                })
            })
            .collect()
    }

    async fn run_migrations(&self) -> Result<()> {
        sqlx::raw_sql(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                applied_at_ms INTEGER NOT NULL
            );",
        )
        .execute(&self.writer)
        .await?;

        if self.is_migration_applied(1).await? {
            return Ok(());
        }

        sqlx::raw_sql(
            "CREATE TABLE IF NOT EXISTS analytics_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                kind TEXT NOT NULL,
                occurred_at_ms INTEGER NOT NULL,
                received_at_ms INTEGER NOT NULL,
                source TEXT NOT NULL,
                subject_uri TEXT,
                search_query TEXT,
                search_query_hash TEXT,
                payload_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_analytics_events_time
                ON analytics_events(occurred_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_analytics_events_kind_time
                ON analytics_events(kind, occurred_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_analytics_events_subject_time
                ON analytics_events(subject_uri, occurred_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_analytics_events_search_hash_time
                ON analytics_events(search_query_hash, occurred_at_ms DESC);",
        )
        .execute(&self.writer)
        .await?;

        sqlx::query(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?, ?, ?)",
        )
        .bind(1_i64)
        .bind("analytics_events")
        .bind(now_ms())
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    async fn is_migration_applied(&self, version: i64) -> Result<bool> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT version FROM schema_migrations WHERE version = ?")
                .bind(version)
                .fetch_optional(&self.writer)
                .await?;
        Ok(row.is_some())
    }
}

pub fn analytics_db_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("SPOTUIFY_ANALYTICS_DB") {
        return Ok(PathBuf::from(path));
    }
    dirs::data_local_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/share")))
        .map(|dir| dir.join("spotuify/analytics.sqlite3"))
        .ok_or_else(|| anyhow::anyhow!("could not resolve analytics data directory"))
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub fn search_performed_event(
    source: AnalyticsSource,
    query: &str,
    result_count: usize,
    latency_ms: u128,
    occurred_at_ms: i64,
) -> AnalyticsEvent {
    let normalized_query = normalize_search_query(query);
    AnalyticsEvent {
        kind: AnalyticsEventKind::SearchPerformed,
        occurred_at_ms,
        source,
        subject_uri: None,
        search_query: Some(query.to_string()),
        search_query_hash: Some(sha256_hex(&normalized_query)),
        payload: serde_json::json!({
            "normalized_query": normalized_query,
            "result_count": result_count,
            "latency_ms": latency_ms,
        }),
    }
}

pub fn spotify_api_finished_event(
    source: AnalyticsSource,
    method: &str,
    path: &str,
    status: Option<u16>,
    elapsed_ms: u128,
    error_class: Option<&str>,
    occurred_at_ms: i64,
) -> AnalyticsEvent {
    AnalyticsEvent {
        kind: AnalyticsEventKind::SpotifyApiFinished,
        occurred_at_ms,
        source,
        subject_uri: None,
        search_query: None,
        search_query_hash: None,
        payload: serde_json::json!({
            "method": method,
            "path": redact_spotify_path(path),
            "status": status,
            "elapsed_ms": elapsed_ms,
            "error_class": error_class,
        }),
    }
}

pub fn action_finished_event(
    source: AnalyticsSource,
    action: &str,
    subject_uri: Option<&str>,
    result: &str,
    payload: Value,
    occurred_at_ms: i64,
) -> AnalyticsEvent {
    let mut payload = match payload {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    payload.insert("action".to_string(), Value::String(action.to_string()));
    payload.insert("result".to_string(), Value::String(result.to_string()));
    AnalyticsEvent {
        kind: AnalyticsEventKind::ActionFinished,
        occurred_at_ms,
        source,
        subject_uri: subject_uri.map(str::to_string),
        search_query: None,
        search_query_hash: None,
        payload: Value::Object(payload),
    }
}

pub fn redact_spotify_path(path: &str) -> String {
    let Some((base, query)) = path.split_once('?') else {
        return path.to_string();
    };
    let query = query
        .split('&')
        .map(|part| match part.split_once('=') {
            Some((key, _)) if matches!(key, "q" | "uri" | "ids" | "market") => {
                format!("{key}=<redacted>")
            }
            _ => part.to_string(),
        })
        .collect::<Vec<_>>()
        .join("&");
    format!("{base}?{query}")
}

fn normalize_search_query(query: &str) -> String {
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn sha256_hex(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        action_finished_event, search_performed_event, spotify_api_finished_event, AnalyticsEvent,
        AnalyticsEventKind, AnalyticsSource, AnalyticsStore,
    };

    #[tokio::test]
    async fn records_search_event_in_sqlite_with_redacted_query_hash() {
        let store = AnalyticsStore::in_memory().await.unwrap();
        let event = AnalyticsEvent {
            kind: AnalyticsEventKind::SearchPerformed,
            occurred_at_ms: 1_700_000_000_000,
            source: AnalyticsSource::Cli,
            subject_uri: None,
            search_query: Some("therapy songs after midnight".to_string()),
            search_query_hash: Some("hash-123".to_string()),
            payload: json!({"result_count": 12, "latency_ms": 84}),
        };

        store.record_event(&event).await.unwrap();
        let events = store.recent_events(10).await.unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, AnalyticsEventKind::SearchPerformed);
        assert_eq!(events[0].source, AnalyticsSource::Cli);
        assert_eq!(
            events[0].search_query.as_deref(),
            Some("therapy songs after midnight")
        );
        assert_eq!(events[0].search_query_hash.as_deref(), Some("hash-123"));
        assert_eq!(events[0].payload["result_count"], 12);
    }

    #[test]
    fn search_event_preserves_raw_query_but_hashes_normalized_query() {
        let first = search_performed_event(
            AnalyticsSource::Cli,
            " Imagine  Dragons ",
            7,
            42,
            1_700_000_000_000,
        );
        let second = search_performed_event(
            AnalyticsSource::Cli,
            "imagine dragons",
            7,
            42,
            1_700_000_000_000,
        );

        assert_eq!(first.search_query.as_deref(), Some(" Imagine  Dragons "));
        assert_eq!(second.search_query.as_deref(), Some("imagine dragons"));
        assert_eq!(first.search_query_hash, second.search_query_hash);
        assert_eq!(first.payload["normalized_query"], "imagine dragons");
    }

    #[test]
    fn spotify_api_event_redacts_search_query_from_path() {
        let event = spotify_api_finished_event(
            AnalyticsSource::SpotifyApi,
            "GET",
            "/search?q=therapy+songs&type=track&limit=10",
            Some(200),
            37,
            None,
            1_700_000_000_000,
        );

        assert_eq!(event.kind, AnalyticsEventKind::SpotifyApiFinished);
        assert_eq!(event.search_query, None);
        assert_eq!(
            event.payload["path"],
            "/search?q=<redacted>&type=track&limit=10"
        );
        assert_eq!(event.payload["status"], 200);
    }

    #[test]
    fn action_event_records_action_result_and_subject() {
        let event = action_finished_event(
            AnalyticsSource::Cli,
            "play",
            Some("spotify:track:abc"),
            "ok",
            serde_json::json!({"selected_from": "search", "rank": 1}),
            1_700_000_000_000,
        );

        assert_eq!(event.kind, AnalyticsEventKind::ActionFinished);
        assert_eq!(event.subject_uri.as_deref(), Some("spotify:track:abc"));
        assert_eq!(event.payload["action"], "play");
        assert_eq!(event.payload["result"], "ok");
        assert_eq!(event.payload["selected_from"], "search");
    }
}
