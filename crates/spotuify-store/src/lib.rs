mod listen_facts;
mod operations;

use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};

use spotuify_core::{
    ArtistRef, Device, LyricLine, LyricsProvider, MediaItem, MediaKind, Notification,
    NotificationState, Playback, Playlist, Queue, Recurrence, Reminder, ReminderState,
    SyncedLyrics,
};
use spotuify_protocol::{
    CacheFreshnessStatus, CacheStatus, FreshnessCounts, ListenSession, SearchScopeData,
    SearchSourceData,
};

const BUSY_TIMEOUT: Duration = Duration::from_secs(30);
const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum rows committed per bulk-write transaction. Smaller windows
/// release the SQLite write lock more often so a hot-path command
/// (pause/skip) can sneak in between a sync flush's chunks instead of
/// waiting for a 500-row playlist refresh to finish.
const BULK_CHUNK_ROWS: usize = 64;

/// A gap larger than this between consecutive plays starts a new listening
/// session (20 minutes — the de-facto standard in scrobbler/music-IR work).
const SESSION_GAP_MS: i64 = 20 * 60 * 1000;

/// Two plays of the same track within this window are treated as one event when
/// merging the local `listen_facts` stream with Spotify recently-played.
const DEDUP_TOLERANCE_MS: i64 = 60 * 1000;

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
/// - v4: analytics derivations — listen_facts, track/artist/album/habit
///   metrics, qualification_rules, playback_progress (Phase 10)
/// - v5: operations log — jj-style mutation log with reversal plans
///   and pre-state capture for undo/redo (Phase 12)
/// - v6: lyrics cache and per-track timing offsets (Phase 16)
/// - v7: freshness columns for playlist cache tables (Phase 6.4)
/// - v8: saved-library sync position for unchanged shortcuts (Phase 6.5)
/// - v9: playlist item primary key preserves duplicate tracks
/// - v10: queue cache snapshots and ordered upcoming items
/// - v11: playlist track accessibility flag for Spotify 403 playlists
/// - v12: lyrics lookup negative cache
/// - v13: media enrichment for cached media/library rows
/// - v14: listening reminders
/// - v15: typed retry-after seconds on sync_events
/// - v16: artist/album reference columns on cached media rows
/// - v17: listen_facts context_uri for playlist-level analytics
/// - v18: flip legacy queue_add operations to reversible = 0 (no
///   queue-remove exists, so their undo was a silent no-op)
pub const CACHE_VERSION: u32 = 18;

const FRESHNESS_FRESH: &str = "fresh";

#[derive(Clone)]
pub struct Store {
    /// Hot-path writer pool. Interactive commands (PlaybackCommand,
    /// receipt insert/finalize, operation log writes, listen_facts
    /// finalize on track boundary) acquire connections here. A
    /// separate acquire queue means hot writes never wait behind a
    /// sync flush's 30s pool-acquire timeout.
    writer: SqlitePool,
    /// Background-write pool. Sync (playlists/library refresh),
    /// retention pruning, and any other bulk persist call routes here
    /// via [`Store::bulk_writer`] and the chunked `_bulk` variants.
    /// SQLite still serialises writes at the WAL header lock, but the
    /// pool split + chunked transactions mean the lock window for any
    /// single statement is short.
    bulk_writer: SqlitePool,
    reader: SqlitePool,
    db_path: PathBuf,
    index_path: PathBuf,
}

