//! Phase 10 (P10.1, P10.3-5) — listen_facts CRUD + per-entity rollup
//! upserts + top-N / habits / rediscovery queries + retention prune.
//!
//! Reads and writes are done through the regular Store pools so the
//! SessionTracker's hot path uses the same writer as Phase 6 receipts
//! and Phase 12 operations — no parallel WAL.

use anyhow::Result;
use sqlx::Row;

use spotuify_core::{ListenFact, MeasurementKind};
use spotuify_protocol::{
    HabitBucket, HabitWindow, RebuildReport, RediscoveryCandidate, SearchHistoryEntry, SinceWindow,
    TopEntry, TopKind,
};

use crate::Store;

impl Store {
    /// Insert one `ListenFact`. Returns the auto-assigned row id.
    pub async fn insert_listen_fact(&self, fact: &ListenFact) -> Result<i64> {
        let res = sqlx::query(
            "INSERT INTO listen_facts (
                session_id, track_uri, artist_uri, album_uri, context_uri,
                started_at_ms, ended_at_ms, duration_ms, elapsed_ms,
                audible_ms, completion_ratio, qualified,
                qualification_rule_version, skip_reason, source, backend,
                private_session, measurement_kind, external_scrobble_id, created_at_ms
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&fact.session_id)
        .bind(&fact.track_uri)
        .bind(fact.artist_uri.as_deref())
        .bind(fact.album_uri.as_deref())
        .bind(fact.context_uri.as_deref())
        .bind(fact.started_at_ms)
        .bind(fact.ended_at_ms)
        .bind(fact.duration_ms)
        .bind(fact.elapsed_ms)
        .bind(fact.audible_ms)
        .bind(fact.completion_ratio)
        .bind(fact.qualified as i64)
        .bind(fact.qualification_rule_version as i64)
        .bind(fact.skip_reason.as_ref().map(|r| r.label()))
        .bind(fact.source.as_ref().map(|s| s.label()))
        .bind(fact.backend.as_ref().map(|b| b.label()))
        .bind(fact.private_session as i64)
        .bind(fact.measurement_kind.label())
        .bind(fact.external_scrobble_id)
        .bind(fact.created_at_ms)
        .execute(&self.writer)
        .await?;
        Ok(res.last_insert_rowid())
    }

    /// Upsert the rollup row for a track. Increments the appropriate
    /// counters atomically so concurrent finalisations stay correct.
    pub async fn upsert_track_metric(
        &self,
        uri: &str,
        qualified: bool,
        audible_ms: i64,
        finalized_at_ms: i64,
    ) -> Result<()> {
        upsert_entity_metric(
            &self.writer,
            "track_metrics",
            "track_uri",
            uri,
            qualified,
            audible_ms,
            finalized_at_ms,
        )
        .await
    }

    pub async fn upsert_artist_metric(
        &self,
        uri: &str,
        qualified: bool,
        audible_ms: i64,
        finalized_at_ms: i64,
    ) -> Result<()> {
        upsert_entity_metric(
            &self.writer,
            "artist_metrics",
            "artist_uri",
            uri,
            qualified,
            audible_ms,
            finalized_at_ms,
        )
        .await
    }

    pub async fn upsert_album_metric(
        &self,
        uri: &str,
        qualified: bool,
        audible_ms: i64,
        finalized_at_ms: i64,
    ) -> Result<()> {
        upsert_entity_metric(
            &self.writer,
            "album_metrics",
            "album_uri",
            uri,
            qualified,
            audible_ms,
            finalized_at_ms,
        )
        .await
    }

    /// Recompute track/artist/album rollups from the canonical listen_facts table.
    /// Used after bulk removal (for example import undo) so legacy rollup-backed
    /// analytics stay consistent with fact-backed analytics.
    pub async fn rebuild_metric_rollups_from_listen_facts(&self) -> Result<()> {
        let mut tx = self.writer.begin().await?;
        sqlx::query("DELETE FROM track_metrics")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM artist_metrics")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM album_metrics")
            .execute(&mut *tx)
            .await?;

