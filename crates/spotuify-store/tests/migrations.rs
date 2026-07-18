#![allow(clippy::panic, clippy::unwrap_used)]

//! Schema migration + cache_version gate tests.
//!
//! Adversarial coverage:
//! - v1 → vN migrations are idempotent (running twice is a no-op).
//! - Each migration adds the columns/tables its phase needs.
//! - Running against a future-version store (forward-incompat) is
//!   detected and refused rather than silently corrupting data.
//! - check_cache_version() reports the right state for tooling.

use spotuify_protocol::{OperationId, ReceiptId, SyncTargetData};
use spotuify_store::{ProviderReconciliationScope, Store, CACHE_VERSION};
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

async fn table_signature(
    store: &Store,
    table: &str,
) -> Vec<(String, String, i64, Option<String>, i64)> {
    sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(store.reader())
        .await
        .unwrap()
        .into_iter()
        .map(|row| {
            (
                row.get("name"),
                row.get("type"),
                row.get("notnull"),
                row.get("dflt_value"),
                row.get("pk"),
            )
        })
        .collect()
}

async fn table_index_signature(store: &Store, table: &str) -> Vec<(String, Option<String>)> {
    sqlx::query(
        "SELECT name, sql FROM sqlite_master
         WHERE type = 'index' AND tbl_name = ? AND sql IS NOT NULL
         ORDER BY name",
    )
    .bind(table)
    .fetch_all(store.reader())
    .await
    .unwrap()
    .into_iter()
    .map(|row| (row.get("name"), row.get("sql")))
    .collect()
}