struct SyncEventRecord<'a> {
    domain: &'a str,
    started_at_ms: i64,
    status: &'a str,
    row_count: u32,
    error: Option<&'a str>,
    retry_after_secs: Option<u64>,
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
        let writer = build_writer_pool(&db_url).await?;
        let bulk_writer = build_writer_pool(&db_url).await?;

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
            bulk_writer,
            reader,
            db_path: db_path.to_path_buf(),
            index_path: index_path.to_path_buf(),
        };
        store.ensure_schema_migrations_table().await?;
        store.check_cache_version().await?;
        store.run_migrations().await?;
        secure_sqlite_files(db_path)?;
        spotuify_protocol::paths::ensure_private_dir(index_path)?;
        Ok(store)
    }

    /// In-memory store for tests across the workspace. Migrations run.
    ///
    /// SQLite `:memory:` databases are per-connection, so the hot
    /// writer, the bulk writer, and the reader all share the same
    /// pool here. Production callers use `open()` which builds three
    /// separate connection pools against the same on-disk database.
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
            bulk_writer: pool.clone(),
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
        self.upsert_media_items_with(items, source, &self.writer)
            .await
    }

    /// Bulk-pool variant of [`Self::upsert_media_items`]. Identical
    /// semantics; routes through the background writer + chunks into
    /// `BULK_CHUNK_ROWS` so a single 500-track playlist refresh
    /// doesn't hold the SQLite write lock across the whole batch.
    pub async fn upsert_media_items_bulk(&self, items: &[MediaItem], source: &str) -> Result<u32> {
        self.upsert_media_items_with(items, source, &self.bulk_writer)
            .await
    }

    async fn upsert_media_items_with(
        &self,
        items: &[MediaItem],
        source: &str,
        pool: &SqlitePool,
    ) -> Result<u32> {
        if items.is_empty() {
            return Ok(0);
        }
        let fetched_at_ms = now_ms();
        let mut written = 0;
        for chunk in items.chunks(BULK_CHUNK_ROWS) {
            let mut tx = pool.begin().await?;
            for item in chunk {
                let item_source = item.source.as_deref().unwrap_or(source);
                // Serialize navigable artist refs (name+uri) for click-through;
                // `NULL` when none so older rows / non-track items stay clean.
                let artists_json = if item.artists.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&item.artists)?)
                };
                sqlx::query(
                    "INSERT INTO media_items (
                        uri, spotify_id, kind, name, subtitle, context, duration_ms,
                        image_url, source, fetched_at_ms, updated_at_ms,
                        freshness_class, sync_generation, album_uri, artists_json
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
                        updated_at_ms = excluded.updated_at_ms,
                        freshness_class = excluded.freshness_class,
                        sync_generation = excluded.sync_generation,
                        album_uri = COALESCE(excluded.album_uri, media_items.album_uri),
                        artists_json = COALESCE(excluded.artists_json, media_items.artists_json)",
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
                .bind(FRESHNESS_FRESH)
                .bind(fetched_at_ms)
                .bind(&item.album_uri)
                .bind(artists_json)
                .execute(&mut *tx)
                .await?;
                written += 1;
            }
            tx.commit().await?;
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
        let mut tx = self.writer.begin().await?;
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
        .execute(&mut *tx)
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
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
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
                    image_url, source, liked, saved, updated_at_ms, album_uri, artists_json
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
                    image_url, source, liked, saved, updated_at_ms, album_uri, artists_json
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
                    duration_ms, image_url, source, liked, media_items.saved, updated_at_ms,
                    media_items.album_uri, media_items.artists_json
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

    /// Liked songs (cache fallback for `Request::SavedTracks`). Saved tracks
    /// only, newest-saved first when `added_at_ms` is known.
    pub async fn list_saved_tracks(&self, limit: u32) -> Result<Vec<MediaItem>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT media_items.uri, spotify_id, media_items.kind, name, subtitle, context,
                    duration_ms, image_url, source, media_items.album,
                    media_items.release_date, library_items.added_at_ms,
                    media_items.album_uri, media_items.artists_json
             FROM library_items
             JOIN media_items ON media_items.uri = library_items.item_uri
             WHERE library_items.saved = 1 AND media_items.kind = 'track'
             ORDER BY library_items.added_at_ms DESC, library_items.fetched_at_ms DESC, name ASC
             LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.reader)
        .await?;
        rows.into_iter().map(row_to_media_item).collect()
    }

    /// Subscribed podcasts (cache-backed `Request::SavedShows`). Saved shows only.
    pub async fn list_saved_shows(&self, limit: u32) -> Result<Vec<MediaItem>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT media_items.uri, spotify_id, media_items.kind, name, subtitle, context,
                    duration_ms, image_url, source
             FROM library_items
             JOIN media_items ON media_items.uri = library_items.item_uri
             WHERE (library_items.saved = 1 OR library_items.followed = 1)
                   AND media_items.kind = 'show'
             ORDER BY name ASC
             LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.reader)
        .await?;
        rows.into_iter().map(row_to_media_item).collect()
    }

    /// Followed artists (cache-backed `Request::FollowedArtists`). Artists are
    /// `followed=1` in `library_items`; ordered alphabetically.
    pub async fn list_followed_artists(&self, limit: u32) -> Result<Vec<MediaItem>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT media_items.uri, spotify_id, media_items.kind, name, subtitle, context,
                    duration_ms, image_url, source
             FROM library_items
             JOIN media_items ON media_items.uri = library_items.item_uri
             WHERE library_items.followed = 1 AND media_items.kind = 'artist'
             ORDER BY name COLLATE NOCASE ASC
             LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.reader)
        .await?;
        rows.into_iter().map(row_to_media_item).collect()
    }

    /// URIs of saved albums (`library_items.saved=1`, `kind='album'`). The
    /// daemon intersects an artist's discography against this set to tag each
    /// album's `in_library` flag without a per-album Spotify call.
    pub async fn saved_album_uris(&self) -> Result<std::collections::HashSet<String>> {
        let rows =
            sqlx::query("SELECT item_uri FROM library_items WHERE saved = 1 AND kind = 'album'")
                .fetch_all(&self.reader)
                .await?;
        rows.into_iter()
            .map(|row| row.try_get::<String, _>("item_uri").map_err(Into::into))
            .collect()
    }

    /// Return the most recent `playback_snapshots` row if any, else
    /// synthesise a [`Playback`] from the most recently played item
    /// in `recent_items` (with `is_playing = false`, `device = None`).
    /// Lets the daemon hand a cold-cache client *something* to render
    /// — the last song the user heard — instead of an empty screen
    /// while the first Spotify poll is in flight. Returns `None` only
    /// when both tables are empty (first-ever launch, no listen
    /// history).
    pub async fn latest_playback_or_recent(&self) -> Result<Option<Playback>> {
        if let Some(playback) = self.latest_playback().await? {
            return Ok(Some(playback));
        }
        let Some(row) = sqlx::query(
            "SELECT media_items.uri, spotify_id, media_items.kind, name, subtitle, context,
                    duration_ms, image_url, source, liked, media_items.saved, updated_at_ms
             FROM recent_items
             JOIN media_items ON media_items.uri = recent_items.item_uri
             ORDER BY recent_items.played_at_ms DESC, recent_items.position ASC
             LIMIT 1",
        )
        .fetch_optional(&self.reader)
        .await?
        else {
            return Ok(None);
        };
        let item = row_to_media_item(row)?;
        Ok(Some(Playback {
            item: Some(item),
            device: None,
            is_playing: false,
            progress_ms: 0,
            shuffle: false,
            repeat: "off".to_string(),
            source: Some(spotuify_core::PlaybackStateSource::RecentFallback),
            ..Default::default()
        }))
    }

    pub async fn latest_playback(&self) -> Result<Option<Playback>> {
        let Some(row) = sqlx::query(
            "SELECT item_uri, device_key, is_playing, progress_ms, shuffle, repeat_state
             FROM playback_snapshots
             WHERE item_uri IS NOT NULL
                OR device_key IS NOT NULL
                OR is_playing = 1
             ORDER BY fetched_at_ms DESC
             LIMIT 1",
        )
        .fetch_optional(&self.reader)
        .await?
        else {
            return Ok(None);
        };

        let item_uri = row.get::<Option<String>, _>("item_uri");
        let device_key = row.get::<Option<String>, _>("device_key");
        let item = match item_uri {
            Some(uri) => self.media_items_by_uris(&[uri]).await?.into_iter().next(),
            None => None,
        };
        let device = match device_key {
            Some(key) => self.device_by_key(&key).await?,
            None => None,
        };
        Ok(Some(Playback {
            item,
            device,
            is_playing: row.get("is_playing"),
            progress_ms: row.get::<i64, _>("progress_ms").max(0) as u64,
            shuffle: row.get("shuffle"),
            repeat: row.get("repeat_state"),
            source: Some(spotuify_core::PlaybackStateSource::Cache),
            ..Default::default()
        }))
    }

    pub async fn latest_queue(&self, limit: u32) -> Result<Option<Queue>> {
        // Prefer the latest snapshot that has any content (a
        // currently_playing URI or at least one queue_item). Pre-fix
        // daemons (≤ 2026-05-18) persisted an empty snapshot every 3s
        // during idle periods; without this filter we'd hand the
        // newest one back and clients would see "queue is empty" even
        // though a meaningful snapshot exists a few rows below. The
        // fallback to the absolute latest row covers fresh installs
        // and brand-new sessions where nothing is queued yet.
        let row = sqlx::query(
            "SELECT id, currently_playing_uri, fetched_at_ms
             FROM queue_snapshots
             WHERE currently_playing_uri IS NOT NULL
                OR EXISTS (SELECT 1 FROM queue_items WHERE snapshot_id = queue_snapshots.id)
             ORDER BY fetched_at_ms DESC
             LIMIT 1",
        )
        .fetch_optional(&self.reader)
        .await?;
        let Some(row) = (match row {
            Some(row) => Some(row),
            None => {
                sqlx::query(
                    "SELECT id, currently_playing_uri, fetched_at_ms
                 FROM queue_snapshots
                 ORDER BY fetched_at_ms DESC
                 LIMIT 1",
                )
                .fetch_optional(&self.reader)
                .await?
            }
        }) else {
            return Ok(None);
        };

        let snapshot_id = row.get::<i64, _>("id");
        let currently_playing = match row.get::<Option<String>, _>("currently_playing_uri") {
            Some(uri) => self.media_items_by_uris(&[uri]).await?.into_iter().next(),
            None => None,
        };
        let fetched_at_ms = row.get::<i64, _>("fetched_at_ms");
        if limit == 0 {
            return Ok(Some(Queue {
                currently_playing,
                items: Vec::new(),
                // Cache reads are by definition stale: we don't know
                // whether the originating session is still alive. The
                // sync layer flips this true after a fresh live probe.
                session_active: false,
                as_of_ms: fetched_at_ms,
            }));
        }

        let rows = sqlx::query(
            "SELECT media_items.uri, spotify_id, media_items.kind, name, subtitle, context,
                    duration_ms, image_url, source, liked, media_items.saved, updated_at_ms,
                    media_items.album_uri, media_items.artists_json
             FROM queue_items
             JOIN media_items ON media_items.uri = queue_items.item_uri
             WHERE queue_items.snapshot_id = ?
             ORDER BY queue_items.position ASC
             LIMIT ?",
        )
        .bind(snapshot_id)
        .bind(limit as i64)
        .fetch_all(&self.reader)
        .await?;
        Ok(Some(Queue {
            currently_playing,
            items: rows
                .into_iter()
                .map(row_to_media_item)
                .collect::<Result<Vec<_>>>()?,
            session_active: false,
            as_of_ms: fetched_at_ms,
        }))
    }

    pub async fn list_devices(&self) -> Result<Vec<Device>> {
        let rows = sqlx::query(
            "SELECT id, name, kind, is_active, is_restricted, supports_volume, volume_percent
             FROM devices
             ORDER BY is_active DESC, fetched_at_ms DESC, name ASC",
        )
        .fetch_all(&self.reader)
        .await?;
        rows.into_iter().map(row_to_device).collect()
    }

    async fn device_by_key(&self, device_key: &str) -> Result<Option<Device>> {
        sqlx::query(
            "SELECT id, name, kind, is_active, is_restricted, supports_volume, volume_percent
             FROM devices
             WHERE device_key = ?",
        )
        .bind(device_key)
        .fetch_optional(&self.reader)
        .await?
        .map(row_to_device)
        .transpose()
    }

    pub async fn list_playlists(&self, limit: u32) -> Result<Vec<Playlist>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT id, name, owner, tracks_total, image_url, snapshot_id
             FROM playlists
             WHERE tracks_accessible = 1
             ORDER BY name COLLATE NOCASE ASC
             LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.reader)
        .await?;
        rows.into_iter().map(row_to_playlist).collect()
    }

    pub async fn playlist_items(&self, playlist_id: &str, limit: u32) -> Result<Vec<MediaItem>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT media_items.uri, spotify_id, media_items.kind, name, subtitle, context,
                    duration_ms, image_url, source, liked, media_items.saved, updated_at_ms,
                    media_items.album_uri, media_items.artists_json
             FROM playlist_items
             JOIN media_items ON media_items.uri = playlist_items.item_uri
             WHERE playlist_items.playlist_id = ?
             ORDER BY playlist_items.position ASC
             LIMIT ?",
        )
        .bind(playlist_id)
        .bind(limit as i64)
        .fetch_all(&self.reader)
        .await?;
        rows.into_iter().map(row_to_media_item).collect()
    }

    pub async fn list_recent_items(&self, limit: u32) -> Result<Vec<MediaItem>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT media_items.uri, spotify_id, media_items.kind, name, subtitle, context,
                    duration_ms, image_url, source, liked, media_items.saved, updated_at_ms,
                    media_items.album_uri, media_items.artists_json
             FROM recent_items
             JOIN media_items ON media_items.uri = recent_items.item_uri
             ORDER BY recent_items.played_at_ms DESC, recent_items.position ASC
             LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.reader)
        .await?;
        rows.into_iter().map(row_to_media_item).collect()
    }

    /// Flip an artist's `followed` flag in the cache after a follow/unfollow
    /// mutation, so the Followed-Artists list reflects it without waiting for
    /// the next library sync. UPDATE-only: never creates an orphan
    /// `library_items` row (a brand-new follow of an artist not yet cached is
    /// picked up by the next `followed_artists` sync).
    pub async fn set_artist_followed(&self, uri: &str, followed: bool) -> Result<()> {
        sqlx::query(
            "UPDATE library_items
             SET followed = ?, fetched_at_ms = ?
             WHERE item_uri = ?",
        )
        .bind(i64::from(followed))
        .bind(now_ms())
        .bind(uri)
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    /// Listening history grouped into sessions. Merges the local `listen_facts`
    /// (plays driven through spotuify) with Spotify `recent_items` (plays from
    /// any device), de-dups near-simultaneous duplicates, then splits the merged
    /// stream wherever the gap between consecutive plays exceeds
    /// [`SESSION_GAP_MS`]. Sessions are newest-first; tracks within a session are
    /// newest-first. `limit` caps the number of sessions returned.
    pub async fn list_listen_sessions(&self, limit: u32) -> Result<Vec<ListenSession>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        // Bound the scan: enough plays to fill `limit` sessions comfortably.
        let scan = (limit as i64).saturating_mul(60).clamp(200, 5_000);
        let mut plays: Vec<(String, i64)> = Vec::new();
        let local = sqlx::query(
            "SELECT track_uri, started_at_ms FROM listen_facts
             ORDER BY started_at_ms DESC LIMIT ?",
        )
        .bind(scan)
        .fetch_all(&self.reader)
        .await?;
        for row in local {
            plays.push((row.get("track_uri"), row.get::<i64, _>("started_at_ms")));
        }
        let recent = sqlx::query(
            "SELECT item_uri, played_at_ms FROM recent_items
             ORDER BY played_at_ms DESC LIMIT ?",
        )
        .bind(scan)
        .fetch_all(&self.reader)
        .await?;
        for row in recent {
            plays.push((row.get("item_uri"), row.get::<i64, _>("played_at_ms")));
        }
        if plays.is_empty() {
            return Ok(Vec::new());
        }
        // Newest-first, then drop near-simultaneous duplicates of the same track
        // that arrived from both sources (within DEDUP_TOLERANCE_MS).
        plays.sort_by_key(|play| std::cmp::Reverse(play.1));
        let mut deduped: Vec<(String, i64)> = Vec::with_capacity(plays.len());
        for (uri, at) in plays {
            if deduped
                .iter()
                .any(|(u, t)| u == &uri && (t - at).abs() <= DEDUP_TOLERANCE_MS)
            {
                continue;
            }
            deduped.push((uri, at));
        }
        // Split into sessions on gaps; `deduped` is newest-first, so a gap to the
        // PREVIOUS (newer) play larger than the threshold starts a new session.
        let mut sessions: Vec<Vec<(String, i64)>> = Vec::new();
        for (uri, at) in deduped {
            match sessions.last_mut() {
                Some(current)
                    if current
                        .last()
                        .is_some_and(|(_, prev)| prev - at <= SESSION_GAP_MS) =>
                {
                    current.push((uri, at));
                }
                _ => {
                    if sessions.len() >= limit as usize {
                        break;
                    }
                    sessions.push(vec![(uri, at)]);
                }
            }
        }
        // Resolve media items per session (newest-first) and assemble.
        let mut out = Vec::with_capacity(sessions.len());
        for plays in sessions {
            let uris = plays.iter().map(|(u, _)| u.clone()).collect::<Vec<_>>();
            let items = self.media_items_by_uris(&uris).await?;
            let started_at_ms = plays.iter().map(|(_, t)| *t).min().unwrap_or(0);
            let ended_at_ms = plays.iter().map(|(_, t)| *t).max().unwrap_or(0);
            out.push(ListenSession {
                session_id: format!("session-{started_at_ms}"),
                started_at_ms,
                ended_at_ms,
                track_count: items.len() as u32,
                context_label: dominant_context(&items),
                tracks: items,
            });
        }
        Ok(out)
    }

    pub async fn saved_tracks_fingerprint(&self, limit: u32) -> Result<(u64, Vec<String>)> {
        let total = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)
             FROM library_items
             WHERE kind = 'track' AND saved = 1",
        )
        .fetch_one(&self.reader)
        .await?
        .max(0) as u64;
        let rows = sqlx::query(
            "SELECT COALESCE(media_items.spotify_id, library_items.item_uri) AS id
             FROM library_items
             JOIN media_items ON media_items.uri = library_items.item_uri
             WHERE library_items.kind = 'track' AND library_items.saved = 1
             ORDER BY library_items.sync_position ASC,
                      library_items.fetched_at_ms DESC,
                      library_items.item_uri ASC
             LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.reader)
        .await?;
        Ok((
            total,
            rows.into_iter()
                .map(|row| row.get::<String, _>("id"))
                .collect(),
        ))
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
        self.persist_devices_with(devices, &self.writer).await
    }

    pub async fn persist_devices_bulk(&self, devices: &[Device]) -> Result<u32> {
        self.persist_devices_with(devices, &self.bulk_writer).await
    }

    async fn persist_devices_with(&self, devices: &[Device], pool: &SqlitePool) -> Result<u32> {
        self.persist_devices_inner(devices, pool, false).await
    }

    /// Persist the device list and **delete every cached row not in
    /// this batch** so the local view exactly mirrors Spotify's
    /// `/v1/me/player/devices` at this point in time.
    ///
    /// Use this from the periodic full-refresh path. The single-device
    /// persist that happens during `persist_playback` (when the
    /// playback poll includes an active device) must NOT prune — it
    /// would nuke every other device after every poll. That path uses
    /// the non-pruning `persist_devices`.
    pub async fn replace_devices(&self, devices: &[Device]) -> Result<u32> {
        self.persist_devices_inner(devices, &self.writer, true)
            .await
    }

    async fn persist_devices_inner(
        &self,
        devices: &[Device],
        pool: &SqlitePool,
        prune_stale: bool,
    ) -> Result<u32> {
        // `prune_stale` with an empty batch would wipe the whole table —
        // which IS the right behavior when Spotify returns 0 devices
        // (the user disconnected everything). Handle that case
        // explicitly; the original short-circuit dropped through to
        // returning 0 without persisting OR pruning.
        if devices.is_empty() {
            if prune_stale {
                sqlx::query("DELETE FROM devices").execute(pool).await?;
            }
            return Ok(0);
        }
        let fetched_at_ms = now_ms();
        for chunk in devices.chunks(BULK_CHUNK_ROWS) {
            let mut tx = pool.begin().await?;
            for device in chunk {
                let device_key = device.id.as_deref().unwrap_or(&device.name);
                sqlx::query(
                    "INSERT INTO devices (
                        device_key, id, name, kind, is_active, is_restricted,
                        supports_volume, volume_percent, fetched_at_ms,
                        freshness_class, sync_generation
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                    ON CONFLICT(device_key) DO UPDATE SET
                        id = excluded.id,
                        name = excluded.name,
                        kind = excluded.kind,
                        is_active = excluded.is_active,
                        is_restricted = excluded.is_restricted,
                        supports_volume = excluded.supports_volume,
                        volume_percent = excluded.volume_percent,
                        fetched_at_ms = excluded.fetched_at_ms,
                        freshness_class = excluded.freshness_class,
                        sync_generation = excluded.sync_generation",
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
                .bind(FRESHNESS_FRESH)
                .bind(fetched_at_ms)
                .execute(&mut *tx)
                .await?;
            }
            tx.commit().await?;
        }
        if prune_stale {
            // Drop every row not stamped with this refresh's
            // generation. Mirrors what Spotify just told us about its
            // /v1/me/player/devices state; ghost rows from prior
            // runs (the 7 stale "spotuify" entries) disappear here.
            sqlx::query("DELETE FROM devices WHERE sync_generation < ?")
                .bind(fetched_at_ms)
                .execute(pool)
                .await?;
        }
        Ok(devices.len() as u32)
    }

    pub async fn persist_playback(&self, playback: &Playback) -> Result<u32> {
        self.persist_playback_with(playback, &self.writer).await
    }

    pub async fn persist_playback_bulk(&self, playback: &Playback) -> Result<u32> {
        self.persist_playback_with(playback, &self.bulk_writer)
            .await
    }

    async fn persist_playback_with(&self, playback: &Playback, pool: &SqlitePool) -> Result<u32> {
        if let Some(item) = &playback.item {
            self.upsert_media_items_with(std::slice::from_ref(item), "spotify", pool)
                .await?;
        }
        if let Some(device) = &playback.device {
            self.persist_devices_with(std::slice::from_ref(device), pool)
                .await?;
        }
        let fetched_at_ms = now_ms();
        let device_key = playback
            .device
            .as_ref()
            .map(|device| device.id.as_deref().unwrap_or(&device.name).to_string());
        sqlx::query(
            "INSERT INTO playback_snapshots (
                item_uri, device_key, is_playing, progress_ms, shuffle, repeat_state,
                fetched_at_ms, freshness_class, sync_generation
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(playback.item.as_ref().map(|item| item.uri.as_str()))
        .bind(device_key)
        .bind(playback.is_playing)
        .bind(playback.progress_ms as i64)
        .bind(playback.shuffle)
        .bind(&playback.repeat)
        .bind(fetched_at_ms)
        .bind(FRESHNESS_FRESH)
        .bind(fetched_at_ms)
        .execute(pool)
        .await?;
        Ok(1)
    }

    pub async fn persist_queue(&self, queue: &Queue) -> Result<u32> {
        self.persist_queue_with(queue, &self.writer).await
    }

    pub async fn persist_queue_bulk(&self, queue: &Queue) -> Result<u32> {
        self.persist_queue_with(queue, &self.bulk_writer).await
    }

    async fn persist_queue_with(&self, queue: &Queue, pool: &SqlitePool) -> Result<u32> {
        let mut media_items = Vec::with_capacity(queue.items.len() + 1);
        if let Some(item) = &queue.currently_playing {
            media_items.push(item.clone());
        }
        media_items.extend(queue.items.iter().cloned());
        self.upsert_media_items_with(&media_items, "spotify", pool)
            .await?;

        let fetched_at_ms = now_ms();
        let mut tx = pool.begin().await?;
        let result = sqlx::query(
            "INSERT INTO queue_snapshots (
                currently_playing_uri, fetched_at_ms, freshness_class, sync_generation
             )
             VALUES (?, ?, ?, ?)",
        )
        .bind(
            queue
                .currently_playing
                .as_ref()
                .map(|item| item.uri.as_str()),
        )
        .bind(fetched_at_ms)
        .bind(FRESHNESS_FRESH)
        .bind(fetched_at_ms)
        .execute(&mut *tx)
        .await?;
        let snapshot_id = result.last_insert_rowid();
        // Track items within a queue snapshot are bounded (Spotify
        // surfaces ~20 upcoming items at most) so a single tx here
        // never accumulates a long write window.
        for (position, item) in queue.items.iter().enumerate() {
            sqlx::query(
                "INSERT INTO queue_items (
                    snapshot_id, item_uri, position, freshness_class, sync_generation
                 )
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(snapshot_id)
            .bind(&item.uri)
            .bind(position as i64)
            .bind(FRESHNESS_FRESH)
            .bind(fetched_at_ms)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(1)
    }

    pub async fn persist_playlists(&self, playlists: &[Playlist]) -> Result<u32> {
        self.persist_playlists_with(playlists, &self.writer).await
    }

    pub async fn persist_playlists_bulk(&self, playlists: &[Playlist]) -> Result<u32> {
        self.persist_playlists_with(playlists, &self.bulk_writer)
            .await
    }

    async fn persist_playlists_with(
        &self,
        playlists: &[Playlist],
        pool: &SqlitePool,
    ) -> Result<u32> {
        if playlists.is_empty() {
            return Ok(0);
        }
        let fetched_at_ms = now_ms();
        let media_items = playlists
            .iter()
            .map(playlist_media_item)
            .collect::<Vec<_>>();
        self.upsert_media_items_with(&media_items, "spotify", pool)
            .await?;
        for chunk in playlists.chunks(BULK_CHUNK_ROWS) {
            let mut tx = pool.begin().await?;
            for playlist in chunk {
                sqlx::query(
                    "INSERT INTO playlists (
                        id, uri, name, owner, tracks_total, image_url, fetched_at_ms,
                        snapshot_id, tracks_accessible, freshness_class, sync_generation
                     )
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT(id) DO UPDATE SET
                        uri = excluded.uri,
                        name = excluded.name,
                        owner = excluded.owner,
                        tracks_total = excluded.tracks_total,
                        image_url = excluded.image_url,
                        fetched_at_ms = excluded.fetched_at_ms,
                        snapshot_id = COALESCE(excluded.snapshot_id, playlists.snapshot_id),
                        tracks_accessible = CASE
                            WHEN COALESCE(excluded.snapshot_id, '') <> COALESCE(playlists.snapshot_id, '')
                            THEN 1
                            ELSE playlists.tracks_accessible
                        END,
                        freshness_class = excluded.freshness_class,
                        sync_generation = excluded.sync_generation",
                )
                .bind(&playlist.id)
                .bind(playlist_uri(&playlist.id))
                .bind(&playlist.name)
                .bind(&playlist.owner)
                .bind(playlist.tracks_total as i64)
                .bind(&playlist.image_url)
                .bind(fetched_at_ms)
                .bind(playlist.snapshot_id.as_deref())
                .bind(1_i64)
                .bind(FRESHNESS_FRESH)
                .bind(fetched_at_ms)
                .execute(&mut *tx)
                .await?;
            }
            tx.commit().await?;
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

    pub async fn playlist_tracks_accessible(&self, playlist_id: &str) -> Result<bool> {
        let accessible: Option<i64> =
            sqlx::query_scalar("SELECT tracks_accessible FROM playlists WHERE id = ?")
                .bind(playlist_id)
                .fetch_optional(&self.reader)
                .await?;
        Ok(accessible.unwrap_or(1) != 0)
    }

    pub async fn mark_playlist_tracks_inaccessible(&self, playlist_id: &str) -> Result<()> {
        sqlx::query(
            "UPDATE playlists
             SET tracks_accessible = 0, fetched_at_ms = ?
             WHERE id = ?",
        )
        .bind(now_ms())
        .bind(playlist_id)
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    /// Count of cached `playlist_items` rows for a given playlist.
    /// Used by the sync refetch gate to detect "snapshot matches but
    /// items missing" — i.e. a cache that was left empty by a partial
    /// failure during a previous persist. Without this check the
    /// snapshot-equality gate would skip refetching and the playlist
    /// would stay empty until Spotify-side mutations bumped the
    /// snapshot.
    pub async fn playlist_items_count(&self, playlist_id: &str) -> Result<u64> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM playlist_items WHERE playlist_id = ?")
                .bind(playlist_id)
                .fetch_one(&self.reader)
                .await?;
        Ok(count.max(0) as u64)
    }

    pub async fn persist_playlist_items(
        &self,
        playlist_id: &str,
        items: &[MediaItem],
    ) -> Result<u32> {
        self.persist_playlist_items_with(playlist_id, items, &self.writer)
            .await
    }

    pub async fn persist_playlist_items_bulk(
        &self,
        playlist_id: &str,
        items: &[MediaItem],
    ) -> Result<u32> {
        self.persist_playlist_items_with(playlist_id, items, &self.bulk_writer)
            .await
    }

    async fn persist_playlist_items_with(
        &self,
        playlist_id: &str,
        items: &[MediaItem],
        pool: &SqlitePool,
    ) -> Result<u32> {
        self.upsert_media_items_with(items, "spotify", pool).await?;
        let added_at_ms = now_ms();
        // DELETE + all INSERTs must run in a SINGLE transaction so the
        // playlist is never observed empty between the old set being
        // removed and the new set being committed. SQLite WAL gives
        // readers snapshot isolation, so they see either the prior
        // contents or the new contents — never the gap. An earlier
        // attempt at this code split DELETE into its own tx and put
        // INSERTs in per-chunk txs to release the writer lock more
        // often; that broke atomicity. If a partial failure left the
        // playlist empty, the playlist's `snapshot_id` was already
        // cached by `persist_playlists_with` so the sync refetch gate
        // (`should_refetch_playlist_tracks`) skipped repair on
        // subsequent runs and the playlist stayed empty forever.
        // Holding the writer for one playlist refresh (~50-100ms for
        // 500 tracks on local disk) is the correct trade-off.
        let mut tx = pool.begin().await?;
        sqlx::query("DELETE FROM playlist_items WHERE playlist_id = ?")
            .bind(playlist_id)
            .execute(&mut *tx)
            .await?;
        for (position, item) in items.iter().enumerate() {
            sqlx::query(
                "INSERT INTO playlist_items (
                    playlist_id, item_uri, position, added_at_ms,
                    freshness_class, sync_generation
                 )
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(playlist_id)
            .bind(&item.uri)
            .bind(position as i64)
            .bind(added_at_ms)
            .bind(FRESHNESS_FRESH)
            .bind(added_at_ms)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(items.len() as u32)
    }

    pub async fn persist_recent_items(&self, items: &[MediaItem]) -> Result<u32> {
        self.persist_recent_items_with(items, &self.writer).await
    }

    pub async fn persist_recent_items_bulk(&self, items: &[MediaItem]) -> Result<u32> {
        self.persist_recent_items_with(items, &self.bulk_writer)
            .await
    }

    async fn persist_recent_items_with(
        &self,
        items: &[MediaItem],
        pool: &SqlitePool,
    ) -> Result<u32> {
        if items.is_empty() {
            return Ok(0);
        }
        self.upsert_media_items_with(items, "spotify", pool).await?;
        let fetched_at_ms = now_ms();
        for (chunk_index, chunk) in items.chunks(BULK_CHUNK_ROWS).enumerate() {
            if chunk.is_empty() {
                continue;
            }
            let chunk_base = chunk_index * BULK_CHUNK_ROWS;
            let mut tx = pool.begin().await?;
            for (offset, item) in chunk.iter().enumerate() {
                let position = chunk_base + offset;
                sqlx::query(
                    "INSERT OR REPLACE INTO recent_items (
                        item_uri, played_at_ms, fetched_at_ms, position,
                        freshness_class, sync_generation
                     )
                     VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind(&item.uri)
                .bind(fetched_at_ms.saturating_sub(position as i64))
                .bind(fetched_at_ms)
                .bind(position as i64)
                .bind(FRESHNESS_FRESH)
                .bind(fetched_at_ms)
                .execute(&mut *tx)
                .await?;
            }
            tx.commit().await?;
        }
        Ok(items.len() as u32)
    }

    pub async fn persist_library_items(&self, items: &[MediaItem]) -> Result<u32> {
        self.persist_library_items_with(items, &self.writer).await
    }

    pub async fn persist_library_items_bulk(&self, items: &[MediaItem]) -> Result<u32> {
        self.persist_library_items_with(items, &self.bulk_writer)
            .await
    }

    async fn persist_library_items_with(
        &self,
        items: &[MediaItem],
        pool: &SqlitePool,
    ) -> Result<u32> {
        if items.is_empty() {
            return Ok(0);
        }
        self.upsert_media_items_with(items, "spotify", pool).await?;
        let fetched_at_ms = now_ms();
        for (chunk_index, chunk) in items.chunks(BULK_CHUNK_ROWS).enumerate() {
            if chunk.is_empty() {
                continue;
            }
            let chunk_base = chunk_index * BULK_CHUNK_ROWS;
            let mut tx = pool.begin().await?;
            for (offset, item) in chunk.iter().enumerate() {
                let position = chunk_base + offset;
                sqlx::query(
                    "INSERT INTO library_items (
                        item_uri, kind, saved, followed, fetched_at_ms,
                        freshness_class, sync_generation, sync_position
                     )
                     VALUES (?, ?, 1, 0, ?, ?, ?, ?)
                     ON CONFLICT(item_uri) DO UPDATE SET
                        kind = excluded.kind,
                        saved = 1,
                        fetched_at_ms = excluded.fetched_at_ms,
                        freshness_class = excluded.freshness_class,
                        sync_generation = excluded.sync_generation,
                        sync_position = excluded.sync_position",
                )
                .bind(&item.uri)
                .bind(item.kind.label())
                .bind(fetched_at_ms)
                .bind(FRESHNESS_FRESH)
                .bind(fetched_at_ms)
                .bind(position as i64)
                .execute(&mut *tx)
                .await?;
                sqlx::query("UPDATE media_items SET saved = 1, liked = 1 WHERE uri = ?")
                    .bind(&item.uri)
                    .execute(&mut *tx)
                    .await?;
            }
            tx.commit().await?;
        }
        Ok(items.len() as u32)
    }

    /// Persist followed artists: upsert their media rows, then mark them
    /// `followed=1` in `library_items`. Unlike saved albums/tracks, artists
    /// are *followed* (not "saved"), so this writes `saved=0, followed=1` and
    /// does not flip `media_items.saved/liked` — keeping the saved-album set
    /// (used for `in_library` tagging) clean.
    pub async fn persist_followed_artists(&self, artists: &[MediaItem]) -> Result<u32> {
        if artists.is_empty() {
            return Ok(0);
        }
        self.upsert_media_items_with(artists, "spotify", &self.bulk_writer)
            .await?;
        let fetched_at_ms = now_ms();
        for chunk in artists.chunks(BULK_CHUNK_ROWS) {
            let mut tx = self.bulk_writer.begin().await?;
            for item in chunk {
                sqlx::query(
                    "INSERT INTO library_items (item_uri, kind, saved, followed, fetched_at_ms)
                     VALUES (?, ?, 0, 1, ?)
                     ON CONFLICT(item_uri) DO UPDATE SET
                        kind = excluded.kind,
                        followed = 1,
                        fetched_at_ms = excluded.fetched_at_ms",
                )
                .bind(&item.uri)
                .bind(item.kind.label())
                .bind(fetched_at_ms)
                .execute(&mut *tx)
                .await?;
            }
            tx.commit().await?;
        }
        Ok(artists.len() as u32)
    }

    pub async fn record_sync_event(
        &self,
        domain: &str,
        started_at_ms: i64,
        status: &str,
        row_count: u32,
        error: Option<&str>,
    ) -> Result<()> {
        self.record_sync_event_with(
            SyncEventRecord {
                domain,
                started_at_ms,
                status,
                row_count,
                error,
                retry_after_secs: None,
            },
            &self.writer,
        )
        .await
    }

    pub async fn record_sync_event_with_retry_after(
        &self,
        domain: &str,
        started_at_ms: i64,
        status: &str,
        row_count: u32,
        error: Option<&str>,
        retry_after_secs: Option<u64>,
    ) -> Result<()> {
        self.record_sync_event_with(
            SyncEventRecord {
                domain,
                started_at_ms,
                status,
                row_count,
                error,
                retry_after_secs,
            },
            &self.writer,
        )
        .await
    }

    pub async fn record_sync_event_bulk(
        &self,
        domain: &str,
        started_at_ms: i64,
        status: &str,
        row_count: u32,
        error: Option<&str>,
    ) -> Result<()> {
        self.record_sync_event_with(
            SyncEventRecord {
                domain,
                started_at_ms,
                status,
                row_count,
                error,
                retry_after_secs: None,
            },
            &self.bulk_writer,
        )
        .await
    }

    pub async fn record_sync_event_bulk_with_retry_after(
        &self,
        domain: &str,
        started_at_ms: i64,
        status: &str,
        row_count: u32,
        error: Option<&str>,
        retry_after_secs: Option<u64>,
    ) -> Result<()> {
        self.record_sync_event_with(
            SyncEventRecord {
                domain,
                started_at_ms,
                status,
                row_count,
                error,
                retry_after_secs,
            },
            &self.bulk_writer,
        )
        .await
    }

    async fn record_sync_event_with(
        &self,
        event: SyncEventRecord<'_>,
        pool: &SqlitePool,
    ) -> Result<()> {
        let finished_at_ms = now_ms();
        sqlx::query(
            "INSERT INTO sync_events (
                domain, started_at_ms, finished_at_ms, status, row_count, error, retry_after_secs
             )
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(event.domain)
        .bind(event.started_at_ms)
        .bind(finished_at_ms)
        .bind(event.status)
        .bind(event.row_count as i64)
        .bind(event.error)
        .bind(
            event
                .retry_after_secs
                .and_then(|secs| i64::try_from(secs).ok()),
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO sync_cursors (domain, last_success_at_ms, last_error)
             VALUES (?, ?, ?)
             ON CONFLICT(domain) DO UPDATE SET
                last_success_at_ms = CASE WHEN ? = 'ok' THEN excluded.last_success_at_ms ELSE sync_cursors.last_success_at_ms END,
                last_error = excluded.last_error",
        )
        .bind(event.domain)
        .bind(if event.status == "ok" {
            Some(finished_at_ms)
        } else {
            None
        })
        .bind(event.error)
        .bind(event.status)
        .execute(pool)
        .await?;
        Ok(())
    }

    pub async fn rate_limit_cooldown_remaining_ms(&self, domain: &str) -> Result<Option<i64>> {
        let row: Option<(i64, Option<String>, Option<i64>)> = sqlx::query_as(
            "SELECT finished_at_ms, error, retry_after_secs
             FROM sync_events
             WHERE domain = ? AND (retry_after_secs IS NOT NULL OR error IS NOT NULL)
             ORDER BY finished_at_ms DESC
             LIMIT 1",
        )
        .bind(domain)
        .fetch_optional(&self.reader)
        .await?;
        let Some((finished_at_ms, error, retry_after_secs)) = row else {
            return Ok(None);
        };
        let retry_after_secs =
            retry_after_secs.or_else(|| error.as_deref().and_then(legacy_retry_after_seconds));
        let Some(retry_after_secs) = retry_after_secs else {
            return Ok(None);
        };
        let retry_until_ms = finished_at_ms.saturating_add(retry_after_secs.saturating_mul(1000));
        let remaining_ms = retry_until_ms.saturating_sub(now_ms());
        Ok((remaining_ms > 0).then_some(remaining_ms))
    }

    /// Phase 13 (P13-J) — drop search-cache rows older than the cutoff.
    /// CASCADE in `search_results` handles the join table. Routes
    /// through the bulk writer: retention is by definition background
    /// work and shouldn't compete with a Pause command on the hot
    /// pool.
    pub async fn prune_search_runs_older_than(&self, cutoff_ms: i64) -> Result<u64> {
        let result = sqlx::query("DELETE FROM search_runs WHERE fetched_at_ms < ?")
            .bind(cutoff_ms)
            .execute(&self.bulk_writer)
            .await?;
        Ok(result.rows_affected())
    }

    pub async fn cached_lyrics(
        &self,
        track_uri: &str,
        ttl: Duration,
    ) -> Result<Option<SyncedLyrics>> {
        let cutoff_ms = now_ms().saturating_sub(ttl.as_millis() as i64);
        let row = sqlx::query(
            "SELECT provider, synced, lines_json, fetched_at_ms, language, source_url
             FROM lyrics_cache
             WHERE track_uri = ? AND fetched_at_ms >= ?",
        )
        .bind(track_uri)
        .bind(cutoff_ms)
        .fetch_optional(&self.reader)
        .await?;
        row.map(|row| row_to_lyrics(track_uri, row)).transpose()
    }

    // --- Listening reminders + notifications ---

    pub async fn create_reminder(&self, r: &Reminder) -> Result<()> {
        sqlx::query(
            "INSERT INTO reminder_schedules (
                id, media_uri, media_kind, name, subtitle, image_url, anchor_at_ms,
                recurrence, tz, next_due_at_ms, state, message, created_at_ms
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&r.id)
        .bind(&r.media_uri)
        .bind(r.media_kind.label())
        .bind(&r.name)
        .bind(&r.subtitle)
        .bind(&r.image_url)
        .bind(r.anchor_at_ms)
        .bind(r.recurrence.label())
        .bind(&r.tz)
        .bind(r.next_due_at_ms)
        .bind(reminder_state_label(r.state))
        .bind(&r.message)
        .bind(r.created_at_ms)
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    pub async fn list_reminders(&self, include_inactive: bool) -> Result<Vec<Reminder>> {
        let sql = if include_inactive {
            "SELECT * FROM reminder_schedules ORDER BY next_due_at_ms ASC"
        } else {
            "SELECT * FROM reminder_schedules WHERE state = 'active' ORDER BY next_due_at_ms ASC"
        };
        let rows = sqlx::query(sql).fetch_all(&self.reader).await?;
        rows.into_iter().map(row_to_reminder).collect()
    }

    pub async fn get_reminder(&self, id: &str) -> Result<Option<Reminder>> {
        let row = sqlx::query("SELECT * FROM reminder_schedules WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.reader)
            .await?;
        row.map(row_to_reminder).transpose()
    }

    pub async fn cancel_reminder(&self, id: &str) -> Result<()> {
        sqlx::query("UPDATE reminder_schedules SET state = 'cancelled' WHERE id = ?")
            .bind(id)
            .execute(&self.writer)
            .await?;
        Ok(())
    }

    /// Active schedules whose next occurrence is at/<= `now_ms`.
    pub async fn due_reminders(&self, now_ms: i64) -> Result<Vec<Reminder>> {
        let rows = sqlx::query(
            "SELECT * FROM reminder_schedules
             WHERE state = 'active' AND next_due_at_ms <= ?
             ORDER BY next_due_at_ms ASC",
        )
        .bind(now_ms)
        .fetch_all(&self.reader)
        .await?;
        rows.into_iter().map(row_to_reminder).collect()
    }

    pub async fn advance_reminder(&self, id: &str, next_due_at_ms: i64) -> Result<()> {
        sqlx::query("UPDATE reminder_schedules SET next_due_at_ms = ? WHERE id = ?")
            .bind(next_due_at_ms)
            .bind(id)
            .execute(&self.writer)
            .await?;
        Ok(())
    }

    pub async fn complete_reminder(&self, id: &str) -> Result<()> {
        sqlx::query("UPDATE reminder_schedules SET state = 'completed' WHERE id = ?")
            .bind(id)
            .execute(&self.writer)
            .await?;
        Ok(())
    }

    pub async fn insert_notification(&self, n: &Notification) -> Result<()> {
        sqlx::query(
            "INSERT INTO reminder_notifications (
                id, reminder_id, media_uri, media_kind, name, subtitle, image_url,
                due_at_ms, fired_at_ms, state, snoozed_until_ms, acted, message, created_at_ms
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&n.id)
        .bind(&n.reminder_id)
        .bind(&n.media_uri)
        .bind(n.media_kind.label())
        .bind(&n.name)
        .bind(&n.subtitle)
        .bind(&n.image_url)
        .bind(n.due_at_ms)
        .bind(n.fired_at_ms)
        .bind(notification_state_label(n.state))
        .bind(n.snoozed_until_ms)
        .bind(&n.acted)
        .bind(&n.message)
        .bind(now_ms())
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    pub async fn list_notifications(&self, include_archived: bool) -> Result<Vec<Notification>> {
        let sql = if include_archived {
            "SELECT * FROM reminder_notifications ORDER BY due_at_ms DESC"
        } else {
            "SELECT * FROM reminder_notifications
             WHERE state NOT IN ('dismissed', 'done') ORDER BY due_at_ms DESC"
        };
        let rows = sqlx::query(sql).fetch_all(&self.reader).await?;
        rows.into_iter().map(row_to_notification).collect()
    }

    pub async fn get_notification(&self, id: &str) -> Result<Option<Notification>> {
        let row = sqlx::query("SELECT * FROM reminder_notifications WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.reader)
            .await?;
        row.map(row_to_notification).transpose()
    }

    pub async fn set_notification_state(
        &self,
        id: &str,
        state: NotificationState,
        snoozed_until_ms: Option<i64>,
        acted: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE reminder_notifications
             SET state = ?, snoozed_until_ms = ?, acted = COALESCE(?, acted)
             WHERE id = ?",
        )
        .bind(notification_state_label(state))
        .bind(snoozed_until_ms)
        .bind(acted)
        .bind(id)
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    /// Snoozed notifications whose `snoozed_until_ms` is at/<= `now_ms`.
    pub async fn due_snoozed_notifications(&self, now_ms: i64) -> Result<Vec<Notification>> {
        let rows = sqlx::query(
            "SELECT * FROM reminder_notifications
             WHERE state = 'snoozed' AND snoozed_until_ms IS NOT NULL AND snoozed_until_ms <= ?
             ORDER BY snoozed_until_ms ASC",
        )
        .bind(now_ms)
        .fetch_all(&self.reader)
        .await?;
        rows.into_iter().map(row_to_notification).collect()
    }

    /// Earliest time the scheduler must wake: min of active schedules'
    /// `next_due_at_ms` and snoozed notifications' `snoozed_until_ms`.
    pub async fn next_reminder_wake_ms(&self) -> Result<Option<i64>> {
        let row: Option<(Option<i64>,)> = sqlx::query_as(
            "SELECT MIN(t) FROM (
                SELECT next_due_at_ms AS t FROM reminder_schedules WHERE state = 'active'
                UNION ALL
                SELECT snoozed_until_ms AS t FROM reminder_notifications
                    WHERE state = 'snoozed' AND snoozed_until_ms IS NOT NULL
             )",
        )
        .fetch_optional(&self.reader)
        .await?;
        Ok(row.and_then(|r| r.0))
    }

    pub async fn upsert_lyrics(&self, lyrics: &SyncedLyrics) -> Result<()> {
        let lines_json = serde_json::to_string(&lyrics.lines)?;
        sqlx::query(
            "INSERT INTO lyrics_cache (
                track_uri, provider, synced, lines_json, fetched_at_ms, language, source_url
             ) VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(track_uri) DO UPDATE SET
                provider = excluded.provider,
                synced = excluded.synced,
                lines_json = excluded.lines_json,
                fetched_at_ms = excluded.fetched_at_ms,
                language = excluded.language,
                source_url = excluded.source_url",
        )
        .bind(&lyrics.track_uri)
        .bind(lyrics.provider.label())
        .bind(if lyrics.synced { 1_i64 } else { 0_i64 })
        .bind(lines_json)
        .bind(lyrics.fetched_at_ms)
        .bind(&lyrics.language)
        .bind(&lyrics.source_url)
        .execute(&self.writer)
        .await?;
        self.clear_lyrics_lookup_failure(&lyrics.track_uri).await?;
        Ok(())
    }

    pub async fn lyrics_lookup_blocked(&self, track_uri: &str) -> Result<bool> {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT unavailable_until_ms
             FROM lyrics_lookup_failures
             WHERE track_uri = ? AND unavailable_until_ms > ?",
        )
        .bind(track_uri)
        .bind(now_ms())
        .fetch_optional(&self.reader)
        .await?;
        Ok(row.is_some())
    }

    pub async fn upsert_lyrics_lookup_failure(
        &self,
        track_uri: &str,
        reason: &str,
        ttl: Duration,
    ) -> Result<()> {
        let failed_at_ms = now_ms();
        let unavailable_until_ms = failed_at_ms.saturating_add(ttl.as_millis() as i64);
        sqlx::query(
            "INSERT INTO lyrics_lookup_failures (
                track_uri, failed_at_ms, unavailable_until_ms, reason
             ) VALUES (?, ?, ?, ?)
             ON CONFLICT(track_uri) DO UPDATE SET
                failed_at_ms = excluded.failed_at_ms,
                unavailable_until_ms = excluded.unavailable_until_ms,
                reason = excluded.reason",
        )
        .bind(track_uri)
        .bind(failed_at_ms)
        .bind(unavailable_until_ms)
        .bind(reason)
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    pub async fn clear_lyrics_lookup_failure(&self, track_uri: &str) -> Result<()> {
        sqlx::query("DELETE FROM lyrics_lookup_failures WHERE track_uri = ?")
            .bind(track_uri)
            .execute(&self.writer)
            .await?;
        Ok(())
    }

    pub async fn lyrics_offset_ms(&self, track_uri: &str) -> Result<i64> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT offset_ms FROM lyrics_offsets WHERE track_uri = ?")
                .bind(track_uri)
                .fetch_optional(&self.reader)
                .await?;
        Ok(row.map_or(0, |(offset_ms,)| offset_ms))
    }

    pub async fn set_lyrics_offset_ms(&self, track_uri: &str, offset_ms: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO lyrics_offsets (track_uri, offset_ms, updated_at_ms)
             VALUES (?, ?, ?)
             ON CONFLICT(track_uri) DO UPDATE SET
                offset_ms = excluded.offset_ms,
                updated_at_ms = excluded.updated_at_ms",
        )
        .bind(track_uri)
        .bind(offset_ms)
        .bind(now_ms())
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    pub async fn cache_status(&self, index_documents: u64) -> Result<CacheStatus> {
        Ok(CacheStatus {
            database_path: self.db_path.display().to_string(),
            index_path: self.index_path.display().to_string(),
            cover_cache_path: String::new(),
            media_items: count_rows(&self.reader, "SELECT COUNT(*) FROM media_items").await?,
            devices: count_rows(&self.reader, "SELECT COUNT(*) FROM devices").await?,
            playback_snapshots: count_rows(&self.reader, "SELECT COUNT(*) FROM playback_snapshots")
                .await?,
            queue_snapshots: count_rows(&self.reader, "SELECT COUNT(*) FROM queue_snapshots")
                .await?,
            queue_items: count_rows(&self.reader, "SELECT COUNT(*) FROM queue_items").await?,
            playlists: count_rows(&self.reader, "SELECT COUNT(*) FROM playlists").await?,
            playlist_items: count_rows(&self.reader, "SELECT COUNT(*) FROM playlist_items").await?,
            recent_items: count_rows(&self.reader, "SELECT COUNT(*) FROM recent_items").await?,
            library_items: count_rows(&self.reader, "SELECT COUNT(*) FROM library_items").await?,
            search_runs: count_rows(&self.reader, "SELECT COUNT(*) FROM search_runs").await?,
            search_results: count_rows(&self.reader, "SELECT COUNT(*) FROM search_results").await?,
            sync_events: count_rows(&self.reader, "SELECT COUNT(*) FROM sync_events").await?,
            lyrics_cache: count_rows(&self.reader, "SELECT COUNT(*) FROM lyrics_cache").await?,
            lyrics_offsets: count_rows(&self.reader, "SELECT COUNT(*) FROM lyrics_offsets").await?,
            cover_cache_files: 0,
            cover_cache_bytes: 0,
            cover_cache_oldest_entry_ms: None,
            cover_cache_ttl_secs: 0,
            cover_cache_max_bytes: 0,
            index_documents,
            last_sync_at_ms: max_i64(&self.reader, "SELECT MAX(finished_at_ms) FROM sync_events")
                .await?,
            last_search_at_ms: max_i64(&self.reader, "SELECT MAX(fetched_at_ms) FROM search_runs")
                .await?,
            freshness: CacheFreshnessStatus {
                media_items: freshness_counts(&self.reader, "media_items").await?,
                devices: freshness_counts(&self.reader, "devices").await?,
                playback_snapshots: freshness_counts(&self.reader, "playback_snapshots").await?,
                queue_snapshots: freshness_counts(&self.reader, "queue_snapshots").await?,
                queue_items: freshness_counts(&self.reader, "queue_items").await?,
                playlists: freshness_counts(&self.reader, "playlists").await?,
                playlist_items: freshness_counts(&self.reader, "playlist_items").await?,
                recent_items: freshness_counts(&self.reader, "recent_items").await?,
                library_items: freshness_counts(&self.reader, "library_items").await?,
            },
        })
    }

    async fn run_migrations(&self) -> Result<()> {
        self.ensure_schema_migrations_table().await?;

        for migration in MIGRATIONS {
            self.apply_migration(migration).await?;
        }

        self.validate_schema().await?;
        Ok(())
    }

    async fn ensure_schema_migrations_table(&self) -> Result<()> {
        sqlx::raw_sql(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                applied_at_ms INTEGER NOT NULL
            );",
        )
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    /// Apply a single migration if not already at this version. Each
    /// migration body is responsible for being idempotent in its SQL
    /// (we use `CREATE TABLE/INDEX IF NOT EXISTS`), so a crash between
    /// running the body and stamping the version row replays cleanly on
    /// the next start.
    async fn apply_migration(&self, migration: &Migration) -> Result<()> {
        if self.is_migration_applied(migration.version).await? {
            return Ok(());
        }
        match migration.kind {
            MigrationKind::Sql(sql) => {
                sqlx::raw_sql(sql).execute(&self.writer).await?;
            }
            MigrationKind::AddColumns(columns) => {
                for column in columns {
                    self.add_column_if_missing(column).await?;
                }
            }
            MigrationKind::RebuildPlaylistItemsPositionPk => {
                self.rebuild_playlist_items_position_pk().await?;
            }
        }
        sqlx::query(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?, ?, ?)",
        )
        .bind(migration.version as i64)
        .bind(migration.name)
        .bind(now_ms())
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    /// Force-run migrations again. Used by tests to assert idempotency.
    #[doc(hidden)]
    pub async fn run_migrations_idempotent_for_test(&self) -> Result<()> {
        self.run_migrations().await
    }

    /// Operator repair path for `spotuify cache repair`.
    ///
    /// Replays idempotent migrations and validates required columns.
    /// Search repair is handled by the CLI/daemon caller because the
    /// Tantivy index lives outside SQLite and is rebuildable.
    pub async fn repair_schema(&self) -> Result<()> {
        self.run_migrations().await
    }

    /// Read-side connection pool. Used by tests + downstream introspection.
    pub fn reader(&self) -> &SqlitePool {
        &self.reader
    }

    /// Background-write pool. Exposed so the daemon's retention loop
    /// and other genuinely-bulk writers can pick the right lane.
    /// Day-to-day callers should prefer the per-method `_bulk`
    /// variants (e.g. `persist_playback_bulk`) which already route
    /// here and chunk transactions appropriately.
    pub fn bulk_writer(&self) -> &SqlitePool {
        &self.bulk_writer
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

    async fn is_migration_applied(&self, version: u32) -> Result<bool> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT version FROM schema_migrations WHERE version = ?")
                .bind(version as i64)
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

    async fn add_column_if_missing(&self, column: &ColumnMigration) -> Result<()> {
        if self.column_exists(column.table, column.name).await? {
            return Ok(());
        }
        let sql = format!(
            "ALTER TABLE {} ADD COLUMN {}",
            column.table, column.definition
        );
        sqlx::query(&sql).execute(&self.writer).await?;
        Ok(())
    }

    async fn playlist_items_uses_position_pk(&self) -> Result<bool> {
        let rows = sqlx::query("PRAGMA table_info(playlist_items)")
            .fetch_all(&self.writer)
            .await?;
        let mut pk_columns = rows
            .iter()
            .filter_map(|row| {
                let pk = row.get::<i64, _>("pk");
                (pk > 0).then(|| (pk, row.get::<String, _>("name")))
            })
            .collect::<Vec<_>>();
        pk_columns.sort_by_key(|(pk, _)| *pk);
        Ok(pk_columns
            .into_iter()
            .map(|(_, name)| name)
            .collect::<Vec<_>>()
            == ["playlist_id", "position"])
    }

    async fn rebuild_playlist_items_position_pk(&self) -> Result<()> {
        if self.playlist_items_uses_position_pk().await? {
            return Ok(());
        }
        sqlx::raw_sql(
            r#"
CREATE TABLE IF NOT EXISTS playlist_items_v9 (
    playlist_id          TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    item_uri             TEXT NOT NULL REFERENCES media_items(uri) ON DELETE CASCADE,
    position             INTEGER NOT NULL,
    added_at_ms          INTEGER NOT NULL,
    snapshot_id_at_fetch TEXT,
    freshness_class      TEXT NOT NULL DEFAULT 'unknown',
    sync_generation      INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (playlist_id, position)
);

INSERT OR REPLACE INTO playlist_items_v9 (
    playlist_id, item_uri, position, added_at_ms,
    snapshot_id_at_fetch, freshness_class, sync_generation
)
SELECT playlist_id, item_uri, position, added_at_ms,
       snapshot_id_at_fetch, freshness_class, sync_generation
FROM playlist_items
ORDER BY playlist_id, position;

DROP TABLE playlist_items;
ALTER TABLE playlist_items_v9 RENAME TO playlist_items;
CREATE INDEX IF NOT EXISTS idx_playlist_items_item ON playlist_items(item_uri);
"#,
        )
        .execute(&self.writer)
        .await?;
        Ok(())
    }
}

async fn build_writer_pool(db_url: &str) -> Result<SqlitePool> {
    let opts = SqliteConnectOptions::from_str(db_url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(BUSY_TIMEOUT)
        .pragma("foreign_keys", "ON");
    Ok(SqlitePoolOptions::new()
        .max_connections(1)
        .acquire_timeout(POOL_ACQUIRE_TIMEOUT)
        .connect_with(opts)
        .await?)
}

fn secure_sqlite_files(db_path: &Path) -> Result<()> {
    spotuify_protocol::paths::secure_private_file_if_exists(db_path)?;
    spotuify_protocol::paths::secure_private_file_if_exists(&sqlite_sidecar_path(db_path, "-wal"))?;
    spotuify_protocol::paths::secure_private_file_if_exists(&sqlite_sidecar_path(db_path, "-shm"))?;
    Ok(())
}

fn sqlite_sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{}", db_path.display(), suffix))
}

pub fn cache_db_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("SPOTUIFY_CACHE_DB") {
        return Ok(PathBuf::from(path));
    }
    Ok(spotuify_protocol::paths::data_dir().join("cache.sqlite3"))
}

pub fn search_index_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("SPOTUIFY_SEARCH_INDEX") {
        return Ok(PathBuf::from(path));
    }
    Ok(spotuify_protocol::paths::data_dir().join("search_index"))
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

/// The album/context label of a media item for session grouping: prefer the
/// album name, fall back to `context`, else `None`.
fn session_label(item: &MediaItem) -> Option<&str> {
    item.album
        .as_deref()
        .filter(|s| !s.is_empty())
        .or_else(|| Some(item.context.as_str()).filter(|s| !s.is_empty()))
}

/// The most common album/context label across a session's tracks, used as the
/// session-albums view's title. `None` when nothing stands out (no items, or
/// all blank). Ties resolve to the first-seen (newest) label.
fn dominant_context(items: &[MediaItem]) -> Option<String> {
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for item in items {
        if let Some(label) = session_label(item) {
            *counts.entry(label).or_insert(0) += 1;
        }
    }
    items
        .iter()
        .filter_map(session_label)
        .max_by_key(|label| counts.get(label).copied().unwrap_or(0))
        .map(str::to_string)
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
        kind: row.get::<String, _>("kind").parse::<MediaKind>()?,
        source: Some(row.get("source")),
        freshness: Some("cached".to_string()),
        explicit: None,
        is_playable: None,
        // Phase v2 columns — read resiliently so queries that don't SELECT
        // them (the common case) still map cleanly.
        album: row.try_get::<Option<String>, _>("album").ok().flatten(),
        added_at_ms: row.try_get::<Option<i64>, _>("added_at_ms").ok().flatten(),
        resume_position_ms: row
            .try_get::<Option<i64>, _>("resume_position_ms")
            .ok()
            .flatten()
            .map(|v| v.max(0) as u64),
        fully_played: row
            .try_get::<Option<i64>, _>("fully_played")
            .ok()
            .flatten()
            .map(|v| v != 0),
        release_date: row
            .try_get::<Option<String>, _>("release_date")
            .ok()
            .flatten(),
        // Not persisted: `album_group` flows live from the provider for the
        // discography view, `in_library` is tagged by the daemon per query, and
        // `genre` flows live (Spotify carries it on artist/album, not track).
        album_group: None,
        in_library: None,
        genre: None,
        // Navigation refs — read resiliently; absent on SELECTs that don't
        // project them and on rows written before migration v16.
        album_uri: row.try_get::<Option<String>, _>("album_uri").ok().flatten(),
        artists: row
            .try_get::<Option<String>, _>("artists_json")
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str::<Vec<ArtistRef>>(&json).ok())
            .unwrap_or_default(),
    })
}