        let track_sql = metric_rebuild_sql("track_metrics", "track_uri", "track_uri");
        sqlx::query(&track_sql)
            .bind(spotuify_core::now_ms())
            .execute(&mut *tx)
            .await?;
        let artist_sql = metric_rebuild_sql("artist_metrics", "artist_uri", "artist_uri");
        sqlx::query(&artist_sql)
            .bind(spotuify_core::now_ms())
            .execute(&mut *tx)
            .await?;
        let album_sql = metric_rebuild_sql("album_metrics", "album_uri", "album_uri");
        sqlx::query(&album_sql)
            .bind(spotuify_core::now_ms())
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        Ok(())
    }

    /// Resolve cached artist/album Spotify URIs for a track, when the
    /// local cache has an unambiguous entity row. This deliberately does
    /// not synthesize URIs from display names.
    pub async fn listen_context_uris(
        &self,
        track_uri: &str,
    ) -> Result<(Option<String>, Option<String>)> {
        let Some(row) = sqlx::query(
            "SELECT subtitle, context, provider
             FROM media_items
             WHERE uri = ? AND kind = 'track'",
        )
        .bind(track_uri)
        .fetch_optional(&self.reader)
        .await?
        else {
            return Ok((None, None));
        };
        let artist_name: String = row.get("subtitle");
        let album_name: String = row.get("context");
        let provider: String = row.get("provider");
        let artist_uri =
            unique_media_uri_by_kind_and_name(&self.reader, "artist", &artist_name, &provider)
                .await?;
        let album_uri =
            unique_media_uri_by_kind_and_name(&self.reader, "album", &album_name, &provider)
                .await?;
        Ok((artist_uri, album_uri))
    }

    /// Top-N entries by total audible_ms (only counting qualified listens).
    /// Provider identity comes from the persisted adapter identity. `None`
    /// intentionally returns every partition without merging equivalent songs;
    /// cross-provider entity resolution is deferred to provider mappings.
    pub async fn top_entries(
        &self,
        kind: TopKind,
        since_window: SinceWindow,
        limit: u32,
        provider: Option<&str>,
    ) -> Result<Vec<TopEntry>> {
        let cutoff_ms = match since_window {
            SinceWindow::All => 0,
            SinceWindow::Days(days) => {
                spotuify_core::now_ms().saturating_sub((days as i64).saturating_mul(86_400_000))
            }
        };

        // For Track/Artist/Album, aggregate listen_facts (filtered by
        // cutoff + qualified=1) and join media_items / playlists for
        // display names. Falling back to the URI when names aren't
        // cached locally so the CLI never renders blanks.
        // Playlists group by the captured playback context (filtered to
        // playlist URIs) and join the playlists table for display names;
        // the other kinds group by their entity URI joined to media_items.
        let query = if matches!(kind, TopKind::Playlists) {
            "SELECT
                lf.context_uri AS uri,
                COALESCE(p.name, lf.context_uri) AS name,
                '' AS subtitle,
                COUNT(*) AS qualified_count,
                0 AS skip_count,
                SUM(lf.audible_ms) AS total_audible_ms,
                MAX(lf.started_at_ms) AS last_listened_at_ms
             FROM listen_facts lf
             LEFT JOIN playlists p
                ON p.uri = lf.context_uri
             LEFT JOIN media_items context_mi
                ON context_mi.uri = lf.context_uri
               AND context_mi.kind = 'playlist'
             WHERE lf.qualified = 1
               AND lf.started_at_ms >= ?
               AND lf.context_uri LIKE '%:playlist:%'
               AND (? IS NULL OR COALESCE(
                    context_mi.provider,
                    lower(substr(lf.context_uri, 1, instr(lf.context_uri, ':') - 1))
               ) = ?)
             GROUP BY lf.context_uri
             ORDER BY total_audible_ms DESC
             LIMIT ?"
                .to_string()
        } else {
            let group_uri = match kind {
                TopKind::Artists => "artist_uri",
                TopKind::Albums => "album_uri",
                _ => "track_uri",
            };
            format!(
                "SELECT
                lf.{group_uri} AS uri,
                COALESCE(mi.name, lf.{group_uri}) AS name,
                COALESCE(mi.subtitle, '') AS subtitle,
                COUNT(*) AS qualified_count,
                0 AS skip_count,
                SUM(lf.audible_ms) AS total_audible_ms,
                MAX(lf.started_at_ms) AS last_listened_at_ms
             FROM listen_facts lf
             LEFT JOIN media_items mi ON mi.uri = lf.{group_uri}
             LEFT JOIN media_items track_mi
                ON track_mi.uri = lf.track_uri
               AND track_mi.kind = 'track'
             WHERE lf.qualified = 1
               AND lf.started_at_ms >= ?
               AND lf.{group_uri} IS NOT NULL
               AND (? IS NULL OR COALESCE(
                    track_mi.provider,
                    mi.provider,
                    lower(substr(lf.track_uri, 1, instr(lf.track_uri, ':') - 1))
               ) = ?)
             GROUP BY lf.{group_uri}
             ORDER BY total_audible_ms DESC
             LIMIT ?"
            )
        };
        let rows = sqlx::query(&query)
            .bind(cutoff_ms)
            .bind(provider)
            .bind(provider)
            .bind(limit as i64)
            .fetch_all(&self.reader)
            .await?;

        Ok(rows
            .into_iter()
            .map(|row| TopEntry {
                uri: row.get::<String, _>("uri"),
                name: row.get::<String, _>("name"),
                subtitle: row.get::<String, _>("subtitle"),
                qualified_count: row.get::<i64, _>("qualified_count").max(0) as u32,
                skip_count: row.get::<i64, _>("skip_count").max(0) as u32,
                total_audible_ms: row.get::<i64, _>("total_audible_ms"),
                last_listened_at_ms: Some(row.get::<i64, _>("last_listened_at_ms")),
            })
            .collect())
    }

    /// Habit metrics. Reads `habit_metrics` rows for the given window,
    /// computing on demand when a bucket hasn't been materialised yet.
    /// The daily rollup job (P10.3) pre-populates buckets at local
    /// midnight to keep queries fast.
    pub async fn habit_buckets(
        &self,
        window: HabitWindow,
        since_ms: Option<i64>,
        provider: Option<&str>,
    ) -> Result<Vec<HabitBucket>> {
        let bucket_ms: i64 = match window {
            HabitWindow::Day => 86_400_000,
            HabitWindow::Week => 7 * 86_400_000,
            HabitWindow::Month => 30 * 86_400_000,
        };
        let since = since_ms.unwrap_or(0);

        // Compute on the fly from listen_facts. The dedicated
        // habit_metrics rollup is an optimisation, not a correctness
        // requirement: the live query always wins on freshness.
        let rows = sqlx::query(
            "WITH bucketed AS (
                SELECT
                    ((started_at_ms / ?) * ?) AS bucket_start_ms,
                    started_at_ms,
                    track_uri,
                    artist_uri,
                    session_id,
                    audible_ms
                FROM listen_facts lf
                LEFT JOIN media_items track_mi
                  ON track_mi.uri = lf.track_uri
                 AND track_mi.kind = 'track'
                WHERE lf.started_at_ms >= ?
                  AND (? IS NULL OR COALESCE(
                       track_mi.provider,
                       lower(substr(lf.track_uri, 1, instr(lf.track_uri, ':') - 1))
                  ) = ?)
             ),
             aggregate AS (
                SELECT
                    bucket_start_ms,
                    SUM(audible_ms) / 60000.0 AS minutes,
                    COUNT(*) AS total_listens,
                    COUNT(DISTINCT track_uri) AS unique_tracks,
                    COUNT(DISTINCT artist_uri) AS unique_artists,
                    COUNT(DISTINCT session_id) AS sessions
                FROM bucketed
                GROUP BY bucket_start_ms
             ),
             first_tracks AS (
                SELECT track_uri, MIN(started_at_ms) AS first_started_at_ms
                FROM listen_facts
                GROUP BY track_uri
             ),
             exploration AS (
                SELECT
                    b.bucket_start_ms,
                    COUNT(DISTINCT b.track_uri) AS new_unique_tracks
                FROM bucketed b
                JOIN first_tracks ft ON ft.track_uri = b.track_uri
                WHERE ft.first_started_at_ms >= b.bucket_start_ms
                  AND ft.first_started_at_ms < b.bucket_start_ms + ?
                GROUP BY b.bucket_start_ms
             ),
             hour_counts AS (
                SELECT
                    bucket_start_ms,
                    CAST(strftime('%H', started_at_ms / 1000, 'unixepoch') AS INTEGER) AS hour,
                    COUNT(*) AS listen_count
                FROM bucketed
                GROUP BY bucket_start_ms, hour
             ),
             top_hours AS (
                SELECT bucket_start_ms, hour
                FROM (
                    SELECT
                        bucket_start_ms,
                        hour,
                        ROW_NUMBER() OVER (
                            PARTITION BY bucket_start_ms
                            ORDER BY listen_count DESC, hour ASC
                        ) AS rn
                    FROM hour_counts
                )
                WHERE rn = 1
             )
             SELECT
                a.bucket_start_ms,
                a.minutes,
                a.total_listens,
                a.unique_tracks,
                a.unique_artists,
                a.sessions,
                th.hour AS top_hour_of_day,
                COALESCE(e.new_unique_tracks, 0) AS new_unique_tracks
             FROM aggregate a
             LEFT JOIN exploration e ON e.bucket_start_ms = a.bucket_start_ms
             LEFT JOIN top_hours th ON th.bucket_start_ms = a.bucket_start_ms
             ORDER BY a.bucket_start_ms ASC",
        )
        .bind(bucket_ms)
        .bind(bucket_ms)
        .bind(since)
        .bind(provider)
        .bind(provider)
        .bind(bucket_ms)
        .fetch_all(&self.reader)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| {
                let total_listens = row.get::<i64, _>("total_listens").max(0);
                let unique_tracks = row.get::<i64, _>("unique_tracks").max(0);
                let new_unique_tracks = row.get::<i64, _>("new_unique_tracks").max(0);
                let repeat_ratio = if total_listens > 0 {
                    (total_listens - unique_tracks).max(0) as f64 / total_listens as f64
                } else {
                    0.0
                };
                let exploration_ratio = if unique_tracks > 0 {
                    new_unique_tracks as f64 / unique_tracks as f64
                } else {
                    0.0
                };
                HabitBucket {
                    bucket: window,
                    bucket_start_ms: row.get::<i64, _>("bucket_start_ms"),
                    listening_minutes: row.get::<f64, _>("minutes"),
                    unique_tracks: unique_tracks as u32,
                    unique_artists: row.get::<i64, _>("unique_artists").max(0) as u32,
                    sessions: row.get::<i64, _>("sessions").max(0) as u32,
                    top_hour_of_day: row
                        .get::<Option<i64>, _>("top_hour_of_day")
                        .map(|h| h as u8),
                    exploration_ratio,
                    repeat_ratio,
                }
            })
            .collect())
    }

    /// Tracks worth re-discovering: qualified listen count > 0 and the
    /// last listen is older than `gap_days`. Returns the longest-dormant
    /// candidates first (largest `days_since_last_listen`).
    pub async fn rediscovery_candidates(
        &self,
        gap_days: u32,
        limit: u32,
        provider: Option<&str>,
    ) -> Result<Vec<RediscoveryCandidate>> {
        let now = spotuify_core::now_ms();
        let cutoff = now.saturating_sub((gap_days as i64).saturating_mul(86_400_000));
        let rows = sqlx::query(
            "SELECT
                lf.track_uri,
                COALESCE(mi.name, lf.track_uri) AS name,
                COALESCE(mi.subtitle, '') AS subtitle,
                COUNT(*) AS qualified_count,
                MAX(lf.started_at_ms) AS last_listened_at_ms
             FROM listen_facts lf
             LEFT JOIN media_items mi ON mi.uri = lf.track_uri
             WHERE lf.qualified = 1
               AND (? IS NULL OR COALESCE(
                    mi.provider,
                    lower(substr(lf.track_uri, 1, instr(lf.track_uri, ':') - 1))
               ) = ?)
             GROUP BY lf.track_uri
             HAVING last_listened_at_ms < ?
             ORDER BY last_listened_at_ms ASC, qualified_count DESC
             LIMIT ?",
        )
        .bind(provider)
        .bind(provider)
        .bind(cutoff)
        .bind(limit as i64)
        .fetch_all(&self.reader)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| {
                let last: i64 = row.get("last_listened_at_ms");
                let days = ((now - last) / 86_400_000).max(0) as u32;
                RediscoveryCandidate {
                    track_uri: row.get::<String, _>("track_uri"),
                    name: row.get::<String, _>("name"),
                    subtitle: row.get::<String, _>("subtitle"),
                    qualified_count: row.get::<i64, _>("qualified_count").max(0) as u32,
                    last_listened_at_ms: last,
                    days_since_last_listen: days,
                }
            })
            .collect())
    }

    /// Search history. Reads `analytics_events WHERE kind='search_performed'`
    /// (Phase 6 event log). Mode controls whether the raw query is
    /// returned or only the normalised hash.
    pub async fn search_history(
        &self,
        normalized_only: bool,
        limit: u32,
    ) -> Result<Vec<SearchHistoryEntry>> {
        let rows = sqlx::query(
            "SELECT
                search_query,
                search_query_hash,
                occurred_at_ms,
                payload_json
             FROM analytics_events
             WHERE kind = 'search_performed'
             ORDER BY occurred_at_ms DESC
             LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.reader)
        .await
        .unwrap_or_default();

        Ok(rows
            .into_iter()
            .map(|row| {
                let payload: String = row.try_get("payload_json").unwrap_or_default();
                let parsed: serde_json::Value =
                    serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null);
                let normalized = parsed
                    .get("normalized_query")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let result_count = parsed
                    .get("result_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                let raw_q: Option<String> = row.try_get("search_query").ok();
                let hash: String = row.try_get("search_query_hash").unwrap_or_default();
                SearchHistoryEntry {
                    query: if normalized_only { None } else { raw_q },
                    normalized,
                    query_hash: hash,
                    occurred_at_ms: row.try_get("occurred_at_ms").unwrap_or_default(),
                    result_count,
                    led_to_listen: false,
                }
            })
            .collect())
    }

    /// Wipe-and-rebuild path for `analytics rebuild`. Drops every
    /// `listen_facts` row (and zeroes the rollups), then walks the
    /// `analytics_events` log to recompute. Idempotent — running twice
    /// produces identical derived tables.
    /// Delete `playback_progress` rows older than `cutoff_ms`. Returns
    /// rows affected. Driven by the daemon retention job (default 90d).
    /// Routes through the bulk writer so retention pruning doesn't
    /// compete with hot-path commands.
    pub async fn prune_playback_progress(&self, cutoff_ms: i64) -> Result<u64> {
        let result = sqlx::query("DELETE FROM playback_progress WHERE sampled_at_ms < ?")
            .bind(cutoff_ms)
            .execute(self.bulk_writer())
            .await?;
        Ok(result.rows_affected())
    }

    /// Delete `analytics_events` rows older than `cutoff_ms`. Default
    /// retention 365d per blueprint; private-session rows are still
    /// subject to the same prune window.
    pub async fn prune_analytics_events(&self, cutoff_ms: i64) -> Result<u64> {
        let result = sqlx::query("DELETE FROM analytics_events WHERE occurred_at_ms < ?")
            .bind(cutoff_ms)
            .execute(self.bulk_writer())
            .await?;
        Ok(result.rows_affected())
    }

    pub async fn rebuild_derivations_from_events(
        &self,
        since_ms: Option<i64>,
    ) -> Result<RebuildReport> {
        let started = spotuify_core::now_ms();
        let cutoff = since_ms.unwrap_or(0);
        let mut tx = self.writer.begin().await?;
        sqlx::query("DELETE FROM listen_facts WHERE started_at_ms >= ?")
            .bind(cutoff)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE track_metrics SET qualified_count = 0, total_audible_ms = 0")
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE artist_metrics SET qualified_count = 0, total_audible_ms = 0")
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE album_metrics SET qualified_count = 0, total_audible_ms = 0")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        // Stream playback_completed events and synthesise listen_facts.
        // This is intentionally light-touch in the foundation; the full
        // SessionTracker replay (P10.3 follow-up) reconstructs the full
        // pause/resume timeline.
        let rows = sqlx::query(
            "SELECT subject_uri, occurred_at_ms, payload_json
             FROM analytics_events
             WHERE kind = 'playback_completed'
               AND occurred_at_ms >= ?
             ORDER BY occurred_at_ms ASC",
        )
        .bind(cutoff)
        .fetch_all(&self.reader)
        .await
        .unwrap_or_default();

        let mut events_processed = 0u64;
        let mut emitted = 0u64;
        let mut qualified = 0u64;
        for row in rows {
            events_processed += 1;
            let uri: Option<String> = row.try_get("subject_uri").ok();
            let Some(track_uri) = uri else { continue };
            let occurred: i64 = row.try_get("occurred_at_ms").unwrap_or_default();
            let payload: String = row.try_get("payload_json").unwrap_or_default();
            let parsed: serde_json::Value =
                serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null);
            let audible_ms = parsed
                .get("audible_ms")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let elapsed_ms = parsed
                .get("elapsed_ms")
                .and_then(|v| v.as_i64())
                .unwrap_or(audible_ms);
            let qualified_event = parsed
                .get("qualified")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let private_session = parsed
                .get("private_session")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let fact = ListenFact {
                id: None,
                session_id: format!("rebuild-{occurred}"),
                track_uri: track_uri.clone(),
                artist_uri: None,
                album_uri: None,
                // Rebuild derives from raw analytics_events, which don't
                // carry playback context; playlist-level top-k only counts
                // live-captured contexts.
                context_uri: None,
                started_at_ms: occurred - elapsed_ms,
                ended_at_ms: occurred,
                duration_ms: elapsed_ms,
                elapsed_ms,
                audible_ms,
                completion_ratio: if elapsed_ms > 0 {
                    audible_ms as f64 / elapsed_ms as f64
                } else {
                    0.0
                },
                qualified: qualified_event,
                qualification_rule_version: spotuify_core::QUALIFICATION_RULE_VERSION,
                skip_reason: None,
                source: None,
                backend: None,
                private_session,
                measurement_kind: MeasurementKind::ObservedPlayback,
                external_scrobble_id: None,
                created_at_ms: occurred,
            };
            self.insert_listen_fact(&fact).await?;
            self.upsert_track_metric(&track_uri, qualified_event, audible_ms, occurred)
                .await?;
            emitted += 1;
            if qualified_event {
                qualified += 1;
            }
        }

        Ok(RebuildReport {
            events_processed,
            listen_facts_emitted: emitted,
            qualified_listens: qualified,
            elapsed_ms: (spotuify_core::now_ms() - started) as u128,
        })
    }
}

