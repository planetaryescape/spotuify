use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};

use spotuify_core::{Device, MediaItem, MediaKind, Playback, Playlist};
use spotuify_protocol::{CacheStatus, SearchScopeData, SearchSourceData};

const BUSY_TIMEOUT: Duration = Duration::from_secs(30);
const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);

/// Cache schema version recognised by this binary.
///
/// Bumped on every incompatible migration. A database with
/// `MAX(schema_migrations.version) > CACHE_VERSION` is rejected at
/// startup with a clear error pointing to `spotuify cache reset --confirm`.
///
/// History:
/// - v1: initial schema (Phase 3)
/// - v2: snapshot_id, snapshot_id_at_fetch, freshness_class,
///   sync_generation (Phase 6.4)
/// - v3: receipts table for two-stage mutation lifecycle (Phase 6.6)
pub const CACHE_VERSION: u32 = 3;

#[derive(Clone)]
pub struct Store {
    writer: SqlitePool,
    reader: SqlitePool,
    db_path: PathBuf,
    index_path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct IndexedMediaItem {
    pub item: MediaItem,
    pub liked: bool,
    pub saved: bool,
    pub added_at_ms: Option<i64>,
    pub source: String,
}

impl Store {
    pub async fn open_default() -> Result<Self> {
        Self::open(&cache_db_path()?, &search_index_path()?).await
    }

    pub async fn open(db_path: &Path, index_path: &Path) -> Result<Self> {
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

        let store = Self {
            writer,
            reader,
            db_path: db_path.to_path_buf(),
            index_path: index_path.to_path_buf(),
        };
        store.run_migrations().await?;
        Ok(store)
    }