fn row_to_device(row: sqlx::sqlite::SqliteRow) -> Result<Device> {
    Ok(Device {
        id: row.get("id"),
        name: row.get("name"),
        kind: row.get("kind"),
        is_active: row.get("is_active"),
        is_restricted: row.get("is_restricted"),
        supports_volume: row.get("supports_volume"),
        volume_percent: row
            .get::<Option<i64>, _>("volume_percent")
            .and_then(|value| u8::try_from(value).ok()),
    })
}

fn row_to_playlist(row: sqlx::sqlite::SqliteRow) -> Result<Playlist> {
    Ok(Playlist {
        id: row.get("id"),
        name: row.get("name"),
        owner: row.get("owner"),
        tracks_total: row.get::<i64, _>("tracks_total").max(0) as u64,
        image_url: row.get("image_url"),
        snapshot_id: row.get("snapshot_id"),
    })
}

fn row_to_lyrics(track_uri: &str, row: sqlx::sqlite::SqliteRow) -> Result<SyncedLyrics> {
    let provider: String = row.get("provider");
    let provider = provider.parse::<LyricsProvider>()?;
    let lines_json: String = row.get("lines_json");
    let lines: Vec<LyricLine> = serde_json::from_str(&lines_json)?;
    let synced: i64 = row.get("synced");
    Ok(SyncedLyrics {
        provider,
        track_uri: track_uri.to_string(),
        lines,
        fetched_at_ms: row.get("fetched_at_ms"),
        synced: synced != 0,
        language: row.get("language"),
        source_url: row.get("source_url"),
    })
}

