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

// Phase 6/Phase 7 architectural cut: analytics types + pure event
// builders live in spotuify-core. Re-exported here so existing
// binary call sites (`crate::analytics::now_ms`, etc.) keep
// compiling during the file-motion phase.
pub use spotuify_core::{
    action_finished_event, now_ms, redact_spotify_path, search_performed_event,
    spotify_api_finished_event, AnalyticsEvent, AnalyticsEventKind, AnalyticsSink, AnalyticsSource,
    StoredAnalyticsEvent,
};

#[async_trait::async_trait]
impl AnalyticsSink for AnalyticsStore {
    async fn record(&self, event: &AnalyticsEvent) {
        if let Err(err) = self.record_event(event).await {
            tracing::warn!(error = %err, "analytics store failed to persist event");
        }
    }
}

const BUSY_TIMEOUT: Duration = Duration::from_secs(30);
const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub struct AnalyticsStore {
    writer: SqlitePool,
    reader: SqlitePool,
}

// StoredAnalyticsEvent moved to spotuify_core::analytics; re-exported
// at the top of this file.

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

    /// Trait-friendly variant used by the AnalyticsSink impl below.
    /// The inherent method keeps the Result-returning signature for
    /// callers that want to handle DB errors.
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
                    kind: row.get::<String, _>("kind").parse().map_err(anyhow::Error::msg)?,
                    occurred_at_ms: row.get("occurred_at_ms"),
                    received_at_ms: row.get("received_at_ms"),
                    source: row.get::<String, _>("source").parse().map_err(anyhow::Error::msg)?,
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

// now_ms moved to spotuify_core::analytics; re-exported from this
// module so existing `crate::analytics::now_ms` call sites keep
// compiling during the file-motion phase.

// search_performed_event, action_finished_event, redact_spotify_path,
// normalize_search_query, sha256_hex all moved to
// spotuify_core::analytics. Re-exports at the top of this file.

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
