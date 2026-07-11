#![allow(clippy::panic, clippy::unwrap_used)]

//! Schema migration + cache_version gate tests.
//!
//! Adversarial coverage:
//! - v1 → vN migrations are idempotent (running twice is a no-op).
//! - Each migration adds the columns/tables its phase needs.
//! - Running against a future-version store (forward-incompat) is
//!   detected and refused rather than silently corrupting data.
//! - check_cache_version() reports the right state for tooling.

use spotuify_store::{Store, CACHE_VERSION};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::Row;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

async fn fresh_store() -> Store {
    Store::in_memory().await.expect("in_memory store")
}

fn temp_store_root(name: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "spotuify-store-{name}-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("temp store root");
    root
}

async fn table_exists(store: &Store, table: &str) -> bool {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT name FROM sqlite_master WHERE type='table' AND name = ?")
            .bind(table)
            .fetch_optional(store.reader())
            .await
            .unwrap();
    row.is_some()
}

async fn index_exists(store: &Store, index: &str) -> bool {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT name FROM sqlite_master WHERE type='index' AND name = ?")
            .bind(index)
            .fetch_optional(store.reader())
            .await
            .unwrap();
    row.is_some()
}

async fn column_exists(store: &Store, table: &str, column: &str) -> bool {
    let rows = sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(store.reader())
        .await
        .unwrap();
    rows.iter()
        .any(|row| row.get::<String, _>("name") == column)
}

async fn column_default(store: &Store, table: &str, column: &str) -> Option<String> {
    let rows = sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(store.reader())
        .await
        .unwrap();
    rows.into_iter()
        .find(|row| row.get::<String, _>("name") == column)
        .and_then(|row| row.try_get::<String, _>("dflt_value").ok())
}