fn reminder_state_label(state: ReminderState) -> &'static str {
    match state {
        ReminderState::Active => "active",
        ReminderState::Completed => "completed",
        ReminderState::Cancelled => "cancelled",
    }
}

fn reminder_state_from_label(label: &str) -> ReminderState {
    match label {
        "completed" => ReminderState::Completed,
        "cancelled" => ReminderState::Cancelled,
        _ => ReminderState::Active,
    }
}

fn notification_state_label(state: NotificationState) -> &'static str {
    match state {
        NotificationState::Unseen => "unseen",
        NotificationState::Seen => "seen",
        NotificationState::Snoozed => "snoozed",
        NotificationState::Dismissed => "dismissed",
        NotificationState::Done => "done",
    }
}

fn notification_state_from_label(label: &str) -> NotificationState {
    match label {
        "seen" => NotificationState::Seen,
        "snoozed" => NotificationState::Snoozed,
        "dismissed" => NotificationState::Dismissed,
        "done" => NotificationState::Done,
        _ => NotificationState::Unseen,
    }
}

fn row_to_reminder(row: sqlx::sqlite::SqliteRow) -> Result<Reminder> {
    Ok(Reminder {
        id: row.get("id"),
        media_uri: row.get("media_uri"),
        media_kind: row.get::<String, _>("media_kind").parse::<MediaKind>()?,
        name: row.get("name"),
        subtitle: row.get("subtitle"),
        image_url: row.get("image_url"),
        anchor_at_ms: row.get("anchor_at_ms"),
        recurrence: Recurrence::parse(&row.get::<String, _>("recurrence")).unwrap_or_default(),
        tz: row.get("tz"),
        next_due_at_ms: row.get("next_due_at_ms"),
        state: reminder_state_from_label(&row.get::<String, _>("state")),
        message: row.get("message"),
        created_at_ms: row.get("created_at_ms"),
    })
}

