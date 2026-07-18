//! Last.fm historical import persistence.

use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::Row;

use spotuify_core::{now_ms, ItemSource, MediaKind, ResourceUri};
use spotuify_protocol::{
    AnalyticsImportRunStatus, AnalyticsImportSummary, AnalyticsImportUndoSummary,
    UnresolvedScrobble,
};

use crate::Store;

#[derive(Clone, Debug)]
pub struct NewExternalScrobble {
    pub provider: String,
    pub username: String,
    pub import_run_id: String,
    pub idempotency_key: String,
    pub scrobbled_at_ms: i64,
    pub artist_name: String,
    pub track_name: String,
    pub album_name: Option<String>,
    pub artist_mbid: Option<String>,
    pub track_mbid: Option<String>,
    pub album_mbid: Option<String>,
    pub url: Option<String>,
    pub raw_json: Value,
    pub normalized_key: String,
}

#[derive(Clone, Debug)]
pub struct StoredExternalScrobble {
    pub id: i64,
    pub duplicate: bool,
}

#[derive(Clone, Debug)]
pub struct PlaybackProgressSample<'a> {
    pub session_id: &'a str,
    pub track_uri: &'a str,
    pub sampled_at_ms: i64,
    pub position_ms: i64,
    pub audible_samples: i64,
    pub sample_rate: i64,
    pub channels: i64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ImportRunFinalCounts {
    pub fetched: u64,
    pub stored: u64,
    pub duplicates: u64,
    pub resolved: u64,
    pub promoted: u64,
    pub unresolved: u64,
}

impl Store {
    pub async fn create_import_run(
        &self,
        run_id: &str,
        provider: &str,
        username: &str,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
        dry_run: bool,
    ) -> Result<()> {
        let now = now_ms();
        sqlx::query(
            "INSERT INTO analytics_import_runs (
                run_id, provider, username, from_ms, to_ms, state, dry_run, started_at_ms
             ) VALUES (?, ?, ?, ?, ?, 'running', ?, ?)",
        )
        .bind(run_id)
        .bind(provider)
        .bind(username)
        .bind(from_ms)
        .bind(to_ms)
        .bind(dry_run as i64)
        .bind(now)
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    pub async fn finish_import_run(
        &self,
        run_id: &str,
        state: &str,
        counts: ImportRunFinalCounts,
        cursor: Option<&str>,
        error: Option<&str>,
    ) -> Result<AnalyticsImportSummary> {
        let finished = now_ms();
        sqlx::query(
            "UPDATE analytics_import_runs
             SET state = ?, fetched = ?, stored = ?, duplicates = ?, resolved = ?, promoted = ?,
                 unresolved = ?, cursor = ?, error = ?, finished_at_ms = ?
             WHERE run_id = ?",
        )
        .bind(state)
        .bind(counts.fetched as i64)
        .bind(counts.stored as i64)
        .bind(counts.duplicates as i64)
        .bind(counts.resolved as i64)
        .bind(counts.promoted as i64)
        .bind(counts.unresolved as i64)
        .bind(cursor)
        .bind(error)
        .bind(finished)
        .bind(run_id)
        .execute(&self.writer)
        .await?;
        let status = self.import_run_status(run_id).await?;
        Ok(AnalyticsImportSummary {
            run_id: status.run_id,
            provider: status.provider,
            username: status.username,
            dry_run: status.dry_run,
            fetched: status.fetched,
            stored: status.stored,
            duplicates: status.duplicates,
            resolved: status.resolved,
            promoted: status.promoted,
            unresolved: status.unresolved,
            started_at_ms: status.started_at_ms,
            finished_at_ms: status.finished_at_ms,
        })
    }

    pub async fn import_run_status(&self, run_id: &str) -> Result<AnalyticsImportRunStatus> {
        let row = sqlx::query(
            "SELECT run_id, provider, username, state, dry_run, from_ms, to_ms, fetched, stored,
                    duplicates, resolved, promoted, unresolved, cursor, started_at_ms, finished_at_ms
             FROM analytics_import_runs WHERE run_id = ?",
        )
        .bind(run_id)
        .fetch_one(&self.reader)
        .await?;
        Ok(AnalyticsImportRunStatus {
            run_id: row.get("run_id"),
            provider: row.get("provider"),
            username: row.get("username"),
            state: row.get("state"),
            dry_run: row.get::<i64, _>("dry_run") != 0,
            from_ms: row.get("from_ms"),
            to_ms: row.get("to_ms"),
            fetched: row.get::<i64, _>("fetched") as u64,
            stored: row.get::<i64, _>("stored") as u64,
            duplicates: row.get::<i64, _>("duplicates") as u64,
            resolved: row.get::<i64, _>("resolved") as u64,
            promoted: row.get::<i64, _>("promoted") as u64,
            unresolved: row.get::<i64, _>("unresolved") as u64,
            cursor: row.get("cursor"),
            started_at_ms: row.get("started_at_ms"),
            finished_at_ms: row.get("finished_at_ms"),
        })
    }