#[tokio::test]
async fn test_open_refuses_future_schema_before_running_current_migrations() {
    let root = temp_store_root("future-schema-preflight");
    let db_path = root.join("cache.sqlite");
    let index_path = root.join("index");
    let db_url = format!("sqlite:{}", db_path.display());
    let opts = SqliteConnectOptions::from_str(&db_url)
        .unwrap()
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE schema_migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at_ms INTEGER NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (99, 'future', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let err = match Store::open(&db_path, &index_path).await {
        Ok(_) => panic!("future schema should be refused before migrations run"),
        Err(err) => err,
    };
    assert!(
        err.to_string().contains("99"),
        "error should name the future schema version: {err}"
    );

    let opts = SqliteConnectOptions::from_str(&db_url)
        .unwrap()
        .read_only(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap();
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM schema_migrations WHERE version <= ?")
        .bind(CACHE_VERSION as i64)
        .fetch_one(&pool)
        .await
        .unwrap();
    pool.close().await;
    assert_eq!(count.0, 0, "old binary must not stamp current migrations");

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn test_cache_version_matches_applied_migrations() {
    let store = fresh_store().await;
    let (count, max_version): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), COALESCE(MAX(version), 0) FROM schema_migrations")
            .fetch_one(store.reader())
            .await
            .unwrap();
    assert_eq!(count, CACHE_VERSION as i64);
    assert_eq!(max_version, CACHE_VERSION as i64);
}

#[tokio::test]
async fn test_v13_adds_media_enrichment_columns() {
    let store = fresh_store().await;
    for col in [
        "album",
        "release_date",
        "resume_position_ms",
        "fully_played",
    ] {
        assert!(
            column_exists(&store, "media_items", col).await,
            "media_items.{col} must exist after v13"
        );
    }
    assert!(
        column_exists(&store, "library_items", "added_at_ms").await,
        "library_items.added_at_ms must exist after v13"
    );
}

#[tokio::test]
async fn test_v14_creates_reminder_tables() {
    let store = fresh_store().await;
    assert!(table_exists(&store, "reminder_schedules").await);
    assert!(table_exists(&store, "reminder_notifications").await);
    for col in ["media_uri", "recurrence", "tz", "next_due_at_ms", "state"] {
        assert!(
            column_exists(&store, "reminder_schedules", col).await,
            "reminder_schedules.{col} must exist"
        );
    }
    for col in [
        "reminder_id",
        "due_at_ms",
        "fired_at_ms",
        "state",
        "snoozed_until_ms",
    ] {
        assert!(
            column_exists(&store, "reminder_notifications", col).await,
            "reminder_notifications.{col} must exist"
        );
    }
}

// --- v4 analytics derivations (Phase 10) ---

#[tokio::test]
async fn test_v4_creates_listen_facts_table() {
    let store = fresh_store().await;
    assert!(
        table_exists(&store, "listen_facts").await,
        "listen_facts table must exist"
    );
    for col in [
        "id",
        "session_id",
        "track_uri",
        "started_at_ms",
        "ended_at_ms",
        "duration_ms",
        "elapsed_ms",
        "audible_ms",
        "completion_ratio",
        "qualified",
        "qualification_rule_version",
        "skip_reason",
        "source",
        "backend",
        "private_session",
        "created_at_ms",
        "measurement_kind",
        "external_scrobble_id",
    ] {
        assert!(
            column_exists(&store, "listen_facts", col).await,
            "listen_facts.{col} must exist"
        );
    }
    assert!(index_exists(&store, "idx_listen_facts_started").await);
    assert!(index_exists(&store, "idx_listen_facts_track_qual").await);
    assert!(index_exists(&store, "idx_listen_facts_artist_qual").await);
    assert!(index_exists(&store, "idx_listen_facts_session").await);
}

#[tokio::test]
async fn test_v4_creates_track_metrics_table() {
    let store = fresh_store().await;
    assert!(table_exists(&store, "track_metrics").await);
    for col in [
        "track_uri",
        "qualified_count",
        "skip_count",
        "total_audible_ms",
        "last_listened_at_ms",
        "first_listened_at_ms",
        "updated_at_ms",
    ] {
        assert!(column_exists(&store, "track_metrics", col).await);
    }
}

#[tokio::test]
async fn test_v4_creates_artist_metrics_table() {
    let store = fresh_store().await;
    assert!(table_exists(&store, "artist_metrics").await);
    assert!(column_exists(&store, "artist_metrics", "artist_uri").await);
    assert!(column_exists(&store, "artist_metrics", "qualified_count").await);
}

#[tokio::test]
async fn test_v4_creates_album_metrics_table() {
    let store = fresh_store().await;
    assert!(table_exists(&store, "album_metrics").await);
    assert!(column_exists(&store, "album_metrics", "album_uri").await);
    assert!(column_exists(&store, "album_metrics", "qualified_count").await);
}

#[tokio::test]
async fn test_v4_creates_habit_metrics_table() {
    let store = fresh_store().await;
    assert!(table_exists(&store, "habit_metrics").await);
    for col in [
        "bucket",
        "bucket_start_ms",
        "listening_minutes",
        "unique_tracks",
        "unique_artists",
        "sessions",
        "top_hour_of_day",
        "exploration_ratio",
        "repeat_ratio",
        "computed_at_ms",
    ] {
        assert!(column_exists(&store, "habit_metrics", col).await);
    }
}

#[tokio::test]
async fn test_v4_creates_qualification_rules_seeded() {
    let store = fresh_store().await;
    assert!(table_exists(&store, "qualification_rules").await);
    let row: Option<(i64, String)> =
        sqlx::query_as("SELECT version, description FROM qualification_rules WHERE version = 1")
            .fetch_optional(store.reader())
            .await
            .unwrap();
    let row = row.expect("qualification_rules v1 must be seeded by migration v4");
    assert_eq!(row.0, 1);
    assert!(
        row.1.contains("30s") || row.1.contains("30 seconds"),
        "qualification rule v1 description must mention 30s minimum, got: {}",
        row.1
    );
}

#[tokio::test]
async fn test_v4_creates_playback_progress_table() {
    let store = fresh_store().await;
    assert!(table_exists(&store, "playback_progress").await);
    for col in [
        "id",
        "session_id",
        "track_uri",
        "sampled_at_ms",
        "position_ms",
        "audible_samples",
        "sample_rate",
        "channels",
    ] {
        assert!(column_exists(&store, "playback_progress", col).await);
    }
    assert!(index_exists(&store, "idx_playback_progress_session_time").await);
    assert!(index_exists(&store, "idx_playback_progress_sampled").await);
}

// --- v5 operations log (Phase 12) ---

#[tokio::test]
async fn test_v5_creates_operations_table() {
    let store = fresh_store().await;
    assert!(
        table_exists(&store, "operations").await,
        "operations table must exist"
    );
    for col in [
        "operation_id",
        "kind",
        "occurred_at_ms",
        "finished_at_ms",
        "source",
        "requester",
        "subject_uris_json",
        "reversible",
        "reversal_plan_json",
        "pre_state_json",
        "status",
        "receipt_id",
        "subject_op_id",
        "undone_by_op_id",
        "redone_by_op_id",
        "error_message",
    ] {
        assert!(
            column_exists(&store, "operations", col).await,
            "operations.{col} must exist"
        );
    }
}

#[tokio::test]
async fn test_v5_creates_operations_indexes() {
    let store = fresh_store().await;
    assert!(index_exists(&store, "idx_operations_status_started").await);
    assert!(index_exists(&store, "idx_operations_source_started").await);
    assert!(index_exists(&store, "idx_operations_subject_op").await);
}

#[tokio::test]
async fn test_v5_subject_uris_json_defaults_to_empty_array() {
    let store = fresh_store().await;
    let default = column_default(&store, "operations", "subject_uris_json").await;
    let trimmed = default.as_deref().map(|s| s.trim_matches('\''));
    assert_eq!(trimmed, Some("[]"));
}

#[tokio::test]
async fn test_v5_reversible_defaults_to_zero() {
    let store = fresh_store().await;
    let default = column_default(&store, "operations", "reversible").await;
    assert_eq!(default.as_deref(), Some("0"));
}

#[tokio::test]
async fn test_v5_migration_is_idempotent() {
    let store = fresh_store().await;
    let ops_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM operations")
        .fetch_one(store.reader())
        .await
        .unwrap();
    store.run_migrations_idempotent_for_test().await.unwrap();
    let ops_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM operations")
        .fetch_one(store.reader())
        .await
        .unwrap();
    assert_eq!(ops_before, ops_after);
}

// --- v6 lyrics cache (Phase 16) ---

#[tokio::test]
async fn test_v6_creates_lyrics_cache_table() {
    let store = fresh_store().await;
    assert!(table_exists(&store, "lyrics_cache").await);
    for col in [
        "track_uri",
        "provider",
        "synced",
        "lines_json",
        "fetched_at_ms",
        "language",
        "source_url",
    ] {
        assert!(
            column_exists(&store, "lyrics_cache", col).await,
            "lyrics_cache.{col} must exist"
        );
    }
    assert!(index_exists(&store, "idx_lyrics_cache_fetched").await);
}

#[tokio::test]
async fn test_v6_creates_lyrics_offsets_table() {
    let store = fresh_store().await;
    assert!(table_exists(&store, "lyrics_offsets").await);
    for col in ["track_uri", "offset_ms", "updated_at_ms"] {
        assert!(
            column_exists(&store, "lyrics_offsets", col).await,
            "lyrics_offsets.{col} must exist"
        );
    }
}

#[tokio::test]
async fn test_v6_migration_is_idempotent() {
    let store = fresh_store().await;
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lyrics_cache")
        .fetch_one(store.reader())
        .await
        .unwrap();
    store.run_migrations_idempotent_for_test().await.unwrap();
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lyrics_cache")
        .fetch_one(store.reader())
        .await
        .unwrap();
    assert_eq!(before, after);
}

// --- v4 idempotency (lives after the v5 group so the helpers above are colocated) ---

#[tokio::test]
async fn test_v4_migration_is_idempotent() {
    let store = fresh_store().await;
    // Running migrations again must not error and must leave row counts unchanged.
    let listen_facts_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM listen_facts")
        .fetch_one(store.reader())
        .await
        .unwrap();
    let qual_rules_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM qualification_rules")
        .fetch_one(store.reader())
        .await
        .unwrap();
    store.run_migrations_idempotent_for_test().await.unwrap();
    let listen_facts_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM listen_facts")
        .fetch_one(store.reader())
        .await
        .unwrap();
    let qual_rules_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM qualification_rules")
        .fetch_one(store.reader())
        .await
        .unwrap();
    assert_eq!(listen_facts_before, listen_facts_after);
    // qualification_rules seed must NOT double-insert.
    assert_eq!(qual_rules_before, qual_rules_after);
    assert_eq!(qual_rules_after, 1, "exactly one seeded rule expected");
}

#[tokio::test]
async fn test_v1_to_v2_migration_is_idempotent() {
    // Building a fresh in-memory store runs both migrations.
    // Running migrations again must not error and must leave row counts
    // unchanged.
    let store = fresh_store().await;
    let before = store.cache_status(0).await.unwrap();
    store.run_migrations_idempotent_for_test().await.unwrap();
    let after = store.cache_status(0).await.unwrap();
    assert_eq!(before.media_items, after.media_items);
    assert_eq!(before.playlists, after.playlists);
}

#[tokio::test]
async fn test_v2_playlists_has_snapshot_id_column() {
    let store = fresh_store().await;
    assert!(
        column_exists(&store, "playlists", "snapshot_id").await,
        "v2 must add playlists.snapshot_id"
    );
}

#[tokio::test]
async fn test_v2_playlist_items_has_snapshot_id_at_fetch_column() {
    let store = fresh_store().await;
    assert!(
        column_exists(&store, "playlist_items", "snapshot_id_at_fetch").await,
        "v2 must add playlist_items.snapshot_id_at_fetch"
    );
}

#[tokio::test]
async fn test_v2_media_items_has_freshness_class_default_unknown() {
    let store = fresh_store().await;
    assert!(
        column_exists(&store, "media_items", "freshness_class").await,
        "v2 must add media_items.freshness_class"
    );
    let default = column_default(&store, "media_items", "freshness_class").await;
    assert!(
        default
            .as_deref()
            .map(str::trim)
            .map(|d| d.trim_matches('\''))
            == Some("unknown"),
        "freshness_class must default to 'unknown', got {default:?}"
    );
}

#[tokio::test]
async fn test_v2_media_items_has_sync_generation_default_zero() {
    let store = fresh_store().await;
    assert!(
        column_exists(&store, "media_items", "sync_generation").await,
        "v2 must add media_items.sync_generation"
    );
    let default = column_default(&store, "media_items", "sync_generation").await;
    assert_eq!(
        default.as_deref(),
        Some("0"),
        "sync_generation default should be 0"
    );
}

#[tokio::test]
async fn test_v2_devices_has_freshness_columns() {
    let store = fresh_store().await;
    assert!(column_exists(&store, "devices", "freshness_class").await);
    assert!(column_exists(&store, "devices", "sync_generation").await);
}

#[tokio::test]
async fn test_v2_playback_snapshots_has_freshness_columns() {
    let store = fresh_store().await;
    assert!(column_exists(&store, "playback_snapshots", "freshness_class").await);
    assert!(column_exists(&store, "playback_snapshots", "sync_generation").await);
}

#[tokio::test]
async fn test_v2_recent_items_has_freshness_columns() {
    let store = fresh_store().await;
    assert!(column_exists(&store, "recent_items", "freshness_class").await);
    assert!(column_exists(&store, "recent_items", "sync_generation").await);
}

#[tokio::test]
async fn test_v2_library_items_has_freshness_columns() {
    let store = fresh_store().await;
    assert!(column_exists(&store, "library_items", "freshness_class").await);
    assert!(column_exists(&store, "library_items", "sync_generation").await);
}

#[tokio::test]
async fn test_v7_playlist_cache_tables_have_freshness_columns() {
    let store = fresh_store().await;
    for table in ["playlists", "playlist_items"] {
        assert!(column_exists(&store, table, "freshness_class").await);
        assert!(column_exists(&store, table, "sync_generation").await);
    }
}

#[tokio::test]
async fn test_v8_library_items_has_sync_position_column() {
    let store = fresh_store().await;
    assert!(column_exists(&store, "library_items", "sync_position").await);
}

#[tokio::test]
async fn test_v10_creates_queue_cache_tables() {
    let store = fresh_store().await;
    assert!(table_exists(&store, "queue_snapshots").await);
    assert!(table_exists(&store, "queue_items").await);
    assert!(index_exists(&store, "idx_queue_snapshots_time").await);
    assert!(index_exists(&store, "idx_queue_items_item").await);
    for (table, columns) in [
        (
            "queue_snapshots",
            &["currently_playing_uri", "fetched_at_ms", "freshness_class"][..],
        ),
        (
            "queue_items",
            &["snapshot_id", "item_uri", "position", "freshness_class"][..],
        ),
    ] {
        for column in columns {
            assert!(column_exists(&store, table, column).await);
        }
    }
}

#[tokio::test]
async fn test_v11_playlists_has_tracks_accessible_column() {
    let store = fresh_store().await;
    assert!(column_exists(&store, "playlists", "tracks_accessible").await);
}

#[tokio::test]
async fn test_v12_creates_lyrics_lookup_failures_table() {
    let store = fresh_store().await;
    assert!(table_exists(&store, "lyrics_lookup_failures").await);
    for column in [
        "track_uri",
        "failed_at_ms",
        "unavailable_until_ms",
        "reason",
    ] {
        assert!(column_exists(&store, "lyrics_lookup_failures", column).await);
    }
}

#[tokio::test]
async fn test_v15_adds_sync_event_retry_after_secs() {
    let store = fresh_store().await;
    assert!(column_exists(&store, "sync_events", "retry_after_secs").await);
}

#[tokio::test]
async fn test_v19_replays_after_body_applied_but_stamp_missing() {
    let store = fresh_store().await;
    assert!(table_exists(&store, "analytics_import_runs").await);
    assert!(table_exists(&store, "external_scrobbles").await);
    assert!(column_exists(&store, "listen_facts", "measurement_kind").await);
    assert!(column_exists(&store, "listen_facts", "external_scrobble_id").await);

    sqlx::query("DELETE FROM schema_migrations WHERE version = 19")
        .execute(store.writer_for_test())
        .await
        .unwrap();

    store
        .run_migrations_idempotent_for_test()
        .await
        .expect("v19 body-applied/stamp-missing replay should not duplicate ALTER columns");

    assert!(table_exists(&store, "analytics_import_runs").await);
    assert!(table_exists(&store, "external_scrobbles").await);
    assert!(column_exists(&store, "listen_facts", "measurement_kind").await);
    assert!(column_exists(&store, "listen_facts", "external_scrobble_id").await);
    assert!(index_exists(&store, "idx_listen_facts_external_scrobble").await);
    let stamped: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations WHERE version = 19")
            .fetch_one(store.reader())
            .await
            .unwrap();
    assert_eq!(stamped, 1);
}

#[tokio::test]
async fn test_check_cache_version_reports_current_at_v2() {
    let store = fresh_store().await;
    let v = store.applied_cache_version().await.unwrap();
    assert_eq!(v, CACHE_VERSION as i64);
}

#[tokio::test]
async fn test_check_cache_version_returns_too_new_when_db_ahead() {
    let store = fresh_store().await;
    // Simulate a future migration applied row.
    sqlx::query(
        "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?, 'future', 0)",
    )
    .bind(99_i64)
    .execute(store.writer_for_test())
    .await
    .unwrap();

    match store.check_cache_version().await {
        Err(message) => {
            let s = message.to_string();
            assert!(s.contains("99"), "error must mention future version: {s}");
        }
        Ok(()) => panic!("expected check_cache_version to error on future version"),
    }
}

#[tokio::test]
async fn test_check_cache_version_clean_at_current() {
    let store = fresh_store().await;
    store
        .check_cache_version()
        .await
        .expect("v2 store should be ok");
}