fn row_to_notification(row: sqlx::sqlite::SqliteRow) -> Result<Notification> {
    Ok(Notification {
        id: row.get("id"),
        reminder_id: row.get("reminder_id"),
        media_uri: row.get("media_uri"),
        media_kind: row.get::<String, _>("media_kind").parse::<MediaKind>()?,
        name: row.get("name"),
        subtitle: row.get("subtitle"),
        image_url: row.get("image_url"),
        due_at_ms: row.get("due_at_ms"),
        fired_at_ms: row.get("fired_at_ms"),
        state: notification_state_from_label(&row.get::<String, _>("state")),
        snoozed_until_ms: row.get("snoozed_until_ms"),
        acted: row.get("acted"),
        message: row.get("message"),
    })
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
        ..Default::default()
    }
}

fn playlist_uri(playlist_id: &str) -> String {
    if playlist_id.starts_with("spotify:playlist:") {
        playlist_id.to_string()
    } else {
        format!("spotify:playlist:{playlist_id}")
    }
}

fn legacy_retry_after_seconds(message: &str) -> Option<i64> {
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

async fn freshness_counts(pool: &SqlitePool, table: &str) -> Result<FreshnessCounts> {
    let sql = format!(
        "SELECT
            COALESCE(SUM(CASE WHEN freshness_class = 'fresh' THEN 1 ELSE 0 END), 0) AS fresh,
            COALESCE(SUM(CASE WHEN freshness_class = 'stale_but_usable' THEN 1 ELSE 0 END), 0) AS stale_but_usable,
            COALESCE(SUM(CASE WHEN freshness_class = 'refreshing' THEN 1 ELSE 0 END), 0) AS refreshing,
            COALESCE(SUM(CASE WHEN freshness_class = 'failed_refresh' THEN 1 ELSE 0 END), 0) AS failed_refresh,
            COALESCE(SUM(CASE WHEN freshness_class = 'unknown' THEN 1 ELSE 0 END), 0) AS unknown,
            COALESCE(MAX(sync_generation), 0) AS max_sync_generation
         FROM {table}"
    );
    let row = sqlx::query(&sql).fetch_one(pool).await?;
    Ok(FreshnessCounts {
        fresh: row.get::<i64, _>("fresh").max(0) as u32,
        stale_but_usable: row.get::<i64, _>("stale_but_usable").max(0) as u32,
        refreshing: row.get::<i64, _>("refreshing").max(0) as u32,
        failed_refresh: row.get::<i64, _>("failed_refresh").max(0) as u32,
        unknown: row.get::<i64, _>("unknown").max(0) as u32,
        max_sync_generation: row.get::<i64, _>("max_sync_generation").max(0),
    })
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
    fetched_at_ms INTEGER NOT NULL,
    tracks_accessible INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS playlist_items (
    playlist_id TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    item_uri    TEXT NOT NULL REFERENCES media_items(uri) ON DELETE CASCADE,
    position    INTEGER NOT NULL,
    added_at_ms INTEGER NOT NULL,
    PRIMARY KEY (playlist_id, position)
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
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    domain           TEXT NOT NULL,
    started_at_ms    INTEGER NOT NULL,
    finished_at_ms   INTEGER NOT NULL,
    status           TEXT NOT NULL,
    row_count        INTEGER NOT NULL,
    error            TEXT,
    retry_after_secs INTEGER
);
CREATE INDEX IF NOT EXISTS idx_sync_events_domain_time ON sync_events(domain, finished_at_ms DESC);

CREATE TABLE IF NOT EXISTS sync_cursors (
    domain             TEXT PRIMARY KEY,
    last_success_at_ms INTEGER,
    last_error         TEXT
);
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

/// Phase 10: analytics derivations. Adds `listen_facts` (one row per
/// finalised listening session), per-entity rollups
/// (`track_metrics`/`artist_metrics`/`album_metrics`), `habit_metrics`
/// (day/week/month buckets), `qualification_rules` (versioned threshold
/// table so future tweaks don't retroactively change history), and
/// `playback_progress` (raw sample-rate-anchored progress samples
/// pruned at 90d).
///
/// Listen qualification rule v1: audible_ms >= max(30s, min(50% of
/// duration, 4min)) AND duration_ms > 30s. The rule version is stamped
/// on every `listen_facts` row so changing the math later doesn't
/// invalidate existing data.
const MIGRATION_004_ANALYTICS_DERIVATIONS: &str = r#"
CREATE TABLE IF NOT EXISTS listen_facts (
    id                          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id                  TEXT NOT NULL,
    track_uri                   TEXT NOT NULL,
    artist_uri                  TEXT,
    album_uri                   TEXT,
    started_at_ms               INTEGER NOT NULL,
    ended_at_ms                 INTEGER NOT NULL,
    duration_ms                 INTEGER NOT NULL,
    elapsed_ms                  INTEGER NOT NULL,
    audible_ms                  INTEGER NOT NULL,
    completion_ratio            REAL NOT NULL,
    qualified                   INTEGER NOT NULL,
    qualification_rule_version  INTEGER NOT NULL,
    skip_reason                 TEXT,
    source                      TEXT,
    backend                     TEXT,
    private_session             INTEGER NOT NULL DEFAULT 0,
    created_at_ms               INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_listen_facts_started
    ON listen_facts(started_at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_listen_facts_track_qual
    ON listen_facts(track_uri, qualified);
CREATE INDEX IF NOT EXISTS idx_listen_facts_artist_qual
    ON listen_facts(artist_uri, qualified);
CREATE INDEX IF NOT EXISTS idx_listen_facts_session
    ON listen_facts(session_id);

CREATE TABLE IF NOT EXISTS track_metrics (
    track_uri             TEXT PRIMARY KEY,
    qualified_count       INTEGER NOT NULL DEFAULT 0,
    skip_count            INTEGER NOT NULL DEFAULT 0,
    total_audible_ms      INTEGER NOT NULL DEFAULT 0,
    last_listened_at_ms   INTEGER,
    first_listened_at_ms  INTEGER,
    updated_at_ms         INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS artist_metrics (
    artist_uri            TEXT PRIMARY KEY,
    qualified_count       INTEGER NOT NULL DEFAULT 0,
    skip_count            INTEGER NOT NULL DEFAULT 0,
    total_audible_ms      INTEGER NOT NULL DEFAULT 0,
    last_listened_at_ms   INTEGER,
    first_listened_at_ms  INTEGER,
    updated_at_ms         INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS album_metrics (
    album_uri             TEXT PRIMARY KEY,
    qualified_count       INTEGER NOT NULL DEFAULT 0,
    skip_count            INTEGER NOT NULL DEFAULT 0,
    total_audible_ms      INTEGER NOT NULL DEFAULT 0,
    last_listened_at_ms   INTEGER,
    first_listened_at_ms  INTEGER,
    updated_at_ms         INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS habit_metrics (
    bucket              TEXT NOT NULL,
    bucket_start_ms     INTEGER NOT NULL,
    listening_minutes   REAL NOT NULL,
    unique_tracks       INTEGER NOT NULL,
    unique_artists      INTEGER NOT NULL,
    sessions            INTEGER NOT NULL,
    top_hour_of_day     INTEGER,
    exploration_ratio   REAL NOT NULL,
    repeat_ratio        REAL NOT NULL,
    computed_at_ms      INTEGER NOT NULL,
    PRIMARY KEY (bucket, bucket_start_ms)
);

CREATE TABLE IF NOT EXISTS qualification_rules (
    version       INTEGER PRIMARY KEY,
    description   TEXT NOT NULL,
    applied_at_ms INTEGER NOT NULL
);
INSERT OR IGNORE INTO qualification_rules (version, description, applied_at_ms)
VALUES (1,
        'audible_ms >= max(30s, min(50% of duration, 4min)) and duration_ms > 30s',
        CAST(strftime('%s','now') AS INTEGER) * 1000);

CREATE TABLE IF NOT EXISTS playback_progress (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id       TEXT NOT NULL,
    track_uri        TEXT NOT NULL,
    sampled_at_ms    INTEGER NOT NULL,
    position_ms      INTEGER NOT NULL,
    audible_samples  INTEGER NOT NULL,
    sample_rate      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_playback_progress_session_time
    ON playback_progress(session_id, sampled_at_ms);
CREATE INDEX IF NOT EXISTS idx_playback_progress_sampled
    ON playback_progress(sampled_at_ms);
"#;

/// Phase 12: operations log. Every mutating daemon request is recorded
/// here with its reversal plan and pre-state, so `spotuify ops undo`
/// (and MCP `undo_last`) can revert it safely. `operation_id` is a
/// UUID v7 (time-orderable string PK). `receipt_id` is the Phase 6.6
/// receipt FK; `subject_op_id` links an undo/redo row back to the
/// operation it acts on.
///
/// `reversible = 1` only for kinds with a meaningful inverse
/// (playlist_add, library_save, transfer, like, …). Transport kinds
/// (play/pause/seek/volume/shuffle/repeat) record `reversible = 0`
/// purely for the audit log.
const MIGRATION_005_OPERATIONS: &str = r#"
CREATE TABLE IF NOT EXISTS operations (
    operation_id        TEXT PRIMARY KEY,
    kind                TEXT NOT NULL,
    occurred_at_ms      INTEGER NOT NULL,
    finished_at_ms      INTEGER,
    source              TEXT NOT NULL,
    requester           TEXT,
    subject_uris_json   TEXT NOT NULL DEFAULT '[]',
    reversible          INTEGER NOT NULL DEFAULT 0,
    reversal_plan_json  TEXT,
    pre_state_json      TEXT,
    status              TEXT NOT NULL,
    receipt_id          TEXT REFERENCES receipts(receipt_id),
    subject_op_id       TEXT REFERENCES operations(operation_id),
    undone_by_op_id     TEXT REFERENCES operations(operation_id),
    redone_by_op_id     TEXT REFERENCES operations(operation_id),
    error_message       TEXT
);
CREATE INDEX IF NOT EXISTS idx_operations_status_started
    ON operations(status, occurred_at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_operations_source_started
    ON operations(source, occurred_at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_operations_subject_op
    ON operations(subject_op_id);
"#;

/// Phase 16: persistent lyrics cache and per-track manual timing offsets.
const MIGRATION_006_LYRICS: &str = r#"
CREATE TABLE IF NOT EXISTS lyrics_cache (
    track_uri     TEXT PRIMARY KEY,
    provider      TEXT NOT NULL,
    synced        INTEGER NOT NULL,
    lines_json    TEXT NOT NULL,
    fetched_at_ms INTEGER NOT NULL,
    language      TEXT,
    source_url    TEXT
);
CREATE INDEX IF NOT EXISTS idx_lyrics_cache_fetched
    ON lyrics_cache(fetched_at_ms DESC);

CREATE TABLE IF NOT EXISTS lyrics_offsets (
    track_uri     TEXT PRIMARY KEY,
    offset_ms     INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
"#;

/// Fast-cadence queue cache. A snapshot records the currently playing
/// item plus the ordered upcoming queue so `spotuify queue` can be
/// served locally and refreshed in the background.
const MIGRATION_010_QUEUE_CACHE: &str = r#"
CREATE TABLE IF NOT EXISTS queue_snapshots (
    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
    currently_playing_uri TEXT REFERENCES media_items(uri) ON DELETE SET NULL,
    fetched_at_ms         INTEGER NOT NULL,
    freshness_class       TEXT NOT NULL DEFAULT 'unknown',
    sync_generation       INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_queue_snapshots_time
    ON queue_snapshots(fetched_at_ms DESC);

CREATE TABLE IF NOT EXISTS queue_items (
    snapshot_id      INTEGER NOT NULL REFERENCES queue_snapshots(id) ON DELETE CASCADE,
    item_uri         TEXT NOT NULL REFERENCES media_items(uri) ON DELETE CASCADE,
    position         INTEGER NOT NULL,
    freshness_class  TEXT NOT NULL DEFAULT 'unknown',
    sync_generation  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (snapshot_id, position)
);
CREATE INDEX IF NOT EXISTS idx_queue_items_item
    ON queue_items(item_uri);
"#;

/// Migration table-of-contents. Append-only: never edit a published
/// migration's body or version. To change semantics, add a follow-up
/// migration that mutates the new schema.
///
/// `Sql` migrations must use `CREATE TABLE IF NOT EXISTS` / `INSERT OR
/// IGNORE` style idempotent statements: the schema_migrations stamp is
/// the last write, so a crash before stamping replays the body cleanly.
struct Migration {
    version: u32,
    name: &'static str,
    kind: MigrationKind,
}

#[allow(dead_code)]
enum MigrationKind {
    Sql(&'static str),
    AddColumns(&'static [ColumnMigration]),
    RebuildPlaylistItemsPositionPk,
}

struct ColumnMigration {
    table: &'static str,
    name: &'static str,
    definition: &'static str,
}

const MIGRATION_002_COLUMNS: &[ColumnMigration] = &[
    ColumnMigration {
        table: "playlists",
        name: "snapshot_id",
        definition: "snapshot_id TEXT",
    },
    ColumnMigration {
        table: "playlist_items",
        name: "snapshot_id_at_fetch",
        definition: "snapshot_id_at_fetch TEXT",
    },
    ColumnMigration {
        table: "media_items",
        name: "freshness_class",
        definition: "freshness_class TEXT NOT NULL DEFAULT 'unknown'",
    },
    ColumnMigration {
        table: "media_items",
        name: "sync_generation",
        definition: "sync_generation INTEGER NOT NULL DEFAULT 0",
    },
    ColumnMigration {
        table: "devices",
        name: "freshness_class",
        definition: "freshness_class TEXT NOT NULL DEFAULT 'unknown'",
    },
    ColumnMigration {
        table: "devices",
        name: "sync_generation",
        definition: "sync_generation INTEGER NOT NULL DEFAULT 0",
    },
    ColumnMigration {
        table: "playback_snapshots",
        name: "freshness_class",
        definition: "freshness_class TEXT NOT NULL DEFAULT 'unknown'",
    },
    ColumnMigration {
        table: "playback_snapshots",
        name: "sync_generation",
        definition: "sync_generation INTEGER NOT NULL DEFAULT 0",
    },
    ColumnMigration {
        table: "recent_items",
        name: "freshness_class",
        definition: "freshness_class TEXT NOT NULL DEFAULT 'unknown'",
    },
    ColumnMigration {
        table: "recent_items",
        name: "sync_generation",
        definition: "sync_generation INTEGER NOT NULL DEFAULT 0",
    },
    ColumnMigration {
        table: "library_items",
        name: "freshness_class",
        definition: "freshness_class TEXT NOT NULL DEFAULT 'unknown'",
    },
    ColumnMigration {
        table: "library_items",
        name: "sync_generation",
        definition: "sync_generation INTEGER NOT NULL DEFAULT 0",
    },
];

const MIGRATION_007_COLUMNS: &[ColumnMigration] = &[
    ColumnMigration {
        table: "playlists",
        name: "freshness_class",
        definition: "freshness_class TEXT NOT NULL DEFAULT 'unknown'",
    },
    ColumnMigration {
        table: "playlists",
        name: "sync_generation",
        definition: "sync_generation INTEGER NOT NULL DEFAULT 0",
    },
    ColumnMigration {
        table: "playlist_items",
        name: "freshness_class",
        definition: "freshness_class TEXT NOT NULL DEFAULT 'unknown'",
    },
    ColumnMigration {
        table: "playlist_items",
        name: "sync_generation",
        definition: "sync_generation INTEGER NOT NULL DEFAULT 0",
    },
];

const MIGRATION_008_COLUMNS: &[ColumnMigration] = &[ColumnMigration {
    table: "library_items",
    name: "sync_position",
    definition: "sync_position INTEGER NOT NULL DEFAULT 0",
}];

const MIGRATION_011_COLUMNS: &[ColumnMigration] = &[ColumnMigration {
    table: "playlists",
    name: "tracks_accessible",
    definition: "tracks_accessible INTEGER NOT NULL DEFAULT 1",
}];

/// Playback context (playlist/album/artist URI) the track was played
/// from, so analytics can do playlist-level top-k. Nullable: pre-17 rows
/// and plays with no context stay NULL.
const MIGRATION_017_COLUMNS: &[ColumnMigration] = &[ColumnMigration {
    table: "listen_facts",
    name: "context_uri",
    definition: "context_uri TEXT",
}];

const MIGRATION_013_COLUMNS: &[ColumnMigration] = &[
    ColumnMigration {
        table: "media_items",
        name: "album",
        definition: "album TEXT",
    },
    ColumnMigration {
        table: "media_items",
        name: "release_date",
        definition: "release_date TEXT",
    },
    ColumnMigration {
        table: "media_items",
        name: "resume_position_ms",
        definition: "resume_position_ms INTEGER",
    },
    ColumnMigration {
        table: "media_items",
        name: "fully_played",
        definition: "fully_played INTEGER",
    },
    ColumnMigration {
        table: "library_items",
        name: "added_at_ms",
        definition: "added_at_ms INTEGER",
    },
];

const MIGRATION_012_LYRICS_NEGATIVE_CACHE: &str = r#"
CREATE TABLE IF NOT EXISTS lyrics_lookup_failures (
    track_uri            TEXT PRIMARY KEY,
    failed_at_ms         INTEGER NOT NULL,
    unavailable_until_ms INTEGER NOT NULL,
    reason               TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_lyrics_lookup_failures_until
    ON lyrics_lookup_failures(unavailable_until_ms DESC);
"#;

const MIGRATION_015_COLUMNS: &[ColumnMigration] = &[ColumnMigration {
    table: "sync_events",
    name: "retry_after_secs",
    definition: "retry_after_secs INTEGER",
}];

const MIGRATION_016_COLUMNS: &[ColumnMigration] = &[
    ColumnMigration {
        table: "media_items",
        name: "album_uri",
        definition: "album_uri TEXT",
    },
    ColumnMigration {
        table: "media_items",
        name: "artists_json",
        definition: "artists_json TEXT",
    },
];

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial_cache",
        kind: MigrationKind::Sql(INITIAL_SCHEMA),
    },
    Migration {
        version: 2,
        name: "snapshot_id_freshness",
        kind: MigrationKind::AddColumns(MIGRATION_002_COLUMNS),
    },
    Migration {
        version: 3,
        name: "receipts",
        kind: MigrationKind::Sql(MIGRATION_003_RECEIPTS),
    },
    Migration {
        version: 4,
        name: "analytics_derivations",
        kind: MigrationKind::Sql(MIGRATION_004_ANALYTICS_DERIVATIONS),
    },
    Migration {
        version: 5,
        name: "operations",
        kind: MigrationKind::Sql(MIGRATION_005_OPERATIONS),
    },
    Migration {
        version: 6,
        name: "lyrics",
        kind: MigrationKind::Sql(MIGRATION_006_LYRICS),
    },
    Migration {
        version: 7,
        name: "playlist_freshness",
        kind: MigrationKind::AddColumns(MIGRATION_007_COLUMNS),
    },
    Migration {
        version: 8,
        name: "library_sync_position",
        kind: MigrationKind::AddColumns(MIGRATION_008_COLUMNS),
    },
    Migration {
        version: 9,
        name: "playlist_items_position_pk",
        kind: MigrationKind::RebuildPlaylistItemsPositionPk,
    },
    Migration {
        version: 10,
        name: "queue_cache",
        kind: MigrationKind::Sql(MIGRATION_010_QUEUE_CACHE),
    },
    Migration {
        version: 11,
        name: "playlist_tracks_accessibility",
        kind: MigrationKind::AddColumns(MIGRATION_011_COLUMNS),
    },
    Migration {
        version: 12,
        name: "lyrics_negative_cache",
        kind: MigrationKind::Sql(MIGRATION_012_LYRICS_NEGATIVE_CACHE),
    },
    Migration {
        version: 13,
        name: "media_enrichment",
        kind: MigrationKind::AddColumns(MIGRATION_013_COLUMNS),
    },
    Migration {
        version: 14,
        name: "reminders",
        kind: MigrationKind::Sql(MIGRATION_014_REMINDERS),
    },
    Migration {
        version: 15,
        name: "sync_events_retry_after",
        kind: MigrationKind::AddColumns(MIGRATION_015_COLUMNS),
    },
    Migration {
        version: 16,
        name: "media_artist_album_refs",
        kind: MigrationKind::AddColumns(MIGRATION_016_COLUMNS),
    },
    Migration {
        version: 17,
        name: "listen_facts_context_uri",
        kind: MigrationKind::AddColumns(MIGRATION_017_COLUMNS),
    },
    Migration {
        version: 18,
        name: "queue_add_not_reversible",
        kind: MigrationKind::Sql(MIGRATION_018_QUEUE_ADD_NOT_REVERSIBLE),
    },
];

/// queue_add ops were recorded with `reversible = 1` and a queue_remove
/// plan whose executor was a silent no-op (neither the Spotify Web API
/// nor librespot 0.8 exposes queue-remove). The kind is non-reversible
/// now; flip legacy rows so `ops undo` stops selecting them and then
/// claiming success while removing nothing.
const MIGRATION_018_QUEUE_ADD_NOT_REVERSIBLE: &str = r#"
UPDATE operations SET reversible = 0 WHERE kind = 'queue_add' AND reversible = 1;
"#;

/// Listening reminders: schedules + fired-occurrence notifications (inbox).
const MIGRATION_014_REMINDERS: &str = r#"
CREATE TABLE IF NOT EXISTS reminder_schedules (
    id              TEXT PRIMARY KEY,
    media_uri       TEXT NOT NULL,
    media_kind      TEXT NOT NULL,
    name            TEXT NOT NULL DEFAULT '',
    subtitle        TEXT NOT NULL DEFAULT '',
    image_url       TEXT,
    anchor_at_ms    INTEGER NOT NULL,
    recurrence      TEXT NOT NULL DEFAULT 'none',
    tz              TEXT NOT NULL DEFAULT 'UTC',
    next_due_at_ms  INTEGER NOT NULL,
    state           TEXT NOT NULL DEFAULT 'active',
    message         TEXT,
    created_at_ms   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_reminder_schedules_due
    ON reminder_schedules(state, next_due_at_ms);

CREATE TABLE IF NOT EXISTS reminder_notifications (
    id               TEXT PRIMARY KEY,
    reminder_id      TEXT NOT NULL,
    media_uri        TEXT NOT NULL,
    media_kind       TEXT NOT NULL,
    name             TEXT NOT NULL DEFAULT '',
    subtitle         TEXT NOT NULL DEFAULT '',
    image_url        TEXT,
    due_at_ms        INTEGER NOT NULL,
    fired_at_ms      INTEGER NOT NULL,
    state            TEXT NOT NULL DEFAULT 'unseen',
    snoozed_until_ms INTEGER,
    acted            TEXT,
    message          TEXT,
    created_at_ms    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_reminder_notifications_state_due
    ON reminder_notifications(state, due_at_ms DESC);
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
    (
        "queue_snapshots",
        &[
            "id",
            "currently_playing_uri",
            "fetched_at_ms",
            "freshness_class",
            "sync_generation",
        ],
    ),
    (
        "queue_items",
        &[
            "snapshot_id",
            "item_uri",
            "position",
            "freshness_class",
            "sync_generation",
        ],
    ),
    (
        "playlists",
        &[
            "id",
            "name",
            "owner",
            "tracks_total",
            "freshness_class",
            "sync_generation",
        ],
    ),
    (
        "playlist_items",
        &[
            "playlist_id",
            "item_uri",
            "position",
            "freshness_class",
            "sync_generation",
        ],
    ),
    (
        "recent_items",
        &["item_uri", "played_at_ms", "fetched_at_ms"],
    ),
    (
        "library_items",
        &["item_uri", "kind", "saved", "followed", "sync_position"],
    ),
    (
        "search_runs",
        &["query", "normalized_query", "scope", "source"],
    ),
    ("search_results", &["search_run_id", "position", "item_uri"]),
    (
        "sync_events",
        &[
            "domain",
            "finished_at_ms",
            "status",
            "row_count",
            "retry_after_secs",
        ],
    ),
    (
        "sync_cursors",
        &["domain", "last_success_at_ms", "last_error"],
    ),
    // v4 — analytics derivations
    (
        "listen_facts",
        &[
            "session_id",
            "track_uri",
            "started_at_ms",
            "audible_ms",
            "qualified",
            "qualification_rule_version",
        ],
    ),
    ("track_metrics", &["track_uri", "qualified_count"]),
    ("artist_metrics", &["artist_uri", "qualified_count"]),
    ("album_metrics", &["album_uri", "qualified_count"]),
    (
        "habit_metrics",
        &["bucket", "bucket_start_ms", "listening_minutes"],
    ),
    ("qualification_rules", &["version", "description"]),
    (
        "playback_progress",
        &[
            "session_id",
            "track_uri",
            "sampled_at_ms",
            "audible_samples",
            "sample_rate",
        ],
    ),
    // v5 — operations log
    (
        "operations",
        &["operation_id", "kind", "occurred_at_ms", "source", "status"],
    ),
    // v6 — lyrics cache
    (
        "lyrics_cache",
        &[
            "track_uri",
            "provider",
            "synced",
            "lines_json",
            "fetched_at_ms",
        ],
    ),
    (
        "lyrics_offsets",
        &["track_uri", "offset_ms", "updated_at_ms"],
    ),
    (
        "lyrics_lookup_failures",
        &[
            "track_uri",
            "failed_at_ms",
            "unavailable_until_ms",
            "reason",
        ],
    ),
];

#[cfg(test)]
mod tests {
    #![allow(clippy::panic, clippy::unwrap_used)]

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
        assert_eq!(status.freshness.media_items.fresh, 1);
        assert_eq!(status.freshness.media_items.unknown, 0);
        assert!(status.freshness.media_items.max_sync_generation > 0);
    }

    #[tokio::test]
    async fn latest_playback_ignores_empty_snapshots_for_recent_fallback() {
        let store = Store::in_memory().await.unwrap();
        let item = track("spotify:track:1", "Sweet Thing", "Chaka Khan");
        store
            .persist_recent_items(std::slice::from_ref(&item))
            .await
            .unwrap();
        store.persist_playback(&Playback::default()).await.unwrap();

        assert!(store.latest_playback().await.unwrap().is_none());
        let playback = store
            .latest_playback_or_recent()
            .await
            .unwrap()
            .expect("recent fallback");
        assert_eq!(
            playback.item.as_ref().map(|item| item.uri.as_str()),
            Some("spotify:track:1")
        );
        assert!(!playback.is_playing);
    }

    #[tokio::test]
    async fn cache_writes_mark_user_facing_cache_tables_fresh() {
        let store = Store::in_memory()
            .await
            .expect("in-memory store should open");
        let item = track("spotify:track:1", "Sweet Thing", "Chaka Khan");
        let device = Device {
            id: Some("device-1".to_string()),
            name: "spotuify-test".to_string(),
            kind: "computer".to_string(),
            is_active: true,
            is_restricted: false,
            volume_percent: Some(50),
            supports_volume: true,
        };
        let playback = Playback {
            item: Some(item.clone()),
            device: Some(device.clone()),
            is_playing: true,
            progress_ms: 1_000,
            shuffle: false,
            repeat: "off".to_string(),
            ..Default::default()
        };
        let playlist = Playlist {
            id: "playlist-1".to_string(),
            name: "Favorites".to_string(),
            owner: "me".to_string(),
            tracks_total: 1,
            image_url: None,
            snapshot_id: Some("snapshot-1".to_string()),
        };

        store
            .persist_devices(&[device])
            .await
            .expect("devices should persist");
        store
            .persist_playback(&playback)
            .await
            .expect("playback should persist");
        store
            .persist_queue(&Queue {
                currently_playing: Some(item.clone()),
                items: vec![item.clone()],
                ..Default::default()
            })
            .await
            .expect("queue should persist");
        store
            .persist_playlists(&[playlist])
            .await
            .expect("playlists should persist");
        store
            .persist_playlist_items("playlist-1", std::slice::from_ref(&item))
            .await
            .expect("playlist items should persist");
        store
            .persist_recent_items(std::slice::from_ref(&item))
            .await
            .expect("recent items should persist");
        store
            .persist_library_items(std::slice::from_ref(&item))
            .await
            .expect("library items should persist");

        let status = store
            .cache_status(0)
            .await
            .expect("cache status should load");

        assert_eq!(status.freshness.devices.fresh, 1);
        assert_eq!(status.freshness.playback_snapshots.fresh, 1);
        assert_eq!(status.freshness.queue_snapshots.fresh, 1);
        assert_eq!(status.freshness.queue_items.fresh, 1);
        assert_eq!(status.freshness.playlists.fresh, 1);
        assert_eq!(status.freshness.playlist_items.fresh, 1);
        assert_eq!(status.freshness.recent_items.fresh, 1);
        assert_eq!(status.freshness.library_items.fresh, 1);
        assert_eq!(status.freshness.playlists.unknown, 0);
        assert!(status.freshness.playlist_items.max_sync_generation > 0);
    }

    #[tokio::test]
    async fn queue_cache_preserves_duplicate_upcoming_items() {
        let store = Store::in_memory()
            .await
            .expect("in-memory store should open");
        let current = track("spotify:track:current", "Now", "Playing");
        let queued = track("spotify:track:queued", "Next", "Again");
        store
            .persist_queue(&Queue {
                currently_playing: Some(current.clone()),
                items: vec![queued.clone(), queued.clone()],
                ..Default::default()
            })
            .await
            .expect("queue should persist");

        let queue = store
            .latest_queue(10)
            .await
            .expect("queue should read")
            .expect("queue should exist");
        let status = store.cache_status(0).await.expect("cache status");

        assert_eq!(
            queue
                .currently_playing
                .as_ref()
                .map(|item| item.uri.as_str()),
            Some("spotify:track:current")
        );
        assert_eq!(
            queue.items.len(),
            2,
            "Spotify queues can contain duplicate upcoming items"
        );
        assert_eq!(queue.items[0].uri, "spotify:track:queued");
        assert_eq!(queue.items[1].uri, "spotify:track:queued");
        assert_eq!(status.queue_snapshots, 1);
        assert_eq!(status.queue_items, 2);
    }

    #[tokio::test]
    async fn latest_queue_prefers_meaningful_snapshot_over_empty_one() {
        // Regression test for the "queue is empty on cold start" bug:
        // pre-fix daemons persisted an empty queue snapshot every 3s
        // during idle periods. The naive "latest by fetched_at_ms"
        // read would always hand back one of those, hiding the actual
        // last-known queue from the previous live session. Confirm
        // the filter promotes the meaningful row.
        let store = Store::in_memory()
            .await
            .expect("in-memory store should open");
        let current = track("spotify:track:current", "Now", "Playing");
        let queued = track("spotify:track:queued", "Next", "Again");
        // Step 1 — live session: persist a meaningful snapshot.
        store
            .persist_queue(&Queue {
                currently_playing: Some(current.clone()),
                items: vec![queued.clone()],
                ..Default::default()
            })
            .await
            .expect("meaningful queue should persist");
        // Step 2 — simulate idle daemon churn by writing two empty
        // snapshots AFTER the meaningful one. Their `fetched_at_ms`
        // beats the meaningful row's, so a naive ORDER BY would pick
        // them first.
        for _ in 0..2 {
            // Tiny pause to advance now_ms(); SQLite is fast enough
            // that two writes in the same ms can land back-to-back.
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            store
                .persist_queue(&Queue::default())
                .await
                .expect("empty snapshot should persist");
        }

        let queue = store
            .latest_queue(10)
            .await
            .expect("queue read")
            .expect("queue should exist");
        assert_eq!(
            queue.currently_playing.as_ref().map(|i| i.uri.as_str()),
            Some("spotify:track:current"),
            "latest_queue must skip past empty snapshots and surface the last meaningful row \
             (so users see their previous session's queue, not a misleading empty list)"
        );
        assert_eq!(queue.items.len(), 1);
        assert!(
            !queue.session_active,
            "cache reads always set session_active=false; \
             the sync layer flips it true only after a live probe"
        );
        assert!(
            queue.as_of_ms > 0,
            "latest_queue must surface the snapshot's fetched_at_ms so clients can render \
             a 'from last session' badge / time hint"
        );
    }

    #[tokio::test]
    async fn playlist_items_preserve_duplicate_tracks_by_position() {
        let store = Store::in_memory()
            .await
            .expect("in-memory store should open");
        let item = track("spotify:track:1", "Sweet Thing", "Chaka Khan");
        let playlist = Playlist {
            id: "playlist-duplicates".to_string(),
            name: "Duplicates".to_string(),
            owner: "me".to_string(),
            tracks_total: 2,
            image_url: None,
            snapshot_id: Some("snapshot-dup".to_string()),
        };
        store
            .persist_playlists(&[playlist])
            .await
            .expect("playlist should persist");
        store
            .persist_playlist_items("playlist-duplicates", &[item.clone(), item.clone()])
            .await
            .expect("playlist items should persist");

        let items = store
            .playlist_items("playlist-duplicates", 10)
            .await
            .expect("playlist items should read");
        let status = store.cache_status(0).await.expect("cache status");

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].uri, "spotify:track:1");
        assert_eq!(items[1].uri, "spotify:track:1");
        assert_eq!(status.playlist_items, 2);
    }

    #[tokio::test]
    async fn inaccessible_playlist_tracks_are_hidden_until_snapshot_changes() {
        let store = Store::in_memory()
            .await
            .expect("in-memory store should open");
        let playlist = Playlist {
            id: "playlist-locked".to_string(),
            name: "Locked".to_string(),
            owner: "other".to_string(),
            tracks_total: 2,
            image_url: None,
            snapshot_id: Some("snapshot-a".to_string()),
        };
        store
            .persist_playlists(std::slice::from_ref(&playlist))
            .await
            .expect("playlist should persist");
        store
            .mark_playlist_tracks_inaccessible(&playlist.id)
            .await
            .expect("playlist should be markable");

        assert!(!store
            .playlist_tracks_accessible(&playlist.id)
            .await
            .expect("access flag should read"));
        assert!(store
            .list_playlists(10)
            .await
            .expect("playlists should read")
            .is_empty());

        store
            .persist_playlists(std::slice::from_ref(&playlist))
            .await
            .expect("same snapshot should persist");
        assert!(store
            .list_playlists(10)
            .await
            .expect("playlists should read")
            .is_empty());

        let changed = Playlist {
            snapshot_id: Some("snapshot-b".to_string()),
            ..playlist
        };
        store
            .persist_playlists(&[changed])
            .await
            .expect("changed snapshot should persist");
        assert_eq!(
            store
                .list_playlists(10)
                .await
                .expect("playlists should read")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn saved_tracks_fingerprint_preserves_sync_order() {
        let store = Store::in_memory()
            .await
            .expect("in-memory store should open");
        let items = vec![
            track("spotify:track:1", "Sweet Thing", "Chaka Khan"),
            track("spotify:track:2", "Never Too Much", "Luther Vandross"),
        ];
        store
            .persist_library_items(&items)
            .await
            .expect("library items should persist");

        let (total, ids) = store
            .saved_tracks_fingerprint(50)
            .await
            .expect("fingerprint should load");

        assert_eq!(total, 2);
        assert_eq!(ids, ["1", "2"]);
    }

    /// Locks the prune contract: `replace_devices` should mirror the
    /// just-landed batch exactly, dropping rows from prior batches.
    /// This is what makes Spotify's eventual auto-expiry of stale
    /// Connect devices propagate into the spotuify cache (the user's
    /// 7 ghost "spotuify" entries disappear once Spotify drops them
    /// from `/v1/me/player/devices` and we refresh).
    #[tokio::test]
    async fn replace_devices_prunes_rows_missing_from_latest_refresh() {
        let store = Store::in_memory()
            .await
            .expect("in-memory store should open");
        fn make_device(id: &str, name: &str) -> Device {
            Device {
                id: Some(id.to_string()),
                name: name.to_string(),
                kind: "computer".to_string(),
                is_active: false,
                is_restricted: false,
                volume_percent: Some(50),
                supports_volume: true,
            }
        }

        // First refresh: three devices (think 3 stale "spotuify" + 1 phone).
        let batch_a = vec![
            make_device("stale-1", "spotuify"),
            make_device("stale-2", "spotuify"),
            make_device("phone-1", "iPhone"),
        ];
        store.replace_devices(&batch_a).await.unwrap();
        assert_eq!(store.list_devices().await.unwrap().len(), 3);

        // Wait long enough that the second refresh's sync_generation
        // is strictly greater (sync_generation is millisecond-resolution).
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        // Second refresh: Spotify has dropped one stale and the
        // phone has gone idle (still present); the live device id
        // is now `live-id`.
        let batch_b = vec![
            make_device("live-id", "spotuify"),
            make_device("phone-1", "iPhone"),
        ];
        store.replace_devices(&batch_b).await.unwrap();
        let after = store.list_devices().await.unwrap();
        let ids: Vec<&str> = after.iter().filter_map(|d| d.id.as_deref()).collect();
        assert_eq!(after.len(), 2, "stale rows must be pruned");
        assert!(ids.contains(&"live-id"));
        assert!(ids.contains(&"phone-1"));
        assert!(!ids.contains(&"stale-1"));
        assert!(!ids.contains(&"stale-2"));
    }

    /// `replace_devices` with an empty batch clears the cache —
    /// Spotify reporting zero devices means "user unplugged
    /// everything"; the cache should reflect that.
    #[tokio::test]
    async fn replace_devices_with_empty_batch_clears_cache() {
        let store = Store::in_memory()
            .await
            .expect("in-memory store should open");
        store
            .replace_devices(&[Device {
                id: Some("d1".to_string()),
                name: "a".to_string(),
                kind: "computer".to_string(),
                is_active: false,
                is_restricted: false,
                volume_percent: None,
                supports_volume: false,
            }])
            .await
            .unwrap();
        assert_eq!(store.list_devices().await.unwrap().len(), 1);

        store.replace_devices(&[]).await.unwrap();
        assert_eq!(store.list_devices().await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn lyrics_cache_round_trips_lines_and_offset() {
        let store = Store::in_memory().await.unwrap();
        let lyrics = SyncedLyrics {
            provider: LyricsProvider::Lrclib,
            track_uri: "spotify:track:lyrics".to_string(),
            lines: vec![LyricLine {
                start_ms: 1_000,
                text: "hello".to_string(),
                is_rtl: false,
            }],
            fetched_at_ms: now_ms(),
            synced: true,
            language: Some("en".to_string()),
            source_url: Some("https://lrclib.net".to_string()),
        };

        store.upsert_lyrics(&lyrics).await.unwrap();
        store
            .set_lyrics_offset_ms("spotify:track:lyrics", 125)
            .await
            .unwrap();

        let cached = store
            .cached_lyrics("spotify:track:lyrics", Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cached.provider, LyricsProvider::Lrclib);
        assert_eq!(cached.lines[0].text, "hello");
        assert_eq!(
            store
                .lyrics_offset_ms("spotify:track:lyrics")
                .await
                .unwrap(),
            125
        );
        let status = store.cache_status(0).await.unwrap();
        assert_eq!(status.lyrics_cache, 1);
        assert_eq!(status.lyrics_offsets, 1);
    }

    #[tokio::test]
    async fn lyrics_lookup_failure_blocks_until_cleared_by_success() {
        let store = Store::in_memory().await.unwrap();
        store
            .upsert_lyrics_lookup_failure(
                "spotify:track:missing",
                "not found",
                Duration::from_secs(60),
            )
            .await
            .unwrap();
        assert!(store
            .lyrics_lookup_blocked("spotify:track:missing")
            .await
            .unwrap());

        let lyrics = SyncedLyrics {
            provider: LyricsProvider::Lrclib,
            track_uri: "spotify:track:missing".to_string(),
            lines: vec![LyricLine {
                start_ms: 0,
                text: "found".to_string(),
                is_rtl: false,
            }],
            fetched_at_ms: now_ms(),
            synced: true,
            language: None,
            source_url: None,
        };
        store.upsert_lyrics(&lyrics).await.unwrap();
        assert!(!store
            .lyrics_lookup_blocked("spotify:track:missing")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn rate_limit_cooldown_uses_latest_retry_after_error() {
        let store = Store::in_memory().await.unwrap();
        let started_at_ms = now_ms();

        store
            .record_sync_event_with_retry_after(
                "recent",
                started_at_ms,
                "error",
                0,
                Some("Spotify GET /me/player/recently-played was rate limited"),
                Some(60),
            )
            .await
            .unwrap();

        assert!(store
            .rate_limit_cooldown_remaining_ms("recent")
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn rate_limit_cooldown_keeps_legacy_retry_after_text_fallback() {
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
            ..Default::default()
        }
    }

    async fn insert_listen_fact(store: &Store, session: &str, uri: &str, at: i64) {
        sqlx::query(
            "INSERT INTO listen_facts
                (session_id, track_uri, started_at_ms, ended_at_ms, duration_ms,
                 elapsed_ms, audible_ms, completion_ratio, qualified,
                 qualification_rule_version, created_at_ms)
             VALUES (?, ?, ?, ?, 0, 0, 0, 0.0, 1, 1, ?)",
        )
        .bind(session)
        .bind(uri)
        .bind(at)
        .bind(at)
        .bind(at)
        .execute(store.writer_for_test())
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn listen_sessions_split_on_gap_and_resolve_tracks() {
        let store = Store::in_memory().await.unwrap();
        let items = vec![
            track("spotify:track:a", "A", "Artist"),
            track("spotify:track:b", "B", "Artist"),
            track("spotify:track:c", "C", "Artist"),
        ];
        store.upsert_media_items(&items, "spotify").await.unwrap();
        let base = 1_700_000_000_000i64;
        // a & b one minute apart → one session; c thirty minutes later → a new one.
        insert_listen_fact(&store, "s1", "spotify:track:a", base).await;
        insert_listen_fact(&store, "s1", "spotify:track:b", base + 60_000).await;
        insert_listen_fact(&store, "s2", "spotify:track:c", base + 30 * 60_000).await;

        let sessions = store.list_listen_sessions(10).await.unwrap();

        assert_eq!(
            sessions.len(),
            2,
            "a 30-min gap should split into two sessions"
        );
        // Newest-first: the lone later play, then the earlier pair.
        assert_eq!(sessions[0].track_count, 1);
        assert_eq!(sessions[0].tracks[0].uri, "spotify:track:c");
        assert_eq!(sessions[1].track_count, 2);
    }

    #[tokio::test]
    async fn listen_sessions_dedup_same_track_across_sources() {
        let store = Store::in_memory().await.unwrap();
        let items = vec![track("spotify:track:a", "A", "Artist")];
        store.upsert_media_items(&items, "spotify").await.unwrap();
        // Same track logged locally and in recent_items at ~the same moment
        // (persist_recent_items stamps `now`, so the local fact must too) so the
        // two land within the dedup tolerance.
        insert_listen_fact(&store, "s1", "spotify:track:a", now_ms()).await;
        store
            .persist_recent_items(std::slice::from_ref(&items[0]))
            .await
            .unwrap();

        let sessions = store.list_listen_sessions(10).await.unwrap();
        let total: u32 = sessions.iter().map(|s| s.track_count).sum();
        assert_eq!(
            total, 1,
            "near-simultaneous duplicate plays collapse to one"
        );
    }

    #[tokio::test]
    async fn set_artist_followed_toggles_followed_flag() {
        let store = Store::in_memory().await.unwrap();
        let artist = MediaItem {
            uri: "spotify:artist:x".to_string(),
            name: "X".to_string(),
            kind: MediaKind::Artist,
            ..Default::default()
        };
        store
            .persist_followed_artists(std::slice::from_ref(&artist))
            .await
            .unwrap();
        assert_eq!(store.list_followed_artists(10).await.unwrap().len(), 1);

        store
            .set_artist_followed("spotify:artist:x", false)
            .await
            .unwrap();
        assert!(store.list_followed_artists(10).await.unwrap().is_empty());
    }

    #[test]
    fn dominant_context_picks_most_common_album() {
        let mk = |album: &str| MediaItem {
            album: Some(album.to_string()),
            ..Default::default()
        };
        let items = vec![mk("Album A"), mk("Album A"), mk("Album B")];
        assert_eq!(dominant_context(&items).as_deref(), Some("Album A"));
        assert_eq!(dominant_context(&[]), None);
    }
}