    pub async fn insert_external_scrobble(
        &self,
        scrobble: &NewExternalScrobble,
    ) -> Result<StoredExternalScrobble> {
        let now = now_ms();
        let raw_json = serde_json::to_string(&scrobble.raw_json)?;
        let result = sqlx::query(
            "INSERT OR IGNORE INTO external_scrobbles (
                provider, username, import_run_id, idempotency_key, scrobbled_at_ms,
                artist_name, track_name, album_name, artist_mbid, track_mbid, album_mbid, url,
                raw_json, normalized_key, resolution_status, created_at_ms, updated_at_ms
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?)",
        )
        .bind(&scrobble.provider)
        .bind(&scrobble.username)
        .bind(&scrobble.import_run_id)
        .bind(&scrobble.idempotency_key)
        .bind(scrobble.scrobbled_at_ms)
        .bind(&scrobble.artist_name)
        .bind(&scrobble.track_name)
        .bind(&scrobble.album_name)
        .bind(&scrobble.artist_mbid)
        .bind(&scrobble.track_mbid)
        .bind(&scrobble.album_mbid)
        .bind(&scrobble.url)
        .bind(raw_json)
        .bind(&scrobble.normalized_key)
        .bind(now)
        .bind(now)
        .execute(&self.writer)
        .await?;

        if result.rows_affected() == 1 {
            return Ok(StoredExternalScrobble {
                id: result.last_insert_rowid(),
                duplicate: false,
            });
        }