fn metric_rebuild_sql(table: &str, pk: &str, fact_column: &str) -> String {
    format!(
        "INSERT INTO {table} (
            {pk}, qualified_count, skip_count, total_audible_ms,
            last_listened_at_ms, first_listened_at_ms, updated_at_ms
         )
         SELECT
            {fact_column},
            SUM(CASE WHEN qualified != 0 THEN 1 ELSE 0 END),
            SUM(CASE WHEN qualified = 0 THEN 1 ELSE 0 END),
            SUM(audible_ms),
            MAX(started_at_ms),
            MIN(started_at_ms),
            ?
         FROM listen_facts
         WHERE {fact_column} IS NOT NULL
         GROUP BY {fact_column}"
    )
}

async fn unique_media_uri_by_kind_and_name(
    reader: &sqlx::SqlitePool,
    kind: &str,
    name: &str,
    provider: &str,
) -> Result<Option<String>> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let rows = sqlx::query(
        "SELECT uri FROM media_items
         WHERE kind = ? AND LOWER(name) = LOWER(?) AND provider = ?
         ORDER BY updated_at_ms DESC
         LIMIT 2",
    )
    .bind(kind)
    .bind(trimmed)
    .bind(provider)
    .fetch_all(reader)
    .await?;
    if rows.len() == 1 {
        Ok(Some(rows[0].get("uri")))
    } else {
        Ok(None)
    }
}