async fn downgrade_provider_identity_schema_to_v21(store: &Store) {
    sqlx::query("DROP INDEX idx_media_items_provider")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query("ALTER TABLE media_items RENAME COLUMN search_origin TO source")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query("ALTER TABLE media_items ADD COLUMN spotify_id TEXT")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query("ALTER TABLE media_items DROP COLUMN provider")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query(
        "ALTER TABLE external_scrobbles RENAME COLUMN resolved_uri TO resolved_spotify_uri",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();
    sqlx::query("DELETE FROM schema_migrations WHERE version = 22")
        .execute(store.writer_for_test())
        .await
        .unwrap();
}

async fn downgrade_provider_sync_schema_to_v22(store: &Store) {
    sqlx::query("DROP INDEX IF EXISTS idx_sync_events_provider_domain_time")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE sync_events_v22 (
            id               INTEGER PRIMARY KEY AUTOINCREMENT,
            domain           TEXT NOT NULL,
            started_at_ms    INTEGER NOT NULL,
            finished_at_ms   INTEGER NOT NULL,
            status           TEXT NOT NULL,
            row_count        INTEGER NOT NULL,
            error            TEXT,
            retry_after_secs INTEGER
        )",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO sync_events_v22 (
            id, domain, started_at_ms, finished_at_ms, status,
            row_count, error, retry_after_secs
         ) SELECT id, domain, started_at_ms, finished_at_ms, status,
                  row_count, error, retry_after_secs
           FROM sync_events",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();
    sqlx::query("DROP TABLE sync_events")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query("ALTER TABLE sync_events_v22 RENAME TO sync_events")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query(
        "CREATE INDEX idx_sync_events_domain_time
         ON sync_events(domain, finished_at_ms DESC)",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();

    sqlx::query(
        "CREATE TABLE sync_cursors_v22 (
            domain             TEXT PRIMARY KEY,
            last_success_at_ms INTEGER,
            last_error         TEXT
        )",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();
    sqlx::query(
        "INSERT OR REPLACE INTO sync_cursors_v22 (domain, last_success_at_ms, last_error)
         SELECT domain, last_success_at_ms, last_error
         FROM sync_cursors WHERE provider = 'spotify'",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();
    sqlx::query("DROP TABLE sync_cursors")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query("ALTER TABLE sync_cursors_v22 RENAME TO sync_cursors")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query("DELETE FROM schema_migrations WHERE version = 23")
        .execute(store.writer_for_test())
        .await
        .unwrap();
}

async fn downgrade_search_runs_schema_to_v23(store: &Store) {
    sqlx::query("DROP INDEX IF EXISTS idx_search_runs_query")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query("ALTER TABLE search_runs DROP COLUMN provider")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query(
        "CREATE INDEX idx_search_runs_query
         ON search_runs(normalized_query, scope, source, fetched_at_ms DESC)",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();
    sqlx::query("DELETE FROM schema_migrations WHERE version = 24")
        .execute(store.writer_for_test())
        .await
        .unwrap();
}

#[tokio::test]
async fn test_v24_search_runs_upgrade_defaults_legacy_rows_and_matches_fresh_schema() {
    let root = temp_store_root("v24-provider-search-upgrade");
    let db_path = root.join("cache.sqlite");
    let index_path = root.join("index");
    let store = Store::open(&db_path, &index_path).await.unwrap();
    sqlx::query(
        "INSERT INTO search_runs (
            query, normalized_query, scope, source, fetched_at_ms, status, result_count, provider
         ) VALUES ('legacy', 'legacy', 'track', 'remote', 1, 'ok', 0, 'work')",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();
    downgrade_search_runs_schema_to_v23(&store).await;
    drop(store);

    let upgraded = Store::open(&db_path, &index_path).await.unwrap();
    let provider: String =
        sqlx::query_scalar("SELECT provider FROM search_runs WHERE query = 'legacy'")
            .fetch_one(upgraded.reader())
            .await
            .unwrap();
    assert_eq!(provider, "spotify");
    let stamp: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations WHERE version = 24")
            .fetch_one(upgraded.reader())
            .await
            .unwrap();
    assert_eq!(stamp, 1);

    let fresh = fresh_store().await;
    assert_eq!(
        table_signature(&upgraded, "search_runs").await,
        table_signature(&fresh, "search_runs").await,
    );
    assert_eq!(
        table_index_signature(&upgraded, "search_runs").await,
        table_index_signature(&fresh, "search_runs").await,
    );

    drop((upgraded, fresh));
    let _ = std::fs::remove_dir_all(root);
}

async fn assert_broken_column_refuses_reopen(table: &str, column: &str) {
    let root = temp_store_root(&format!("broken-{table}-{column}"));
    let db_path = root.join("cache.sqlite");
    let index_path = root.join("index");
    let store = Store::open(&db_path, &index_path)
        .await
        .expect("fresh store");
    sqlx::query(&format!(
        "ALTER TABLE {table} RENAME COLUMN {column} TO broken_{column}"
    ))
    .execute(store.writer_for_test())
    .await
    .expect("break required column");
    drop(store);

    let err = match Store::open(&db_path, &index_path).await {
        Ok(_) => panic!("broken schema should be refused"),
        Err(err) => err,
    };
    let expected = format!("missing required column {table}.{column}");
    assert!(
        err.to_string().contains(&expected),
        "error should name the broken column: {err}"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn test_open_refuses_broken_initial_schema_column() {
    assert_broken_column_refuses_reopen("media_items", "search_origin").await;
}

#[tokio::test]
async fn test_open_refuses_broken_late_migration_column() {
    assert_broken_column_refuses_reopen("reminder_schedules", "state").await;
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
async fn test_v22_provider_identity_schema_is_vendor_neutral() {
    let store = fresh_store().await;
    for column in ["provider", "search_origin"] {
        assert!(column_exists(&store, "media_items", column).await);
    }
    assert!(!column_exists(&store, "media_items", "spotify_id").await);
    assert!(!column_exists(&store, "media_items", "source").await);
    assert!(column_exists(&store, "external_scrobbles", "resolved_uri").await);
    assert!(!column_exists(&store, "external_scrobbles", "resolved_spotify_uri").await);
    assert!(index_exists(&store, "idx_media_items_provider").await);
}

#[tokio::test]
async fn test_v21_upgrade_preserves_rows_and_matches_fresh_schema() {
    let root = temp_store_root("v21-provider-upgrade");
    let db_path = root.join("cache.sqlite");
    let index_path = root.join("index");
    let store = Store::open(&db_path, &index_path).await.unwrap();
    let item = spotuify_core::MediaItem {
        id: Some("track-1".to_string()),
        uri: "spotify:track:track-1".to_string(),
        name: "Upgrade Track".to_string(),
        subtitle: "Upgrade Artist".to_string(),
        context: "Upgrade Album".to_string(),
        duration_ms: 123_000,
        kind: spotuify_core::MediaKind::Track,
        source: Some(spotuify_core::ItemSource::Provider("spotify".to_string())),
        ..Default::default()
    };
    store.upsert_media_items(&[item], "spotify").await.unwrap();
    sqlx::query(
        "INSERT INTO library_items (item_uri, kind, saved, followed, fetched_at_ms)
         VALUES ('spotify:track:track-1', 'track', 1, 0, 1)",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO external_scrobbles (
            provider, username, import_run_id, idempotency_key, scrobbled_at_ms,
            artist_name, track_name, raw_json, normalized_key, resolution_status,
            resolved_uri, created_at_ms, updated_at_ms
         ) VALUES (
            'lastfm', 'user', 'run-1', 'key-1', 1,
            'Upgrade Artist', 'Upgrade Track', '{}', 'upgrade track', 'resolved',
            'spotify:track:track-1', 1, 1
         )",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();

    // Recreate the exact v21 column shape while retaining representative rows.
    downgrade_provider_identity_schema_to_v21(&store).await;
    sqlx::query("UPDATE media_items SET spotify_id = 'track-1'")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    drop(store);

    let upgraded = Store::open(&db_path, &index_path).await.unwrap();
    let provider: String =
        sqlx::query_scalar("SELECT provider FROM media_items WHERE uri = 'spotify:track:track-1'")
            .fetch_one(upgraded.reader())
            .await
            .unwrap();
    assert_eq!(provider, "spotify");
    let resolved: Option<String> = sqlx::query_scalar(
        "SELECT resolved_uri FROM external_scrobbles WHERE idempotency_key = 'key-1'",
    )
    .fetch_one(upgraded.reader())
    .await
    .unwrap();
    assert_eq!(resolved.as_deref(), Some("spotify:track:track-1"));
    let library_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM library_items WHERE item_uri = 'spotify:track:track-1'",
    )
    .fetch_one(upgraded.reader())
    .await
    .unwrap();
    assert_eq!(library_rows, 1, "media migration must preserve FK children");
    let loaded = upgraded
        .media_items_by_uris(&["spotify:track:track-1".to_string()])
        .await
        .unwrap();
    assert_eq!(loaded[0].id.as_deref(), Some("track-1"));
    assert_eq!(
        loaded[0]
            .source
            .as_ref()
            .map(spotuify_core::ItemSource::as_str),
        Some("spotify")
    );

    // A missing stamp over an already-final schema is safely replayed.
    sqlx::query("DELETE FROM schema_migrations WHERE version = 22")
        .execute(upgraded.writer_for_test())
        .await
        .unwrap();
    drop(upgraded);
    let upgraded = Store::open(&db_path, &index_path).await.unwrap();

    let fresh = fresh_store().await;
    for table in ["media_items", "external_scrobbles"] {
        assert_eq!(
            table_signature(&upgraded, table).await,
            table_signature(&fresh, table).await,
            "fresh and upgraded {table} schemas must match"
        );
        assert_eq!(
            table_index_signature(&upgraded, table).await,
            table_index_signature(&fresh, table).await,
            "fresh and upgraded {table} indexes must match"
        );
    }

    drop(upgraded);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn test_v22_upgrade_drops_legacy_bad_rows_and_migrates_good_rows() {
    // The cache is rebuildable, so v22 must tolerate legacy rows that released
    // code persisted verbatim (Spotify local-file URIs) or whose kind no longer
    // matches the URI: drop them (with their FK children) instead of rolling
    // back and bricking every daemon start. Good rows must still migrate.
    let root = temp_store_root("v22-tolerant-upgrade");
    let db_path = root.join("cache.sqlite");
    let index_path = root.join("index");
    let store = Store::open(&db_path, &index_path).await.unwrap();
    store
        .upsert_media_items(
            &[spotuify_core::MediaItem {
                id: Some("good".to_string()),
                uri: "spotify:track:good".to_string(),
                name: "Good Track".to_string(),
                kind: spotuify_core::MediaKind::Track,
                ..Default::default()
            }],
            "spotify",
        )
        .await
        .unwrap();
    downgrade_provider_identity_schema_to_v21(&store).await;
    // Legacy non-canonical local-file URI (6 segments; unparseable).
    sqlx::query(
        "INSERT INTO media_items (uri, kind, name, source, fetched_at_ms, updated_at_ms)
         VALUES ('spotify:local:Artist:Album:Title:290', 'track', 'Local File', 'spotify', 1, 1)",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();
    // Kind that disagrees with the URI.
    sqlx::query(
        "INSERT INTO media_items (uri, kind, name, source, fetched_at_ms, updated_at_ms)
         VALUES ('spotify:track:mismatch', 'album', 'Mismatch', 'spotify', 1, 1)",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();
    // A dependent FK child on the bad local row must be cascade-cleaned.
    sqlx::query(
        "INSERT INTO library_items (item_uri, kind, saved, followed, fetched_at_ms)
         VALUES ('spotify:local:Artist:Album:Title:290', 'track', 1, 0, 1)",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();
    drop(store);

    let upgraded = Store::open(&db_path, &index_path)
        .await
        .expect("tolerant migration must succeed over legacy bad rows");
    let stamp: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations WHERE version = 22")
            .fetch_one(upgraded.reader())
            .await
            .unwrap();
    assert_eq!(stamp, 1, "tolerant migration must still stamp v22");
    assert!(column_exists(&upgraded, "media_items", "provider").await);

    let bad_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_items
         WHERE uri IN ('spotify:local:Artist:Album:Title:290', 'spotify:track:mismatch')",
    )
    .fetch_one(upgraded.reader())
    .await
    .unwrap();
    assert_eq!(bad_rows, 0, "bad rows must be dropped");
    let dangling: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM library_items
         WHERE item_uri = 'spotify:local:Artist:Album:Title:290'",
    )
    .fetch_one(upgraded.reader())
    .await
    .unwrap();
    assert_eq!(
        dangling, 0,
        "FK children of dropped rows must be cascade-cleaned"
    );
    let provider: String =
        sqlx::query_scalar("SELECT provider FROM media_items WHERE uri = 'spotify:track:good'")
            .fetch_one(upgraded.reader())
            .await
            .unwrap();
    assert_eq!(provider, "spotify", "good rows must still migrate");
    drop(upgraded);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn test_open_tolerates_bad_media_row_and_repair_scrubs_it() {
    // Store::open must never fail on row DATA (one bad row would make the daemon
    // permanently unstartable and cost a full-table read on every start). The
    // bounded row-integrity pass lives on the explicit repair path instead.
    let root = temp_store_root("open-tolerates-bad-row");
    let db_path = root.join("cache.sqlite");
    let index_path = root.join("index");
    let store = Store::open(&db_path, &index_path).await.unwrap();
    store
        .upsert_media_items(
            &[spotuify_core::MediaItem {
                id: Some("ok".to_string()),
                uri: "spotify:track:ok".to_string(),
                name: "OK".to_string(),
                kind: spotuify_core::MediaKind::Track,
                ..Default::default()
            }],
            "spotify",
        )
        .await
        .unwrap();
    // Corrupt the kind so it no longer matches the URI.
    sqlx::query("UPDATE media_items SET kind = 'album' WHERE uri = 'spotify:track:ok'")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    drop(store);

    let reopened = Store::open(&db_path, &index_path)
        .await
        .expect("open must tolerate a bad row");
    reopened
        .repair_schema()
        .await
        .expect("repair scrubs bad rows");
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_items")
        .fetch_one(reopened.reader())
        .await
        .unwrap();
    assert_eq!(remaining, 0, "repair must drop the invalid row");
    drop(reopened);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn test_v22_startup_validation_rejects_malformed_provider_index() {
    let root = temp_store_root("v22-malformed-provider-index");
    let db_path = root.join("cache.sqlite");
    let index_path = root.join("index");
    let store = Store::open(&db_path, &index_path).await.unwrap();
    sqlx::query("DROP INDEX idx_media_items_provider")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query("CREATE INDEX idx_media_items_provider ON media_items(kind)")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    drop(store);

    let err = match Store::open(&db_path, &index_path).await {
        Ok(_) => panic!("malformed required index must be refused"),
        Err(err) => err,
    };
    assert!(
        err.to_string()
            .contains("idx_media_items_provider must be media_items(provider)"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn test_v22_startup_validation_rejects_unique_provider_index() {
    let root = temp_store_root("v22-unique-provider-index");
    let db_path = root.join("cache.sqlite");
    let index_path = root.join("index");
    let store = Store::open(&db_path, &index_path).await.unwrap();
    sqlx::query("DROP INDEX idx_media_items_provider")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query("CREATE UNIQUE INDEX idx_media_items_provider ON media_items(provider)")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    drop(store);

    let err = match Store::open(&db_path, &index_path).await {
        Ok(_) => panic!("unique provider index must be refused"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("non-unique"), "{err}");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn test_v22_startup_validation_rejects_partial_provider_index() {
    let root = temp_store_root("v22-partial-provider-index");
    let db_path = root.join("cache.sqlite");
    let index_path = root.join("index");
    let store = Store::open(&db_path, &index_path).await.unwrap();
    sqlx::query("DROP INDEX idx_media_items_provider")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query(
        "CREATE INDEX idx_media_items_provider ON media_items(provider)
         WHERE provider = 'spotify'",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();
    drop(store);

    let err = match Store::open(&db_path, &index_path).await {
        Ok(_) => panic!("partial provider index must be refused"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("non-partial"), "{err}");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn test_v22_concurrent_openers_serialize_and_stamp_once() {
    let root = temp_store_root("v22-concurrent-openers");
    let db_path = root.join("cache.sqlite");
    let index_path = root.join("index");
    let store = Store::open(&db_path, &index_path).await.unwrap();
    downgrade_provider_identity_schema_to_v21(&store).await;
    drop(store);

    let (first, second) = tokio::join!(
        Store::open(&db_path, &index_path),
        Store::open(&db_path, &index_path)
    );
    let first = first.expect("first concurrent opener");
    let second = second.expect("second concurrent opener");
    let stamp: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations WHERE version = 22")
            .fetch_one(first.reader())
            .await
            .unwrap();
    assert_eq!(stamp, 1);
    assert!(column_exists(&second, "media_items", "provider").await);
    drop((first, second));
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn test_v23_upgrade_preserves_rows_and_matches_fresh_schema() {
    let root = temp_store_root("v23-provider-sync-upgrade");
    let db_path = root.join("cache.sqlite");
    let index_path = root.join("index");
    let store = Store::open(&db_path, &index_path).await.unwrap();
    store
        .record_provider_sync_event("spotify", "library", 1, "ok", 3, None)
        .await
        .unwrap();
    downgrade_provider_sync_schema_to_v22(&store).await;
    drop(store);

    let upgraded = Store::open(&db_path, &index_path).await.unwrap();
    let historical_provider: String =
        sqlx::query_scalar("SELECT provider FROM sync_events WHERE domain = 'library'")
            .fetch_one(upgraded.reader())
            .await
            .unwrap();
    // Legacy rows predate the provider abstraction (Spotify-only era), so they
    // migrate to 'spotify' to match sync_cursors and stay visible to
    // provider-scoped reads under 'spotify'.
    assert_eq!(historical_provider, "spotify");
    let cursor_provider: String =
        sqlx::query_scalar("SELECT provider FROM sync_cursors WHERE domain = 'library'")
            .fetch_one(upgraded.reader())
            .await
            .unwrap();
    assert_eq!(cursor_provider, "spotify");
    let stamp: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations WHERE version = 23")
            .fetch_one(upgraded.reader())
            .await
            .unwrap();
    assert_eq!(stamp, 1);

    let fresh = fresh_store().await;
    for table in ["sync_events", "sync_cursors"] {
        assert_eq!(
            table_signature(&upgraded, table).await,
            table_signature(&fresh, table).await,
            "fresh and upgraded {table} schemas must match"
        );
        assert_eq!(
            table_index_signature(&upgraded, table).await,
            table_index_signature(&fresh, table).await,
            "fresh and upgraded {table} indexes must match"
        );
    }

    drop(upgraded);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn test_v23_repairs_stamped_malformed_provider_cursor_and_index() {
    let root = temp_store_root("v23-stamped-structural-repair");
    let db_path = root.join("cache.sqlite");
    let index_path = root.join("index");
    let store = Store::open(&db_path, &index_path).await.unwrap();

    sqlx::query("DROP INDEX idx_sync_events_provider_domain_time")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE sync_events_bad (
            id               INTEGER PRIMARY KEY AUTOINCREMENT,
            domain           TEXT NOT NULL,
            started_at_ms    INTEGER NOT NULL,
            finished_at_ms   INTEGER NOT NULL,
            status           TEXT NOT NULL,
            row_count        INTEGER NOT NULL,
            error            TEXT,
            retry_after_secs INTEGER,
            provider         INTEGER DEFAULT NULL
        )",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO sync_events_bad (
            domain, started_at_ms, finished_at_ms, status, row_count, provider
         ) VALUES ('library', 1, 2, 'error', 0, NULL)",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();
    sqlx::query("DROP TABLE sync_events")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query("ALTER TABLE sync_events_bad RENAME TO sync_events")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query(
        "CREATE UNIQUE INDEX idx_sync_events_provider_domain_time
         ON sync_events(domain)",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();

    sqlx::query("DROP TABLE sync_cursors")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE sync_cursors (
            provider TEXT,
            domain   TEXT,
            cursor   TEXT,
            PRIMARY KEY (provider, domain)
         )",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO sync_cursors (provider, domain, cursor)
         VALUES (NULL, 'library/track', 'opaque')",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();
    drop(store);

    let repaired = Store::open(&db_path, &index_path).await.unwrap();
    let event_provider: String =
        sqlx::query_scalar("SELECT provider FROM sync_events WHERE domain = 'library'")
            .fetch_one(repaired.reader())
            .await
            .unwrap();
    // NULL legacy provider coalesces to 'spotify' (matching sync_cursors), not
    // 'system', so pre-upgrade history stays visible to 'spotify'-scoped reads.
    assert_eq!(event_provider, "spotify");
    let (cursor_provider, cursor): (String, Vec<u8>) =
        sqlx::query_as("SELECT provider, cursor FROM sync_cursors WHERE domain = 'library/track'")
            .fetch_one(repaired.reader())
            .await
            .unwrap();
    assert_eq!(cursor_provider, "spotify");
    assert_eq!(cursor, b"opaque");

    let fresh = fresh_store().await;
    for table in ["sync_events", "sync_cursors"] {
        assert_eq!(
            table_signature(&repaired, table).await,
            table_signature(&fresh, table).await,
            "repaired and fresh {table} schemas must match"
        );
        assert_eq!(
            table_index_signature(&repaired, table).await,
            table_index_signature(&fresh, table).await,
            "repaired and fresh {table} indexes must match"
        );
    }

    drop(repaired);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn test_v23_repairs_any_noncanonical_sync_events_column_transactionally() {
    let root = temp_store_root("v23-full-event-signature-repair");
    let db_path = root.join("cache.sqlite");
    let index_path = root.join("index");
    let store = Store::open(&db_path, &index_path).await.unwrap();
    sqlx::query("DROP INDEX idx_sync_events_provider_domain_time")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE sync_events_bad (
            id               INTEGER PRIMARY KEY AUTOINCREMENT,
            domain           TEXT NOT NULL,
            started_at_ms    INTEGER NOT NULL,
            finished_at_ms   INTEGER NOT NULL,
            status           TEXT NOT NULL,
            row_count        TEXT,
            error            TEXT,
            provider         TEXT NOT NULL DEFAULT 'system'
        )",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO sync_events_bad (
            domain, started_at_ms, finished_at_ms, status, row_count, provider
         ) VALUES ('library', 1, 2, 'ok', '7', 'apple')",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();
    sqlx::query("DROP TABLE sync_events")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query("ALTER TABLE sync_events_bad RENAME TO sync_events")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    drop(store);

    let repaired = Store::open(&db_path, &index_path).await.unwrap();
    let fresh = fresh_store().await;
    assert_eq!(
        table_signature(&repaired, "sync_events").await,
        table_signature(&fresh, "sync_events").await
    );
    let row: (String, i64, Option<i64>) = sqlx::query_as(
        "SELECT provider, row_count, retry_after_secs FROM sync_events WHERE domain = 'library'",
    )
    .fetch_one(repaired.reader())
    .await
    .unwrap();
    assert_eq!(row, ("apple".to_string(), 7, None));
    let stamp: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations WHERE version = 23")
            .fetch_one(repaired.reader())
            .await
            .unwrap();
    assert_eq!(stamp, 1);

    drop((repaired, fresh));
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn test_v23_repairs_stamped_missing_sync_state_tables() {
    let root = temp_store_root("v23-stamped-missing-tables");
    let db_path = root.join("cache.sqlite");
    let index_path = root.join("index");
    let store = Store::open(&db_path, &index_path).await.unwrap();
    sqlx::query("DROP TABLE sync_events")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query("DROP TABLE sync_cursors")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    drop(store);

    let repaired = Store::open(&db_path, &index_path).await.unwrap();
    let fresh = fresh_store().await;
    for table in ["sync_events", "sync_cursors"] {
        assert_eq!(
            table_signature(&repaired, table).await,
            table_signature(&fresh, table).await,
            "repaired and fresh {table} schemas must match"
        );
        assert_eq!(
            table_index_signature(&repaired, table).await,
            table_index_signature(&fresh, table).await,
            "repaired and fresh {table} indexes must match"
        );
    }
    let stamp: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations WHERE version = 23")
            .fetch_one(repaired.reader())
            .await
            .unwrap();
    assert_eq!(stamp, 1);

    repaired
        .record_provider_sync_event("apple", "library", 1, "ok", 1, None)
        .await
        .unwrap();
    let provider: String =
        sqlx::query_scalar("SELECT provider FROM sync_events WHERE domain = 'library'")
            .fetch_one(repaired.reader())
            .await
            .unwrap();
    assert_eq!(provider, "apple");

    drop(repaired);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn test_v23_repairs_stamped_index_with_wrong_sort_order() {
    let root = temp_store_root("v23-stamped-index-repair");
    let db_path = root.join("cache.sqlite");
    let index_path = root.join("index");
    let store = Store::open(&db_path, &index_path).await.unwrap();
    sqlx::query("DROP INDEX idx_sync_events_provider_domain_time")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query(
        "CREATE INDEX idx_sync_events_provider_domain_time
         ON sync_events(provider, domain, finished_at_ms)",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();
    drop(store);

    let repaired = Store::open(&db_path, &index_path).await.unwrap();
    let finished_desc: i64 = sqlx::query_scalar(
        "SELECT \"desc\" FROM pragma_index_xinfo('idx_sync_events_provider_domain_time')
         WHERE name = 'finished_at_ms'",
    )
    .fetch_one(repaired.reader())
    .await
    .unwrap();
    assert_eq!(finished_desc, 1);

    drop(repaired);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn test_v23_concurrent_openers_serialize_structural_repair() {
    let root = temp_store_root("v23-concurrent-openers");
    let db_path = root.join("cache.sqlite");
    let index_path = root.join("index");
    let store = Store::open(&db_path, &index_path).await.unwrap();
    downgrade_provider_sync_schema_to_v22(&store).await;
    drop(store);

    let (first, second) = tokio::join!(
        Store::open(&db_path, &index_path),
        Store::open(&db_path, &index_path)
    );
    let first = first.expect("first concurrent opener");
    let second = second.expect("second concurrent opener");
    let stamp: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations WHERE version = 23")
            .fetch_one(first.reader())
            .await
            .unwrap();
    assert_eq!(stamp, 1);
    assert!(column_exists(&second, "sync_events", "provider").await);
    assert!(index_exists(&second, "idx_sync_events_provider_domain_time").await);

    drop((first, second));
    let _ = std::fs::remove_dir_all(root);
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
async fn test_v27_preserves_v26_reconciliation_rows_as_targeted() {
    let root = temp_store_root("v26-provider-reconciliation-upgrade");
    let db_path = root.join("cache.db");
    let index_path = root.join("index");
    let store = Store::open(&db_path, &index_path).await.unwrap();
    let receipt_id = ReceiptId::new_v7();
    let operation_id = OperationId::new_v7();

    sqlx::query("DROP TABLE provider_reconciliations")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::raw_sql(
        "CREATE TABLE provider_reconciliations (
            receipt_id         TEXT PRIMARY KEY REFERENCES receipts(receipt_id) ON DELETE CASCADE,
            operation_id       TEXT NOT NULL REFERENCES operations(operation_id) ON DELETE CASCADE,
            provider           TEXT NOT NULL,
            target             TEXT NOT NULL CHECK(target IN ('library', 'playlists')),
            resource_uris_json TEXT NOT NULL,
            status             TEXT NOT NULL CHECK(status IN ('pending', 'running', 'completed')),
            attempts           INTEGER NOT NULL DEFAULT 0,
            last_error         TEXT,
            created_at_ms      INTEGER NOT NULL,
            finished_at_ms     INTEGER
        );
        CREATE INDEX idx_provider_reconciliations_status_created
            ON provider_reconciliations(status, created_at_ms);",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO receipts
         (receipt_id, action, status, request_json, started_at_ms)
         VALUES (?, 'save', 'failed', '{}', 10)",
    )
    .bind(receipt_id.to_string())
    .execute(store.writer_for_test())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO operations (
            operation_id, kind, occurred_at_ms, source, subject_uris_json,
            reversible, status, receipt_id
         ) VALUES (?, 'library_save', 10, 'daemon-internal', '[]', 0, 'failed', ?)",
    )
    .bind(operation_id.to_string())
    .bind(receipt_id.to_string())
    .execute(store.writer_for_test())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_reconciliations (
            receipt_id, operation_id, provider, target, resource_uris_json,
            status, attempts, last_error, created_at_ms
         ) VALUES (?, ?, 'fake', 'library', '[\"fake:track:one\"]',
                   'pending', 3, 'offline', 20)",
    )
    .bind(receipt_id.to_string())
    .bind(operation_id.to_string())
    .execute(store.writer_for_test())
    .await
    .unwrap();
    sqlx::query("DELETE FROM schema_migrations WHERE version >= 27")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    drop(store);

    let upgraded = Store::open(&db_path, &index_path).await.unwrap();
    let rows = upgraded
        .pending_provider_reconciliations_for_receipt(receipt_id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].operation_id, operation_id);
    assert_eq!(rows[0].target, SyncTargetData::Library);
    assert_eq!(rows[0].scope, ProviderReconciliationScope::Targeted);
    assert_eq!(rows[0].resource_uris, vec!["fake:track:one".to_string()]);
    assert_eq!(rows[0].attempts, 3);
    assert_eq!(rows[0].last_error.as_deref(), Some("offline"));
    assert!(column_exists(&upgraded, "provider_reconciliations", "reconciliation_id").await);
    assert!(column_exists(&upgraded, "provider_reconciliations", "scope").await);
    drop(upgraded);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn test_check_cache_version_reports_current_at_v2() {
    let store = fresh_store().await;
    let v = store.applied_cache_version().await.unwrap();
    assert_eq!(v, CACHE_VERSION as i64);
}

#[tokio::test]
async fn test_stamped_v28_without_stability_table_upgrades() {
    let root = temp_store_root("v28-stability-upgrade");
    let db_path = root.join("cache.db");
    let index_path = root.join("index");
    let store = Store::open(&db_path, &index_path).await.unwrap();
    sqlx::query("DROP TABLE provider_reconciliation_stability")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query("DELETE FROM schema_migrations WHERE version >= 29")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    drop(store);

    let upgraded = Store::open(&db_path, &index_path).await.unwrap();
    assert!(table_exists(&upgraded, "provider_reconciliation_stability").await);
    assert!(
        column_exists(
            &upgraded,
            "provider_reconciliation_stability",
            "next_pass_after_ms",
        )
        .await
    );
    assert_eq!(
        upgraded.applied_cache_version().await.unwrap(),
        CACHE_VERSION as i64
    );
    drop(upgraded);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn test_stamped_v29_partial_stability_table_upgrades() {
    let root = temp_store_root("v29-stability-upgrade");
    let db_path = root.join("cache.db");
    let index_path = root.join("index");
    let store = Store::open(&db_path, &index_path).await.unwrap();
    sqlx::query("DROP TABLE provider_reconciliation_stability")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE provider_reconciliation_stability (
            reconciliation_id TEXT PRIMARY KEY
                REFERENCES provider_reconciliations(reconciliation_id) ON DELETE CASCADE,
            required_passes INTEGER NOT NULL CHECK(required_passes >= 2),
            successful_passes INTEGER NOT NULL DEFAULT 0 CHECK(successful_passes >= 0)
        )",
    )
    .execute(store.writer_for_test())
    .await
    .unwrap();
    sqlx::query("DELETE FROM schema_migrations WHERE version >= 30")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    drop(store);

    let upgraded = Store::open(&db_path, &index_path).await.unwrap();
    assert!(
        column_exists(
            &upgraded,
            "provider_reconciliation_stability",
            "next_pass_after_ms",
        )
        .await
    );
    assert_eq!(
        upgraded.applied_cache_version().await.unwrap(),
        CACHE_VERSION as i64
    );
    drop(upgraded);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn test_stamped_v30_without_claim_tokens_upgrades() {
    let root = temp_store_root("v30-claim-token-upgrade");
    let db_path = root.join("cache.db");
    let index_path = root.join("index");
    let store = Store::open(&db_path, &index_path).await.unwrap();
    sqlx::query("ALTER TABLE provider_reconciliations DROP COLUMN claim_token")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query("ALTER TABLE provider_reconciliations DROP COLUMN last_claim_token")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    sqlx::query("DELETE FROM schema_migrations WHERE version >= 31")
        .execute(store.writer_for_test())
        .await
        .unwrap();
    drop(store);

    let upgraded = Store::open(&db_path, &index_path).await.unwrap();
    assert!(column_exists(&upgraded, "provider_reconciliations", "claim_token").await);
    assert!(column_exists(&upgraded, "provider_reconciliations", "last_claim_token",).await);
    assert_eq!(
        upgraded.applied_cache_version().await.unwrap(),
        CACHE_VERSION as i64
    );
    drop(upgraded);
    std::fs::remove_dir_all(root).unwrap();
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