        let id: i64 = sqlx::query_scalar(
            "SELECT id FROM external_scrobbles
             WHERE provider = ? AND username = ? AND idempotency_key = ?",
        )
        .bind(&scrobble.provider)
        .bind(&scrobble.username)
        .bind(&scrobble.idempotency_key)
        .fetch_one(&self.reader)
        .await?;
        Ok(StoredExternalScrobble {
            id,
            duplicate: true,
        })
    }

    pub async fn mark_external_scrobble_resolution(
        &self,
        id: i64,
        status: &str,
        resolved_uri: Option<&str>,
        confidence: Option<f64>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE external_scrobbles
             SET resolution_status = ?, resolved_uri = ?, confidence = ?, updated_at_ms = ?
             WHERE id = ?",
        )
        .bind(status)
        .bind(resolved_uri)
        .bind(confidence)
        .bind(now_ms())
        .bind(id)
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    pub async fn exact_track_match(
        &self,
        artist: &str,
        track: &str,
        album: Option<&str>,
        provider: Option<&str>,
    ) -> Result<Option<spotuify_core::MediaItem>> {
        let artist_norm = normalize_for_match(artist);
        let track_norm = normalize_for_match(track);
        let album_norm = album.map(normalize_for_match);
        let rows = sqlx::query(
            "SELECT uri, kind, name, subtitle, context, duration_ms,
                    image_url, search_origin, liked, saved, updated_at_ms
             FROM media_items
             WHERE kind = 'track' AND LOWER(name) = ?
               AND (? IS NULL OR provider = ?)
             ORDER BY saved DESC, liked DESC, updated_at_ms DESC
             LIMIT 10",
        )
        .bind(&track_norm)
        .bind(provider)
        .bind(provider)
        .fetch_all(&self.reader)
        .await?;

        let mut matches = Vec::new();
        for row in rows {
            let item = row_to_media_item_public(row)?;
            let haystack = normalize_for_match(&format!("{} {}", item.subtitle, item.context));
            let album_ok = album_norm
                .as_ref()
                .is_none_or(|needle| haystack.contains(needle));
            if haystack.contains(&artist_norm) && album_ok {
                matches.push(item);
            }
        }
        Ok((matches.len() == 1).then(|| matches.remove(0)))
    }

    pub async fn unresolved_scrobbles(&self, run_id: &str) -> Result<Vec<UnresolvedScrobble>> {
        let rows = sqlx::query(
            "SELECT id, scrobbled_at_ms, artist_name, track_name, album_name, url,
                    resolution_status, confidence
             FROM external_scrobbles
             WHERE import_run_id = ? AND resolution_status NOT IN ('resolved', 'promoted', 'duplicate')
             ORDER BY scrobbled_at_ms DESC",
        )
        .bind(run_id)
        .fetch_all(&self.reader)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(UnresolvedScrobble {
                    id: row.get("id"),
                    scrobbled_at_ms: row.get("scrobbled_at_ms"),
                    artist: row.get("artist_name"),
                    track: row.get("track_name"),
                    album: row.get("album_name"),
                    url: row.get("url"),
                    resolution_status: row.get("resolution_status"),
                    confidence: row.get("confidence"),
                })
            })
            .collect()
    }

    pub async fn undo_import_run(
        &self,
        run_id: &str,
        dry_run: bool,
    ) -> Result<AnalyticsImportUndoSummary> {
        let fact_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM listen_facts
             WHERE external_scrobble_id IN (SELECT id FROM external_scrobbles WHERE import_run_id = ?)",
        )
        .bind(run_id)
        .fetch_one(&self.reader)
        .await?;
        let raw_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM external_scrobbles WHERE import_run_id = ?")
                .bind(run_id)
                .fetch_one(&self.reader)
                .await?;
        if !dry_run {
            sqlx::query(
                "DELETE FROM listen_facts
                 WHERE external_scrobble_id IN (SELECT id FROM external_scrobbles WHERE import_run_id = ?)",
            )
            .bind(run_id)
            .execute(&self.writer)
            .await?;
            self.rebuild_metric_rollups_from_listen_facts().await?;
            sqlx::query("UPDATE analytics_import_runs SET state = 'undone' WHERE run_id = ?")
                .bind(run_id)
                .execute(&self.writer)
                .await?;
        }
        Ok(AnalyticsImportUndoSummary {
            run_id: run_id.to_string(),
            dry_run,
            listen_facts_removed: fact_count.max(0) as u64,
            raw_scrobbles_preserved: raw_count.max(0) as u64,
        })
    }

    pub async fn count_listen_facts_for_external(&self, external_id: i64) -> Result<u64> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM listen_facts WHERE external_scrobble_id = ?")
                .bind(external_id)
                .fetch_one(&self.reader)
                .await?;
        Ok(count.max(0) as u64)
    }

    pub async fn insert_playback_progress_sample(
        &self,
        sample: &PlaybackProgressSample<'_>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO playback_progress (
                session_id, track_uri, sampled_at_ms, position_ms,
                audible_samples, sample_rate, channels
             ) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(sample.session_id)
        .bind(sample.track_uri)
        .bind(sample.sampled_at_ms)
        .bind(sample.position_ms)
        .bind(sample.audible_samples)
        .bind(sample.sample_rate)
        .bind(sample.channels)
        .execute(&self.writer)
        .await?;
        Ok(())
    }
}

fn normalize_for_match(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn row_to_media_item_public(row: sqlx::sqlite::SqliteRow) -> Result<spotuify_core::MediaItem> {
    let uri: String = row.get("uri");
    let resource = ResourceUri::parse(&uri)
        .with_context(|| format!("cached media row has invalid URI `{uri}`"))?;
    let kind = media_kind_from_label_local(&row.get::<String, _>("kind"))?;
    if resource.kind() != kind {
        anyhow::bail!(
            "cached media URI kind `{}` does not match row kind `{kind}` for `{uri}`",
            resource.kind()
        );
    }
    Ok(spotuify_core::MediaItem {
        id: Some(resource.bare_id().to_string()),
        uri,
        name: row.get("name"),
        subtitle: row.get("subtitle"),
        context: row.get("context"),
        duration_ms: row.get::<i64, _>("duration_ms").max(0) as u64,
        image_url: row.get("image_url"),
        kind,
        source: Some(ItemSource::from(row.get::<String, _>("search_origin"))),
        freshness: Some("cached".to_string()),
        explicit: None,
        is_playable: None,
        ..Default::default()
    })
}

fn media_kind_from_label_local(label: &str) -> Result<MediaKind> {
    match label {
        "track" => Ok(MediaKind::Track),
        "episode" => Ok(MediaKind::Episode),
        "show" => Ok(MediaKind::Show),
        "album" => Ok(MediaKind::Album),
        "artist" => Ok(MediaKind::Artist),
        "playlist" => Ok(MediaKind::Playlist),
        _ => anyhow::bail!("unknown media kind `{label}`"),
    }
}