async fn upsert_entity_metric(
    writer: &sqlx::SqlitePool,
    table: &str,
    pk: &str,
    uri: &str,
    qualified: bool,
    audible_ms: i64,
    finalized_at_ms: i64,
) -> Result<()> {
    // SQLite's UPSERT (ON CONFLICT … DO UPDATE) lands the increment
    // in one statement. The `excluded.*` references see the values
    // we tried to INSERT, so we can keep the SQL terse + literal.
    let sql = format!(
        "INSERT INTO {table} (
            {pk}, qualified_count, skip_count, total_audible_ms,
            last_listened_at_ms, first_listened_at_ms, updated_at_ms
         ) VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT({pk}) DO UPDATE SET
            qualified_count = qualified_count + excluded.qualified_count,
            skip_count      = skip_count + excluded.skip_count,
            total_audible_ms = total_audible_ms + excluded.total_audible_ms,
            last_listened_at_ms = MAX(last_listened_at_ms, excluded.last_listened_at_ms),
            updated_at_ms = excluded.updated_at_ms"
    );
    let qual_count = if qualified { 1_i64 } else { 0 };
    let skip_count = if qualified { 0_i64 } else { 1 };
    sqlx::query(&sql)
        .bind(uri)
        .bind(qual_count)
        .bind(skip_count)
        .bind(audible_ms)
        .bind(finalized_at_ms)
        .bind(finalized_at_ms)
        .bind(finalized_at_ms)
        .execute(writer)
        .await?;
    Ok(())
}