    /// In-memory store for tests across the workspace. Migrations run.
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
            db_path: PathBuf::from(":memory:"),
            index_path: PathBuf::from(":memory:"),
        };
        store.run_migrations().await?;
        Ok(store)
    }

    pub fn index_path(&self) -> &Path {
        &self.index_path
    }

    pub async fn upsert_media_items(&self, items: &[MediaItem], source: &str) -> Result<u32> {
        let fetched_at_ms = now_ms();
        let mut written = 0;
        for item in items {
            let item_source = item.source.as_deref().unwrap_or(source);
            sqlx::query(
                "INSERT INTO media_items (
                    uri, spotify_id, kind, name, subtitle, context, duration_ms,
                    image_url, source, fetched_at_ms, updated_at_ms
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(uri) DO UPDATE SET
                    spotify_id = excluded.spotify_id,
                    kind = excluded.kind,
                    name = excluded.name,
                    subtitle = excluded.subtitle,
                    context = excluded.context,
                    duration_ms = excluded.duration_ms,
                    image_url = excluded.image_url,
                    source = excluded.source,
                    fetched_at_ms = excluded.fetched_at_ms,
                    updated_at_ms = excluded.updated_at_ms",
            )
            .bind(&item.uri)
            .bind(&item.id)
            .bind(item.kind.label())
            .bind(&item.name)
            .bind(&item.subtitle)
            .bind(&item.context)
            .bind(item.duration_ms as i64)
            .bind(&item.image_url)
            .bind(item_source)
            .bind(fetched_at_ms)
            .bind(fetched_at_ms)
            .execute(&self.writer)
            .await?;
            written += 1;
        }
        Ok(written)
    }

    pub async fn cache_search_results(
        &self,
        query: &str,
        scope: SearchScopeData,
        source: SearchSourceData,
        items: &[MediaItem],
    ) -> Result<u32> {
        self.upsert_media_items(items, source.label()).await?;
        let fetched_at_ms = now_ms();
        let result = sqlx::query(
            "INSERT INTO search_runs (
                query, normalized_query, scope, source, fetched_at_ms, status, result_count
            ) VALUES (?, ?, ?, ?, ?, 'ok', ?)",
        )
        .bind(query)
        .bind(normalize_query(query))
        .bind(scope.label())
        .bind(source.label())
        .bind(fetched_at_ms)
        .bind(items.len() as i64)
        .execute(&self.writer)
        .await?;
        let search_run_id = result.last_insert_rowid();
        for (position, item) in items.iter().enumerate() {
            sqlx::query(
                "INSERT INTO search_results (search_run_id, position, item_uri)
                 VALUES (?, ?, ?)",
            )
            .bind(search_run_id)
            .bind(position as i64)
            .bind(&item.uri)
            .execute(&self.writer)
            .await?;
        }
        Ok(items.len() as u32)
    }

    pub async fn local_search(
        &self,
        query: &str,
        scope: SearchScopeData,
        limit: u32,
    ) -> Result<Vec<MediaItem>> {
        let tokens = query
            .split_whitespace()
            .map(|token| format!("%{}%", token.to_ascii_lowercase()))
            .collect::<Vec<_>>();
        if tokens.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        let mut sql = String::from(
            "SELECT uri, spotify_id, kind, name, subtitle, context, duration_ms,
                    image_url, source, liked, saved, updated_at_ms
             FROM media_items WHERE ",
        );
        if scope != SearchScopeData::All {
            sql.push_str("kind = ? AND ");
        }
        for index in 0..tokens.len() {
            if index > 0 {
                sql.push_str(" AND ");
            }
            sql.push_str("LOWER(name || ' ' || subtitle || ' ' || context || ' ' || uri) LIKE ?");
        }
        sql.push_str(" ORDER BY saved DESC, liked DESC, updated_at_ms DESC, name ASC LIMIT ?");

        let mut statement = sqlx::query(&sql);
        if scope != SearchScopeData::All {
            statement = statement.bind(scope.label());
        }
        for token in &tokens {
            statement = statement.bind(token);
        }
        statement = statement.bind(limit as i64);

        let rows = statement.fetch_all(&self.reader).await?;
        rows.into_iter().map(row_to_media_item).collect()
    }

    pub async fn media_items_by_uris(&self, uris: &[String]) -> Result<Vec<MediaItem>> {
        if uris.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = uris.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let sql = format!(
            "SELECT uri, spotify_id, kind, name, subtitle, context, duration_ms,
                    image_url, source, liked, saved, updated_at_ms
             FROM media_items WHERE uri IN ({placeholders})"
        );
        let mut statement = sqlx::query(&sql);
        for uri in uris {
            statement = statement.bind(uri);
        }
        let rows = statement.fetch_all(&self.reader).await?;
        let mut by_uri = std::collections::HashMap::with_capacity(rows.len());
        for row in rows {
            let item = row_to_media_item(row)?;
            by_uri.insert(item.uri.clone(), item);
        }
        Ok(uris.iter().filter_map(|uri| by_uri.remove(uri)).collect())
    }

    pub async fn list_library_items(&self, limit: u32) -> Result<Vec<MediaItem>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT media_items.uri, spotify_id, media_items.kind, name, subtitle, context,
                    duration_ms, image_url, source, liked, media_items.saved, updated_at_ms
             FROM library_items
             JOIN media_items ON media_items.uri = library_items.item_uri
             WHERE library_items.saved = 1 OR library_items.followed = 1
             ORDER BY library_items.fetched_at_ms DESC, name ASC
             LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.reader)
        .await?;
        rows.into_iter().map(row_to_media_item).collect()
    }

    pub async fn list_media_for_index(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<IndexedMediaItem>> {
        let rows = sqlx::query(
            "SELECT uri, spotify_id, kind, name, subtitle, context, duration_ms,
                    image_url, source, liked, saved, updated_at_ms,
                    COALESCE((SELECT MAX(added_at_ms) FROM playlist_items WHERE item_uri = media_items.uri), updated_at_ms) AS added_at_ms
             FROM media_items
             ORDER BY updated_at_ms DESC, uri ASC
             LIMIT ? OFFSET ?",
        )
        .bind(limit as i64)
        .bind(offset as i64)
        .fetch_all(&self.reader)
        .await?;

        rows.into_iter()
            .map(|row| {
                let liked = row.get::<i64, _>("liked") != 0;
                let saved = row.get::<i64, _>("saved") != 0;
                let source = row.get::<String, _>("source");
                let added_at_ms = row.get::<Option<i64>, _>("added_at_ms");
                Ok(IndexedMediaItem {
                    item: row_to_media_item(row)?,
                    liked,
                    saved,
                    added_at_ms,
                    source,
                })
            })
            .collect()
    }

    pub async fn persist_devices(&self, devices: &[Device]) -> Result<u32> {
        let fetched_at_ms = now_ms();
        for device in devices {
            let device_key = device.id.as_deref().unwrap_or(&device.name);
            sqlx::query(
                "INSERT INTO devices (
                    device_key, id, name, kind, is_active, is_restricted,
                    supports_volume, volume_percent, fetched_at_ms
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(device_key) DO UPDATE SET
                    id = excluded.id,
                    name = excluded.name,
                    kind = excluded.kind,
                    is_active = excluded.is_active,
                    is_restricted = excluded.is_restricted,
                    supports_volume = excluded.supports_volume,
                    volume_percent = excluded.volume_percent,
                    fetched_at_ms = excluded.fetched_at_ms",
            )
            .bind(device_key)
            .bind(&device.id)
            .bind(&device.name)
            .bind(&device.kind)
            .bind(device.is_active)
            .bind(device.is_restricted)
            .bind(device.supports_volume)
            .bind(device.volume_percent.map(i64::from))
            .bind(fetched_at_ms)
            .execute(&self.writer)
            .await?;
        }
        Ok(devices.len() as u32)
    }

    pub async fn persist_playback(&self, playback: &Playback) -> Result<u32> {
        if let Some(item) = &playback.item {
            self.upsert_media_items(std::slice::from_ref(item), "spotify")
                .await?;
        }
        if let Some(device) = &playback.device {
            self.persist_devices(std::slice::from_ref(device)).await?;
        }
        let fetched_at_ms = now_ms();
        let device_key = playback
            .device
            .as_ref()
            .map(|device| device.id.as_deref().unwrap_or(&device.name).to_string());
        sqlx::query(
            "INSERT INTO playback_snapshots (
                item_uri, device_key, is_playing, progress_ms, shuffle, repeat_state, fetched_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(playback.item.as_ref().map(|item| item.uri.as_str()))
        .bind(device_key)
        .bind(playback.is_playing)
        .bind(playback.progress_ms as i64)
        .bind(playback.shuffle)
        .bind(&playback.repeat)
        .bind(fetched_at_ms)
        .execute(&self.writer)
        .await?;
        Ok(1)
    }

    pub async fn persist_playlists(&self, playlists: &[Playlist]) -> Result<u32> {
        let fetched_at_ms = now_ms();
        let media_items = playlists
            .iter()
            .map(playlist_media_item)
            .collect::<Vec<_>>();
        self.upsert_media_items(&media_items, "spotify").await?;
        for playlist in playlists {
            sqlx::query(
                "INSERT INTO playlists (id, uri, name, owner, tracks_total, image_url, fetched_at_ms, snapshot_id)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET
                    uri = excluded.uri,
                    name = excluded.name,
                    owner = excluded.owner,
                    tracks_total = excluded.tracks_total,
                    image_url = excluded.image_url,
                    fetched_at_ms = excluded.fetched_at_ms,
                    snapshot_id = COALESCE(excluded.snapshot_id, playlists.snapshot_id)",
            )
            .bind(&playlist.id)
            .bind(playlist_uri(&playlist.id))
            .bind(&playlist.name)
            .bind(&playlist.owner)
            .bind(playlist.tracks_total as i64)
            .bind(&playlist.image_url)
            .bind(fetched_at_ms)
            .bind(playlist.snapshot_id.as_deref())
            .execute(&self.writer)
            .await?;
        }
        Ok(playlists.len() as u32)
    }

    /// Read the locally cached snapshot_id for a playlist. Phase 6.5
    /// sync gate calls this before deciding whether to refetch tracks.
    pub async fn playlist_snapshot_id(&self, playlist_id: &str) -> Result<Option<String>> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT snapshot_id FROM playlists WHERE id = ?")
                .bind(playlist_id)
                .fetch_optional(&self.reader)
                .await?;
        Ok(row.and_then(|(s,)| s))
    }

    pub async fn persist_playlist_items(
        &self,
        playlist_id: &str,
        items: &[MediaItem],
    ) -> Result<u32> {
        self.upsert_media_items(items, "spotify").await?;
        let added_at_ms = now_ms();
        sqlx::query("DELETE FROM playlist_items WHERE playlist_id = ?")
            .bind(playlist_id)
            .execute(&self.writer)
            .await?;
        for (position, item) in items.iter().enumerate() {
            sqlx::query(
                "INSERT INTO playlist_items (playlist_id, item_uri, position, added_at_ms)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(playlist_id)
            .bind(&item.uri)
            .bind(position as i64)
            .bind(added_at_ms)
            .execute(&self.writer)
            .await?;
        }
        Ok(items.len() as u32)
    }

    pub async fn persist_recent_items(&self, items: &[MediaItem]) -> Result<u32> {
        self.upsert_media_items(items, "spotify").await?;
        let fetched_at_ms = now_ms();
        for (position, item) in items.iter().enumerate() {
            sqlx::query(
                "INSERT OR REPLACE INTO recent_items (item_uri, played_at_ms, fetched_at_ms, position)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(&item.uri)
            .bind(fetched_at_ms.saturating_sub(position as i64))
            .bind(fetched_at_ms)
            .bind(position as i64)
            .execute(&self.writer)
            .await?;
        }
        Ok(items.len() as u32)
    }

    pub async fn persist_library_items(&self, items: &[MediaItem]) -> Result<u32> {
        self.upsert_media_items(items, "spotify").await?;
        let fetched_at_ms = now_ms();
        for item in items {
            sqlx::query(
                "INSERT INTO library_items (item_uri, kind, saved, followed, fetched_at_ms)
                 VALUES (?, ?, 1, 0, ?)
                 ON CONFLICT(item_uri) DO UPDATE SET
                    kind = excluded.kind,
                    saved = 1,
                    fetched_at_ms = excluded.fetched_at_ms",
            )
            .bind(&item.uri)
            .bind(item.kind.label())
            .bind(fetched_at_ms)
            .execute(&self.writer)
            .await?;
            sqlx::query("UPDATE media_items SET saved = 1, liked = 1 WHERE uri = ?")
                .bind(&item.uri)
                .execute(&self.writer)
                .await?;
        }
        Ok(items.len() as u32)
    }

    pub async fn record_sync_event(
        &self,
        domain: &str,
        started_at_ms: i64,
        status: &str,
        row_count: u32,
        error: Option<&str>,
    ) -> Result<()> {
        let finished_at_ms = now_ms();
        sqlx::query(
            "INSERT INTO sync_events (domain, started_at_ms, finished_at_ms, status, row_count, error)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(domain)
        .bind(started_at_ms)
        .bind(finished_at_ms)
        .bind(status)
        .bind(row_count as i64)
        .bind(error)
        .execute(&self.writer)
        .await?;
        sqlx::query(
            "INSERT INTO sync_cursors (domain, last_success_at_ms, last_error)
             VALUES (?, ?, ?)
             ON CONFLICT(domain) DO UPDATE SET
                last_success_at_ms = CASE WHEN ? = 'ok' THEN excluded.last_success_at_ms ELSE sync_cursors.last_success_at_ms END,
                last_error = excluded.last_error",
        )
        .bind(domain)
        .bind(if status == "ok" { Some(finished_at_ms) } else { None })
        .bind(error)
        .bind(status)
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    pub async fn rate_limit_cooldown_remaining_ms(&self, domain: &str) -> Result<Option<i64>> {
        let row: Option<(i64, Option<String>)> = sqlx::query_as(
            "SELECT finished_at_ms, error
             FROM sync_events
             WHERE domain = ? AND error IS NOT NULL
             ORDER BY finished_at_ms DESC
             LIMIT 1",
        )
        .bind(domain)
        .fetch_optional(&self.reader)
        .await?;
        let Some((finished_at_ms, Some(error))) = row else {
            return Ok(None);
        };
        let Some(retry_after_secs) = retry_after_seconds(&error) else {
            return Ok(None);
        };
        let retry_until_ms = finished_at_ms.saturating_add(retry_after_secs.saturating_mul(1000));
        let remaining_ms = retry_until_ms.saturating_sub(now_ms());
        Ok((remaining_ms > 0).then_some(remaining_ms))
    }

    pub async fn cache_status(&self, index_documents: u64) -> Result<CacheStatus> {
        Ok(CacheStatus {
            database_path: self.db_path.display().to_string(),
            index_path: self.index_path.display().to_string(),
            media_items: count_rows(&self.reader, "SELECT COUNT(*) FROM media_items").await?,
            devices: count_rows(&self.reader, "SELECT COUNT(*) FROM devices").await?,
            playback_snapshots: count_rows(&self.reader, "SELECT COUNT(*) FROM playback_snapshots")
                .await?,
            playlists: count_rows(&self.reader, "SELECT COUNT(*) FROM playlists").await?,
            playlist_items: count_rows(&self.reader, "SELECT COUNT(*) FROM playlist_items").await?,
            recent_items: count_rows(&self.reader, "SELECT COUNT(*) FROM recent_items").await?,
            library_items: count_rows(&self.reader, "SELECT COUNT(*) FROM library_items").await?,
            search_runs: count_rows(&self.reader, "SELECT COUNT(*) FROM search_runs").await?,
            search_results: count_rows(&self.reader, "SELECT COUNT(*) FROM search_results").await?,
            sync_events: count_rows(&self.reader, "SELECT COUNT(*) FROM sync_events").await?,
            index_documents,
            last_sync_at_ms: max_i64(&self.reader, "SELECT MAX(finished_at_ms) FROM sync_events")
                .await?,
            last_search_at_ms: max_i64(&self.reader, "SELECT MAX(fetched_at_ms) FROM search_runs")
                .await?,
        })
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

        if !self.is_migration_applied(1).await? {
            sqlx::raw_sql(INITIAL_SCHEMA).execute(&self.writer).await?;
            sqlx::query(
                "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (1, 'initial_cache', ?)",
            )
            .bind(now_ms())
            .execute(&self.writer)
            .await?;
        }

        if !self.is_migration_applied(2).await? {
            sqlx::raw_sql(MIGRATION_002_SNAPSHOT_ID_FRESHNESS)
                .execute(&self.writer)
                .await?;
            sqlx::query(
                "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (2, 'snapshot_id_freshness', ?)",
            )
            .bind(now_ms())
            .execute(&self.writer)
            .await?;
        }

        if !self.is_migration_applied(3).await? {
            sqlx::raw_sql(MIGRATION_003_RECEIPTS)
                .execute(&self.writer)
                .await?;
            sqlx::query(
                "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (3, 'receipts', ?)",
            )
            .bind(now_ms())
            .execute(&self.writer)
            .await?;
        }

        self.validate_schema().await?;
        Ok(())
    }

    /// Force-run migrations again. Used by tests to assert idempotency.
    #[doc(hidden)]
    pub async fn run_migrations_idempotent_for_test(&self) -> Result<()> {
        self.run_migrations().await
    }

    /// Read-side connection pool. Used by tests + downstream introspection.
    pub fn reader(&self) -> &SqlitePool {
        &self.reader
    }

    /// Write-side connection pool — gated behind `for_test` so production
    /// code never bypasses the store API. Tests use it to inject scenarios
    /// (corrupt rows, future migration entries, etc.).
    #[doc(hidden)]
    pub fn writer_for_test(&self) -> &SqlitePool {
        &self.writer
    }

    /// Greatest applied migration version in this database. Used by
    /// `check_cache_version`; also surfaced to `spotuify doctor`.
    pub async fn applied_cache_version(&self) -> Result<i64> {
        let row: Option<(Option<i64>,)> =
            sqlx::query_as("SELECT MAX(version) FROM schema_migrations")
                .fetch_optional(&self.reader)
                .await?;
        Ok(row.and_then(|(v,)| v).unwrap_or(0))
    }

    // --- Phase 6.6 receipts lifecycle ---

    /// Persist a pending receipt at mutation-issue time. Called before the
    /// daemon makes the Spotify Web API call so the receipt survives a
    /// crash mid-mutation. The original request JSON is captured for
    /// Phase 12 ops_log and for human-readable rollback diffs.
    pub async fn insert_pending_receipt(
        &self,
        receipt: &spotuify_protocol::Receipt,
        request_json: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO receipts \
             (receipt_id, action, status, request_json, started_at_ms, finished_at_ms, error_json) \
             VALUES (?, ?, ?, ?, ?, NULL, NULL)",
        )
        .bind(receipt.receipt_id.0.to_string())
        .bind(&receipt.action)
        .bind("pending")
        .bind(request_json)
        .bind(receipt.started_at_ms)
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    /// Transition a pending receipt to confirmed or failed. First-write
    /// wins: subsequent finalizes on the same receipt are silent no-ops
    /// so daemon restarts can't double-fire MutationFinalized events.
    pub async fn finalize_receipt(
        &self,
        receipt_id: spotuify_protocol::ReceiptId,
        status: spotuify_protocol::ReceiptStatus,
        message: &str,
        finished_at_ms: i64,
        error: Option<&spotuify_protocol::ApiErrorSummary>,
    ) -> Result<()> {
        let status_str = match status {
            spotuify_protocol::ReceiptStatus::Pending => "pending",
            spotuify_protocol::ReceiptStatus::Confirmed => "confirmed",
            spotuify_protocol::ReceiptStatus::Failed => "failed",
        };
        let error_json = match error {
            Some(e) => Some(
                serde_json::to_string(e)
                    .map_err(|err| anyhow::anyhow!("error serialize: {err}"))?,
            ),
            None => None,
        };
        sqlx::query(
            "UPDATE receipts SET status = ?, message = ?, finished_at_ms = ?, error_json = ? \
             WHERE receipt_id = ? AND status = 'pending'",
        )
        .bind(status_str)
        .bind(message)
        .bind(finished_at_ms)
        .bind(error_json.as_deref())
        .bind(receipt_id.0.to_string())
        .execute(&self.writer)
        .await?;
        // Always Ok: zero rows updated means already-finalised, which is
        // the idempotent path.
        Ok(())
    }

    /// Fetch a receipt by id. Errors when missing rather than returning
    /// a default so the daemon can't accidentally treat "not found" as
    /// "already confirmed".
    pub async fn get_receipt(
        &self,
        receipt_id: spotuify_protocol::ReceiptId,
    ) -> Result<spotuify_protocol::Receipt> {
        let row = sqlx::query(
            "SELECT receipt_id, action, status, message, started_at_ms, finished_at_ms, error_json \
             FROM receipts WHERE receipt_id = ?",
        )
        .bind(receipt_id.0.to_string())
        .fetch_optional(&self.reader)
        .await?
        .ok_or_else(|| anyhow::anyhow!("receipt {receipt_id} not found"))?;
        row_to_receipt(&row)
    }

    /// Receipts left in the pending state. Called on daemon startup to
    /// reconcile mutations that were in flight when the previous run
    /// died. The daemon decides per-receipt whether to retry, give up
    /// after a TTL, or surface to the user.
    pub async fn list_pending_receipts(&self) -> Result<Vec<spotuify_protocol::Receipt>> {
        let rows = sqlx::query(
            "SELECT receipt_id, action, status, message, started_at_ms, finished_at_ms, error_json \
             FROM receipts WHERE status = 'pending' ORDER BY started_at_ms ASC",
        )
        .fetch_all(&self.reader)
        .await?;
        rows.iter().map(row_to_receipt).collect()
    }

    /// The original request JSON captured at insert_pending_receipt time.
    /// Used by Phase 12 ops_log + ops_show.
    pub async fn receipt_request_json(
        &self,
        receipt_id: spotuify_protocol::ReceiptId,
    ) -> Result<String> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT request_json FROM receipts WHERE receipt_id = ?")
                .bind(receipt_id.0.to_string())
                .fetch_optional(&self.reader)
                .await?;
        row.map(|(s,)| s)
            .ok_or_else(|| anyhow::anyhow!("receipt {receipt_id} not found"))
    }

    /// Refuse to start when the database has been touched by a future
    /// binary. Returning `Ok(())` means we can proceed safely.
    ///
    /// Two policies:
    /// - applied == CACHE_VERSION → ok (current).
    /// - applied < CACHE_VERSION → migrations would have run already, so
    ///   reaching this method means run_migrations didn't complete; that's
    ///   an internal bug → ok here (caller bumps the version).
    /// - applied > CACHE_VERSION → fatal. User must downgrade the db or
    ///   run `spotuify cache reset --confirm`.
    pub async fn check_cache_version(&self) -> anyhow::Result<()> {
        let applied = self.applied_cache_version().await?;
        if applied > CACHE_VERSION as i64 {
            anyhow::bail!(
                "spotuify cache schema is at version {applied} but this binary only \
                 understands up to v{CACHE_VERSION}. Downgrade the binary or run \
                 `spotuify cache reset --confirm` to start fresh."
            );
        }
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

    async fn validate_schema(&self) -> Result<()> {
        for (table, columns) in REQUIRED_COLUMNS {
            for column in *columns {
                if !self.column_exists(table, column).await? {
                    anyhow::bail!("store schema is missing required column {table}.{column}");
                }
            }
        }
        Ok(())
    }

    async fn column_exists(&self, table: &str, column: &str) -> Result<bool> {
        let query = format!("PRAGMA table_info({table})");
        let rows = sqlx::query(&query).fetch_all(&self.writer).await?;
        Ok(rows
            .iter()
            .any(|row| row.get::<String, _>("name") == column))
    }
}

pub fn cache_db_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("SPOTUIFY_CACHE_DB") {
        return Ok(PathBuf::from(path));
    }
    dirs::data_local_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/share")))
        .map(|dir| dir.join("spotuify/cache.sqlite3"))
        .ok_or_else(|| anyhow::anyhow!("could not resolve local data directory"))
}

pub fn search_index_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("SPOTUIFY_SEARCH_INDEX") {
        return Ok(PathBuf::from(path));
    }
    dirs::data_local_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/share")))
        .map(|dir| dir.join("spotuify/search_index"))
        .ok_or_else(|| anyhow::anyhow!("could not resolve local data directory"))
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn normalize_query(query: &str) -> String {
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn row_to_media_item(row: sqlx::sqlite::SqliteRow) -> Result<MediaItem> {
    Ok(MediaItem {
        id: row.get("spotify_id"),
        uri: row.get("uri"),
        name: row.get("name"),
        subtitle: row.get("subtitle"),
        context: row.get("context"),
        duration_ms: row.get::<i64, _>("duration_ms").max(0) as u64,
        image_url: row.get("image_url"),
        kind: media_kind_from_label(&row.get::<String, _>("kind"))?,
        source: Some(row.get("source")),
        freshness: Some("cached".to_string()),
        explicit: None,
        is_playable: None,
    })
}

fn media_kind_from_label(label: &str) -> Result<MediaKind> {
    match label {
        "track" => Ok(MediaKind::Track),
        "episode" => Ok(MediaKind::Episode),
        "album" => Ok(MediaKind::Album),
        "artist" => Ok(MediaKind::Artist),
        "playlist" => Ok(MediaKind::Playlist),
        _ => anyhow::bail!("unknown media kind `{label}`"),
    }
}

fn playlist_media_item(playlist: &Playlist) -> MediaItem {
    MediaItem {
        id: Some(playlist.id.clone()),
        uri: playlist_uri(&playlist.id),
        name: playlist.name.clone(),
        subtitle: playlist.owner.clone(),
        context: format!("{} tracks", playlist.tracks_total),
        duration_ms: 0,
        image_url: playlist.image_url.clone(),
        kind: MediaKind::Playlist,
        source: Some("spotify".to_string()),
        freshness: None,
        explicit: None,
        is_playable: None,
    }
}

fn playlist_uri(playlist_id: &str) -> String {
    if playlist_id.starts_with("spotify:playlist:") {
        playlist_id.to_string()
    } else {
        format!("spotify:playlist:{playlist_id}")
    }
}

fn retry_after_seconds(message: &str) -> Option<i64> {
    let message = message.to_ascii_lowercase();
    if !(message.contains("rate limit") || message.contains("rate limited")) {
        return None;
    }
    let (_, after) = message.split_once("retry after ")?;
    let digits = after
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    digits.parse::<i64>().ok()
}

async fn count_rows(pool: &SqlitePool, sql: &str) -> Result<u32> {
    Ok(sqlx::query_scalar::<_, i64>(sql)
        .fetch_one(pool)
        .await?
        .max(0) as u32)
}

async fn max_i64(pool: &SqlitePool, sql: &str) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar::<_, Option<i64>>(sql)
        .fetch_one(pool)
        .await?)
}

const INITIAL_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS media_items (
    uri           TEXT PRIMARY KEY,
    spotify_id    TEXT,
    kind          TEXT NOT NULL,
    name          TEXT NOT NULL,
    subtitle      TEXT NOT NULL DEFAULT '',
    context       TEXT NOT NULL DEFAULT '',
    duration_ms   INTEGER NOT NULL DEFAULT 0,
    image_url     TEXT,
    source        TEXT NOT NULL DEFAULT 'spotify',
    liked         INTEGER NOT NULL DEFAULT 0,
    saved         INTEGER NOT NULL DEFAULT 0,
    fetched_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_media_items_kind ON media_items(kind);
CREATE INDEX IF NOT EXISTS idx_media_items_updated ON media_items(updated_at_ms DESC);

CREATE TABLE IF NOT EXISTS devices (
    device_key      TEXT PRIMARY KEY,
    id              TEXT,
    name            TEXT NOT NULL,
    kind            TEXT NOT NULL,
    is_active       INTEGER NOT NULL,
    is_restricted   INTEGER NOT NULL,
    supports_volume INTEGER NOT NULL,
    volume_percent  INTEGER,
    fetched_at_ms   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS playback_snapshots (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    item_uri      TEXT,
    device_key    TEXT,
    is_playing    INTEGER NOT NULL,
    progress_ms   INTEGER NOT NULL,
    shuffle       INTEGER NOT NULL,
    repeat_state  TEXT NOT NULL,
    fetched_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_playback_snapshots_time ON playback_snapshots(fetched_at_ms DESC);

CREATE TABLE IF NOT EXISTS playlists (
    id            TEXT PRIMARY KEY,
    uri           TEXT NOT NULL,
    name          TEXT NOT NULL,
    owner         TEXT NOT NULL,
    tracks_total  INTEGER NOT NULL,
    image_url     TEXT,
    fetched_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS playlist_items (
    playlist_id TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    item_uri    TEXT NOT NULL REFERENCES media_items(uri) ON DELETE CASCADE,
    position    INTEGER NOT NULL,
    added_at_ms INTEGER NOT NULL,
    PRIMARY KEY (playlist_id, item_uri)
);
CREATE INDEX IF NOT EXISTS idx_playlist_items_item ON playlist_items(item_uri);

CREATE TABLE IF NOT EXISTS recent_items (
    item_uri      TEXT NOT NULL REFERENCES media_items(uri) ON DELETE CASCADE,
    played_at_ms  INTEGER NOT NULL,
    fetched_at_ms INTEGER NOT NULL,
    position      INTEGER NOT NULL,
    PRIMARY KEY (item_uri, played_at_ms)
);
CREATE INDEX IF NOT EXISTS idx_recent_items_played ON recent_items(played_at_ms DESC);

CREATE TABLE IF NOT EXISTS library_items (
    item_uri      TEXT PRIMARY KEY REFERENCES media_items(uri) ON DELETE CASCADE,
    kind          TEXT NOT NULL,
    saved         INTEGER NOT NULL DEFAULT 0,
    followed      INTEGER NOT NULL DEFAULT 0,
    fetched_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS search_runs (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    query            TEXT NOT NULL,
    normalized_query TEXT NOT NULL,
    scope            TEXT NOT NULL,
    source           TEXT NOT NULL,
    fetched_at_ms    INTEGER NOT NULL,
    status           TEXT NOT NULL,
    result_count     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_search_runs_query ON search_runs(normalized_query, scope, source, fetched_at_ms DESC);

CREATE TABLE IF NOT EXISTS search_results (
    search_run_id INTEGER NOT NULL REFERENCES search_runs(id) ON DELETE CASCADE,
    position      INTEGER NOT NULL,
    item_uri      TEXT NOT NULL REFERENCES media_items(uri) ON DELETE CASCADE,
    PRIMARY KEY (search_run_id, position)
);

CREATE TABLE IF NOT EXISTS sync_events (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    domain         TEXT NOT NULL,
    started_at_ms  INTEGER NOT NULL,
    finished_at_ms INTEGER NOT NULL,
    status         TEXT NOT NULL,
    row_count      INTEGER NOT NULL,
    error          TEXT
);
CREATE INDEX IF NOT EXISTS idx_sync_events_domain_time ON sync_events(domain, finished_at_ms DESC);

CREATE TABLE IF NOT EXISTS sync_cursors (
    domain             TEXT PRIMARY KEY,
    last_success_at_ms INTEGER,
    last_error         TEXT
);
"#;

/// Phase 6.4 schema migration: snapshot_id, snapshot_id_at_fetch,
/// freshness_class, sync_generation.
///
/// `freshness_class` accepts values in {fresh, stale_but_usable,
/// refreshing, failed_refresh, unknown}. Application-enforced; no CHECK
/// constraint because SQLite would prevent migrating older rows.
///
/// `sync_generation` is bumped on each full sync so we can detect
/// cache-version skew across daemon restarts.
const MIGRATION_002_SNAPSHOT_ID_FRESHNESS: &str = r#"
ALTER TABLE playlists      ADD COLUMN snapshot_id          TEXT;
ALTER TABLE playlist_items ADD COLUMN snapshot_id_at_fetch TEXT;

ALTER TABLE media_items        ADD COLUMN freshness_class TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE media_items        ADD COLUMN sync_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE devices            ADD COLUMN freshness_class TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE devices            ADD COLUMN sync_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE playback_snapshots ADD COLUMN freshness_class TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE playback_snapshots ADD COLUMN sync_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE recent_items       ADD COLUMN freshness_class TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE recent_items       ADD COLUMN sync_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE library_items      ADD COLUMN freshness_class TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE library_items      ADD COLUMN sync_generation INTEGER NOT NULL DEFAULT 0;
"#;

/// Phase 6.6: receipts table for the two-stage mutation lifecycle.
///
/// `receipt_id` is the textual form of a UUID v7 (lexicographic ordering
/// matches chronological order). `request_json` keeps the originating
/// Request so Phase 12 ops_log can render diffs and the daemon can
/// retry on startup if a finalize race lost the response.
///
/// `error_json` holds a serialised `ApiErrorSummary` when status='failed'.
const MIGRATION_003_RECEIPTS: &str = r#"
CREATE TABLE IF NOT EXISTS receipts (
    receipt_id     TEXT PRIMARY KEY,
    action         TEXT NOT NULL,
    status         TEXT NOT NULL,
    request_json   TEXT NOT NULL,
    message        TEXT NOT NULL DEFAULT '',
    started_at_ms  INTEGER NOT NULL,
    finished_at_ms INTEGER,
    error_json     TEXT
);
CREATE INDEX IF NOT EXISTS idx_receipts_status_started ON receipts(status, started_at_ms);
"#;

/// Translate a `receipts` row into the protocol's [`Receipt`] type.
fn row_to_receipt(row: &sqlx::sqlite::SqliteRow) -> Result<spotuify_protocol::Receipt> {
    use sqlx::Row;
    let id_str: String = row.try_get("receipt_id")?;
    let id = uuid::Uuid::parse_str(&id_str)
        .map_err(|err| anyhow::anyhow!("malformed receipt id `{id_str}`: {err}"))?;
    let status_str: String = row.try_get("status")?;
    let status = match status_str.as_str() {
        "pending" => spotuify_protocol::ReceiptStatus::Pending,
        "confirmed" => spotuify_protocol::ReceiptStatus::Confirmed,
        "failed" => spotuify_protocol::ReceiptStatus::Failed,
        other => anyhow::bail!("unknown receipt status `{other}`"),
    };
    let error_json: Option<String> = row.try_get("error_json")?;
    let error = match error_json {
        Some(raw) if !raw.is_empty() => Some(
            serde_json::from_str::<spotuify_protocol::ApiErrorSummary>(&raw)
                .map_err(|err| anyhow::anyhow!("malformed error_json: {err}"))?,
        ),
        _ => None,
    };
    Ok(spotuify_protocol::Receipt {
        receipt_id: spotuify_protocol::ReceiptId(id),
        action: row.try_get("action")?,
        status,
        message: row.try_get("message").unwrap_or_default(),
        started_at_ms: row.try_get("started_at_ms")?,
        finished_at_ms: row.try_get("finished_at_ms")?,
        error,
    })
}

const REQUIRED_COLUMNS: &[(&str, &[&str])] = &[
    (
        "media_items",
        &["uri", "kind", "name", "source", "fetched_at_ms"],
    ),
    ("devices", &["device_key", "name", "fetched_at_ms"]),
    (
        "playback_snapshots",
        &["item_uri", "is_playing", "fetched_at_ms"],
    ),
    ("playlists", &["id", "name", "owner", "tracks_total"]),
    ("playlist_items", &["playlist_id", "item_uri", "position"]),
    (
        "recent_items",
        &["item_uri", "played_at_ms", "fetched_at_ms"],
    ),
    ("library_items", &["item_uri", "kind", "saved", "followed"]),
    (
        "search_runs",
        &["query", "normalized_query", "scope", "source"],
    ),
    ("search_results", &["search_run_id", "position", "item_uri"]),
    (
        "sync_events",
        &["domain", "finished_at_ms", "status", "row_count"],
    ),
    (
        "sync_cursors",
        &["domain", "last_success_at_ms", "last_error"],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cached_remote_search_results_are_queryable_locally_without_network() {
        let store = Store::in_memory().await.unwrap();
        let items = vec![track(
            "spotify:track:1",
            "Never Too Much",
            "Luther Vandross",
        )];

        store
            .cache_search_results(
                "luther vandross",
                SearchScopeData::Track,
                SearchSourceData::Spotify,
                &items,
            )
            .await
            .unwrap();

        let results = store
            .local_search("luther", SearchScopeData::Track, 10)
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].uri, "spotify:track:1");
        assert_eq!(results[0].source.as_deref(), Some("spotify"));
        assert_eq!(results[0].freshness.as_deref(), Some("cached"));
    }

    #[tokio::test]
    async fn cache_status_reports_rows_and_search_freshness() {
        let store = Store::in_memory().await.unwrap();
        let items = vec![track("spotify:track:1", "Sweet Thing", "Chaka Khan")];
        store
            .cache_search_results(
                "chaka khan",
                SearchScopeData::Track,
                SearchSourceData::Spotify,
                &items,
            )
            .await
            .unwrap();

        let status = store.cache_status(1).await.unwrap();

        assert_eq!(status.media_items, 1);
        assert_eq!(status.search_runs, 1);
        assert_eq!(status.search_results, 1);
        assert_eq!(status.index_documents, 1);
        assert!(status.last_search_at_ms.is_some());
    }

    #[tokio::test]
    async fn rate_limit_cooldown_uses_latest_retry_after_error() {
        let store = Store::in_memory().await.unwrap();
        let started_at_ms = now_ms();

        store
            .record_sync_event(
                "recent",
                started_at_ms,
                "error",
                0,
                Some("Spotify GET /me/player/recently-played was rate limited; retry after 60s"),
            )
            .await
            .unwrap();

        assert!(store
            .rate_limit_cooldown_remaining_ms("recent")
            .await
            .unwrap()
            .is_some());
    }

    fn track(uri: &str, name: &str, artist: &str) -> MediaItem {
        MediaItem {
            id: uri.rsplit(':').next().map(str::to_string),
            uri: uri.to_string(),
            name: name.to_string(),
            subtitle: artist.to_string(),
            context: "Test album".to_string(),
            duration_ms: 180_000,
            image_url: None,
            kind: MediaKind::Track,
            source: Some("spotify".to_string()),
            freshness: None,
            explicit: None,
            is_playable: None,
        }
    }
}
