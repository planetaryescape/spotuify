//! Last.fm historical import orchestration.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::StatusCode;
use serde_json::Value;
use sha2::{Digest, Sha256};
use spotuify_core::{
    now_ms, qualify_listen, ListenFact, MeasurementKind, MediaItem, MediaKind, PlaybackSource,
    SkipReason, QUALIFICATION_RULE_VERSION,
};
use spotuify_protocol::{
    AnalyticsImportRunStatus, AnalyticsImportSummary, AnalyticsImportUndoSummary, DaemonEvent,
    ExportTarget, ResponseData, SearchScopeData, UnresolvedScrobble,
};
use spotuify_store::{ImportRunFinalCounts, NewExternalScrobble, Store};
use uuid::Uuid;

use crate::state::DaemonState;

const LASTFM_PROVIDER: &str = "lastfm";
const LASTFM_DEFAULT_BASE_URL: &str = "https://ws.audioscrobbler.com/2.0/";
const LASTFM_PAGE_LIMIT: u32 = 200;
const LASTFM_RETRIES: usize = 3;

#[derive(Clone, Debug)]
pub(crate) struct LastfmImportRequest {
    pub target: ExportTarget,
    pub username: Option<String>,
    pub api_key: Option<String>,
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    pub apply: bool,
}

pub(crate) async fn import_lastfm(
    state: std::sync::Arc<DaemonState>,
    request: LastfmImportRequest,
) -> Result<ResponseData> {
    if request.target != ExportTarget::LastFm {
        bail!("unsupported analytics import target; only lastfm is implemented");
    }
    let config = spotuify_spotify::config::Config::load().ok();
    let username = request
        .username
        .or_else(|| std::env::var("SPOTUIFY_LASTFM_USER").ok())
        .or_else(|| config.as_ref().and_then(|c| c.analytics.lastfm_user.clone()))
        .context("Last.fm username required: pass --user or set analytics.lastfm_user/SPOTUIFY_LASTFM_USER")?;
    let api_key = request
        .api_key
        .or_else(|| std::env::var("SPOTUIFY_LASTFM_API_KEY").ok())
        .or_else(|| {
            config
                .as_ref()
                .and_then(|c| c.analytics.lastfm_api_key.clone())
        })
        .context(
            "Last.fm API key required: set SPOTUIFY_LASTFM_API_KEY or analytics.lastfm_api_key",
        )?;

    let run_id = Uuid::now_v7().to_string();
    let dry_run = !request.apply;
    if !dry_run {
        state
            .store()
            .create_import_run(
                &run_id,
                LASTFM_PROVIDER,
                &username,
                request.from_ms,
                request.to_ms,
                dry_run,
            )
            .await?;
    }

    let client = LastfmClient::from_env_or_default(api_key);
    let mut counts = ImportCounts::default();
    let mut fetched_so_far = 0_u64;
    emit_import_progress(&state, &run_id, &username, "started", counts, None);
    let fetched = match client
        .recent_tracks_with_progress(
            &username,
            request.from_ms,
            request.to_ms,
            |page, total, rows| {
                fetched_so_far = fetched_so_far.saturating_add(rows as u64);
                emit_import_progress(
                    &state,
                    &run_id,
                    &username,
                    "fetch-page",
                    ImportCounts {
                        fetched: fetched_so_far,
                        ..counts
                    },
                    Some(format!("fetched page {page}/{total}")),
                );
            },
        )
        .await
    {
        Ok(fetched) => fetched,
        Err(err) => {
            emit_import_progress(
                &state,
                &run_id,
                &username,
                "failed",
                counts,
                Some(err.to_string()),
            );
            if !dry_run {
                let _ = state
                    .store()
                    .finish_import_run(
                        &run_id,
                        "failed",
                        counts.into(),
                        None,
                        Some(&err.to_string()),
                    )
                    .await;
            }
            return Err(err);
        }
    };

    counts.fetched = fetched.len() as u64;
    emit_import_progress(&state, &run_id, &username, "fetched", counts, None);

    for (index, scrobble) in fetched.into_iter().enumerate() {
        let idempotency_key = idempotency_key(&username, &scrobble);
        let normalized_key = normalized_scrobble_key(&scrobble);
        let stored_id = if dry_run {
            None
        } else {
            let stored = state
                .store()
                .insert_external_scrobble(&NewExternalScrobble {
                    provider: LASTFM_PROVIDER.to_string(),
                    username: username.clone(),
                    import_run_id: run_id.clone(),
                    idempotency_key,
                    scrobbled_at_ms: scrobble.scrobbled_at_ms,
                    artist_name: scrobble.artist.clone(),
                    track_name: scrobble.track.clone(),
                    album_name: scrobble.album.clone(),
                    artist_mbid: scrobble.artist_mbid.clone(),
                    track_mbid: scrobble.track_mbid.clone(),
                    album_mbid: scrobble.album_mbid.clone(),
                    url: scrobble.url.clone(),
                    raw_json: scrobble.raw.clone(),
                    normalized_key,
                })
                .await?;
            if stored.duplicate {
                counts.duplicates += 1;
            } else {
                counts.stored += 1;
            }
            Some(stored.id)
        };

        let resolution = resolve_scrobble(&state, &scrobble).await;
        match resolution {
            Ok(Some((item, confidence))) => {
                counts.resolved += 1;
                if let Some(external_id) = stored_id {
                    state
                        .store()
                        .mark_external_scrobble_resolution(
                            external_id,
                            "resolved",
                            Some(&item.uri),
                            Some(confidence),
                        )
                        .await?;
                    if promote_imported_listen(state.store(), external_id, &scrobble, &item).await?
                    {
                        counts.promoted += 1;
                        state
                            .store()
                            .mark_external_scrobble_resolution(
                                external_id,
                                "promoted",
                                Some(&item.uri),
                                Some(confidence),
                            )
                            .await?;
                    }
                }
            }
            Ok(None) => {
                counts.unresolved += 1;
                if let Some(external_id) = stored_id {
                    state
                        .store()
                        .mark_external_scrobble_resolution(external_id, "unresolved", None, None)
                        .await?;
                }
            }
            Err(err) => {
                counts.unresolved += 1;
                if let Some(external_id) = stored_id {
                    state
                        .store()
                        .mark_external_scrobble_resolution(external_id, "resolve_error", None, None)
                        .await?;
                }
                tracing::debug!(error = %err, artist = %scrobble.artist, track = %scrobble.track, "Last.fm scrobble resolution failed");
            }
        }
        if index % 25 == 0 || index + 1 == counts.fetched as usize {
            emit_import_progress(&state, &run_id, &username, "processing", counts, None);
        }
    }

    let summary = if dry_run {
        AnalyticsImportSummary {
            run_id: run_id.clone(),
            provider: LASTFM_PROVIDER.to_string(),
            username: username.clone(),
            dry_run,
            fetched: counts.fetched,
            stored: 0,
            duplicates: 0,
            resolved: counts.resolved,
            promoted: 0,
            unresolved: counts.unresolved,
            started_at_ms: now_ms(),
            finished_at_ms: Some(now_ms()),
        }
    } else {
        state
            .store()
            .finish_import_run(
                &run_id,
                "completed",
                ImportRunFinalCounts {
                    fetched: counts.fetched,
                    stored: counts.stored,
                    duplicates: counts.duplicates,
                    resolved: counts.resolved,
                    promoted: counts.promoted,
                    unresolved: counts.unresolved,
                },
                None,
                None,
            )
            .await?
    };
    emit_import_progress(&state, &run_id, &username, "completed", counts, None);
    Ok(ResponseData::AnalyticsImportSummary { summary })
}

pub(crate) async fn import_status(store: &Store, run_id: String) -> Result<ResponseData> {
    let status: AnalyticsImportRunStatus = store.import_run_status(&run_id).await?;
    Ok(ResponseData::AnalyticsImportRunStatus { status })
}

pub(crate) async fn import_unresolved(store: &Store, run_id: String) -> Result<ResponseData> {
    let entries: Vec<UnresolvedScrobble> = store.unresolved_scrobbles(&run_id).await?;
    Ok(ResponseData::AnalyticsImportUnresolved { entries })
}

pub(crate) async fn import_undo(
    store: &Store,
    run_id: String,
    dry_run: bool,
    force: bool,
) -> Result<ResponseData> {
    if !dry_run && !force {
        bail!("import undo requires --yes (or use --dry-run to preview)");
    }
    let summary: AnalyticsImportUndoSummary = store.undo_import_run(&run_id, dry_run).await?;
    Ok(ResponseData::AnalyticsImportUndoSummary { summary })
}

#[derive(Clone, Copy, Debug, Default)]
struct ImportCounts {
    fetched: u64,
    stored: u64,
    duplicates: u64,
    resolved: u64,
    promoted: u64,
    unresolved: u64,
}

impl From<ImportCounts> for ImportRunFinalCounts {
    fn from(counts: ImportCounts) -> Self {
        Self {
            fetched: counts.fetched,
            stored: counts.stored,
            duplicates: counts.duplicates,
            resolved: counts.resolved,
            promoted: counts.promoted,
            unresolved: counts.unresolved,
        }
    }
}

fn emit_import_progress(
    state: &DaemonState,
    run_id: &str,
    username: &str,
    phase: &str,
    counts: ImportCounts,
    message: Option<String>,
) {
    state.emit_event(DaemonEvent::AnalyticsImportProgress {
        run_id: run_id.to_string(),
        provider: LASTFM_PROVIDER.to_string(),
        username: username.to_string(),
        phase: phase.to_string(),
        fetched: counts.fetched,
        stored: counts.stored,
        resolved: counts.resolved,
        promoted: counts.promoted,
        unresolved: counts.unresolved,
        message,
    });
}

async fn resolve_scrobble(
    state: &std::sync::Arc<DaemonState>,
    scrobble: &LastfmScrobble,
) -> Result<Option<(MediaItem, f64)>> {
    if let Some(item) = state
        .store()
        .exact_track_match(&scrobble.artist, &scrobble.track, scrobble.album.as_deref())
        .await?
    {
        return Ok(Some((item, 1.0)));
    }

    let query = format!("{} {}", scrobble.track, scrobble.artist);
    let local = state
        .store()
        .local_search(&query, SearchScopeData::Track, 5)
        .await?;
    if let Some(item) = single_high_confidence(local, scrobble) {
        return Ok(Some((item, 0.96)));
    }

    let spotify = match state.spotify_client().await {
        Ok(client) => match tokio::time::timeout(
            Duration::from_secs(10),
            client.search_with_limit(&query, &[MediaKind::Track], 5),
        )
        .await
        {
            Ok(Ok(items)) => items,
            Ok(Err(err)) => {
                tracing::debug!(error = %err, "Spotify fallback search failed during Last.fm import");
                Vec::new()
            }
            Err(_) => Vec::new(),
        },
        Err(err) => {
            tracing::debug!(error = %err, "Spotify client unavailable during Last.fm import");
            Vec::new()
        }
    };
    if let Some(item) = single_high_confidence(spotify, scrobble) {
        state
            .store()
            .upsert_media_items(std::slice::from_ref(&item), "spotify")
            .await
            .ok();
        return Ok(Some((item, 0.92)));
    }

    Ok(None)
}

fn single_high_confidence(items: Vec<MediaItem>, scrobble: &LastfmScrobble) -> Option<MediaItem> {
    let mut candidates = items
        .into_iter()
        .filter(|item| item.kind == MediaKind::Track)
        .filter(|item| {
            normalize(&item.name) == normalize(&scrobble.track)
                && normalize(&format!("{} {}", item.subtitle, item.context))
                    .contains(&normalize(&scrobble.artist))
        })
        .collect::<Vec<_>>();
    (candidates.len() == 1).then(|| candidates.remove(0))
}

async fn promote_imported_listen(
    store: &Store,
    external_id: i64,
    scrobble: &LastfmScrobble,
    item: &MediaItem,
) -> Result<bool> {
    if store.count_listen_facts_for_external(external_id).await? > 0 {
        return Ok(false);
    }
    let duration_ms = (item.duration_ms as i64).max(0);
    if duration_ms <= 30_000 {
        return Ok(false);
    }
    let lower_bound_ms = qualify_listen(duration_ms, i64::MAX).threshold_ms;
    let ended_at_ms = scrobble.scrobbled_at_ms;
    let started_at_ms = ended_at_ms.saturating_sub(lower_bound_ms);
    let (artist_uri, album_uri) = store
        .listen_context_uris(&item.uri)
        .await
        .unwrap_or((None, None));
    let fact = ListenFact {
        id: None,
        session_id: format!("lastfm-import-{external_id}"),
        track_uri: item.uri.clone(),
        artist_uri,
        album_uri,
        context_uri: None,
        started_at_ms,
        ended_at_ms,
        duration_ms,
        elapsed_ms: lower_bound_ms,
        audible_ms: lower_bound_ms,
        completion_ratio: if duration_ms > 0 {
            (lower_bound_ms as f64 / duration_ms as f64).clamp(0.0, 1.0)
        } else {
            0.0
        },
        qualified: true,
        qualification_rule_version: QUALIFICATION_RULE_VERSION,
        skip_reason: Some(SkipReason::TrackEnd),
        source: Some(PlaybackSource::Unknown),
        backend: None,
        private_session: false,
        measurement_kind: MeasurementKind::LastfmScrobbleImport,
        external_scrobble_id: Some(external_id),
        created_at_ms: now_ms(),
    };
    store.insert_listen_fact(&fact).await?;
    store
        .upsert_track_metric(
            &fact.track_uri,
            fact.qualified,
            fact.audible_ms,
            fact.ended_at_ms,
        )
        .await?;
    if let Some(artist_uri) = fact.artist_uri.as_deref() {
        store
            .upsert_artist_metric(
                artist_uri,
                fact.qualified,
                fact.audible_ms,
                fact.ended_at_ms,
            )
            .await?;
    }
    if let Some(album_uri) = fact.album_uri.as_deref() {
        store
            .upsert_album_metric(album_uri, fact.qualified, fact.audible_ms, fact.ended_at_ms)
            .await?;
    }
    Ok(true)
}

#[derive(Clone, Debug)]
struct LastfmClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl LastfmClient {
    fn from_env_or_default(api_key: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key,
            base_url: std::env::var("SPOTUIFY_LASTFM_API_BASE_URL")
                .unwrap_or_else(|_| LASTFM_DEFAULT_BASE_URL.to_string()),
        }
    }

    async fn recent_tracks_with_progress<F>(
        &self,
        username: &str,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
        mut on_page: F,
    ) -> Result<Vec<LastfmScrobble>>
    where
        F: FnMut(u32, u32, usize),
    {
        let mut page = 1_u32;
        let mut total_pages = 1_u32;
        let mut out = Vec::new();
        while page <= total_pages {
            let response = self.fetch_page(username, from_ms, to_ms, page).await?;
            total_pages = response.total_pages.max(1);
            let before = out.len();
            out.extend(
                response
                    .tracks
                    .into_iter()
                    .filter(|track| !track.now_playing),
            );
            on_page(page, total_pages, out.len().saturating_sub(before));
            page += 1;
        }
        Ok(out)
    }

    async fn fetch_page(
        &self,
        username: &str,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
        page: u32,
    ) -> Result<LastfmPage> {
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..LASTFM_RETRIES {
            let mut req = self
                .http
                .get(&self.base_url)
                .query(&[
                    ("method", "user.getRecentTracks"),
                    ("user", username),
                    ("api_key", &self.api_key),
                    ("format", "json"),
                    ("extended", "1"),
                ])
                .query(&[("limit", LASTFM_PAGE_LIMIT), ("page", page)]);
            if let Some(from) = from_ms {
                req = req.query(&[("from", from / 1000)]);
            }
            if let Some(to) = to_ms {
                req = req.query(&[("to", to / 1000)]);
            }
            let result = tokio::time::timeout(Duration::from_secs(20), req.send()).await;
            match result {
                Ok(Ok(resp)) => match parse_lastfm_response(resp).await {
                    Ok(page) => return Ok(page),
                    Err(err) => last_err = Some(err),
                },
                Ok(Err(err)) => last_err = Some(err.into()),
                Err(_) => last_err = Some(anyhow::anyhow!("Last.fm request timed out")),
            }
            let sleep_ms = 250_u64.saturating_mul(1 << attempt);
            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Last.fm request failed")))
    }
}

async fn parse_lastfm_response(resp: reqwest::Response) -> Result<LastfmPage> {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        bail!("retryable Last.fm HTTP {status}: {body}");
    }
    let json: Value = serde_json::from_str(&body).context("malformed Last.fm JSON")?;
    if let Some(error_code) = json.get("error").and_then(|v| v.as_i64()) {
        let message = json
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Last.fm API error");
        if error_code == 29 {
            bail!("Last.fm rate limited (29): {message}");
        }
        bail!("Last.fm API error {error_code}: {message}");
    }
    if !status.is_success() {
        bail!("Last.fm HTTP {status}: {body}");
    }
    LastfmPage::from_json(json)
}

#[derive(Debug)]
struct LastfmPage {
    total_pages: u32,
    tracks: Vec<LastfmScrobble>,
}

impl LastfmPage {
    fn from_json(json: Value) -> Result<Self> {
        let recent = json
            .get("recenttracks")
            .context("Last.fm response missing recenttracks")?;
        let attrs = recent.get("@attr").unwrap_or(&Value::Null);
        let total_pages = attrs
            .get("totalPages")
            .or_else(|| attrs.get("totalpages"))
            .and_then(value_to_u32)
            .unwrap_or(1);
        let track_value = recent
            .get("track")
            .cloned()
            .unwrap_or(Value::Array(Vec::new()));
        let rows = match track_value {
            Value::Array(rows) => rows,
            Value::Object(_) => vec![track_value],
            _ => Vec::new(),
        };
        let mut tracks = Vec::new();
        for row in rows {
            match LastfmScrobble::from_json(row) {
                Ok(track) => tracks.push(track),
                Err(err) => tracing::debug!(error = %err, "skipping malformed Last.fm scrobble"),
            }
        }
        Ok(Self {
            total_pages,
            tracks,
        })
    }
}

#[derive(Clone, Debug)]
struct LastfmScrobble {
    artist: String,
    track: String,
    album: Option<String>,
    artist_mbid: Option<String>,
    track_mbid: Option<String>,
    album_mbid: Option<String>,
    url: Option<String>,
    scrobbled_at_ms: i64,
    now_playing: bool,
    raw: Value,
}

impl LastfmScrobble {
    fn from_json(raw: Value) -> Result<Self> {
        let now_playing = raw
            .get("@attr")
            .and_then(|v| v.get("nowplaying"))
            .and_then(|v| v.as_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("true"));
        let track = text_field(raw.get("name")).context("missing track name")?;
        let artist = text_field(raw.get("artist")).context("missing artist")?;
        let album = text_field(raw.get("album"));
        let scrobbled_at_ms = raw
            .get("date")
            .and_then(|v| v.get("uts"))
            .and_then(value_to_i64)
            .map(|s| s * 1000)
            .unwrap_or(0);
        if !now_playing && scrobbled_at_ms <= 0 {
            bail!("missing scrobble timestamp");
        }
        let artist_mbid = mbid_field(raw.get("artist"));
        let album_mbid = mbid_field(raw.get("album"));
        let track_mbid = raw
            .get("mbid")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(ToString::to_string);
        let url = raw
            .get("url")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(ToString::to_string);
        Ok(Self {
            artist,
            track,
            album,
            artist_mbid,
            track_mbid,
            album_mbid,
            url,
            scrobbled_at_ms,
            now_playing,
            raw,
        })
    }
}

fn text_field(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) => non_empty(s),
        Value::Object(map) => map
            .get("#text")
            .and_then(|v| v.as_str())
            .and_then(non_empty)
            .or_else(|| map.get("name").and_then(|v| v.as_str()).and_then(non_empty)),
        _ => None,
    }
}

fn mbid_field(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Object(map) => map.get("mbid").and_then(|v| v.as_str()).and_then(non_empty),
        _ => None,
    }
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn value_to_u32(value: &Value) -> Option<u32> {
    value
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .or_else(|| value.as_str()?.parse().ok())
}

fn value_to_i64(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_str()?.parse().ok())
}

fn idempotency_key(username: &str, scrobble: &LastfmScrobble) -> String {
    let mut hasher = Sha256::new();
    hasher.update(LASTFM_PROVIDER.as_bytes());
    hasher.update(b"\0");
    hasher.update(username.as_bytes());
    hasher.update(b"\0");
    hasher.update(scrobble.scrobbled_at_ms.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(normalized_scrobble_key(scrobble).as_bytes());
    format!("{:x}", hasher.finalize())
}

fn normalized_scrobble_key(scrobble: &LastfmScrobble) -> String {
    [
        normalize(&scrobble.artist),
        normalize(&scrobble.track),
        scrobble.album.as_deref().map(normalize).unwrap_or_default(),
        scrobble.artist_mbid.clone().unwrap_or_default(),
        scrobble.track_mbid.clone().unwrap_or_default(),
        scrobble.album_mbid.clone().unwrap_or_default(),
        scrobble.url.clone().unwrap_or_default(),
    ]
    .join("|")
}

fn normalize(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn parses_extended_recent_tracks_and_skips_nowplaying_later() {
        let json = serde_json::json!({
            "recenttracks": {
                "@attr": { "totalPages": "1" },
                "track": [
                    {"name":"Song", "artist":{"#text":"Artist", "mbid":"a"}, "album":{"#text":"Album"}, "date":{"uts":"1700000000"}, "url":"u"},
                    {"name":"Live", "artist":{"#text":"Artist"}, "@attr":{"nowplaying":"true"}}
                ]
            }
        });
        let page = LastfmPage::from_json(json).unwrap();
        assert_eq!(page.total_pages, 1);
        assert_eq!(page.tracks.len(), 2);
        assert!(page.tracks[1].now_playing);
        assert_eq!(page.tracks[0].scrobbled_at_ms, 1_700_000_000_000);
    }

    #[tokio::test]
    async fn client_fetches_recent_tracks_with_required_params_and_pagination() {
        let _guard = crate::ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        std::env::set_var("SPOTUIFY_LASTFM_API_BASE_URL", server.uri());
        Mock::given(method("GET"))
            .and(query_param("method", "user.getRecentTracks"))
            .and(query_param("user", "alice"))
            .and(query_param("api_key", "key"))
            .and(query_param("format", "json"))
            .and(query_param("extended", "1"))
            .and(query_param("limit", "200"))
            .and(query_param("from", "1700000000"))
            .and(query_param("to", "1700000200"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "recenttracks": {
                    "@attr": { "totalPages": "2" },
                    "track": [{"name":"One", "artist":{"#text":"Artist"}, "album":{"#text":"Album"}, "date":{"uts":"1700000100"}}]
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "recenttracks": {
                    "@attr": { "totalPages": "2" },
                    "track": [{"name":"Now", "artist":{"#text":"Artist"}, "@attr":{"nowplaying":"true"}}]
                }
            })))
            .mount(&server)
            .await;

        let tracks = LastfmClient::from_env_or_default("key".to_string())
            .recent_tracks_with_progress(
                "alice",
                Some(1_700_000_000_000),
                Some(1_700_000_200_000),
                |_, _, _| {},
            )
            .await
            .expect("recent tracks");

        std::env::remove_var("SPOTUIFY_LASTFM_API_BASE_URL");
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].track, "One");
    }

    #[tokio::test]
    async fn promote_imported_listen_updates_rollups_and_is_idempotent() {
        let store = Store::in_memory().await.expect("store");
        let scrobble = LastfmScrobble {
            artist: "Artist".to_string(),
            track: "Track".to_string(),
            album: Some("Album".to_string()),
            artist_mbid: None,
            track_mbid: None,
            album_mbid: None,
            url: None,
            scrobbled_at_ms: 1_700_000_000_000,
            now_playing: false,
            raw: serde_json::json!({"name":"Track"}),
        };
        let item = MediaItem {
            id: Some("track-id".to_string()),
            uri: "spotify:track:imported".to_string(),
            name: "Track".to_string(),
            subtitle: "Artist".to_string(),
            context: "Album".to_string(),
            duration_ms: 240_000,
            image_url: None,
            kind: MediaKind::Track,
            source: Some("test".to_string()),
            freshness: None,
            explicit: None,
            is_playable: Some(true),
            ..Default::default()
        };

        assert!(promote_imported_listen(&store, 42, &scrobble, &item)
            .await
            .expect("promote"));
        let track_count: i64 = sqlx::query_scalar(
            "SELECT qualified_count FROM track_metrics WHERE track_uri = 'spotify:track:imported'",
        )
        .fetch_one(store.reader())
        .await
        .unwrap();
        assert_eq!(track_count, 1);

        assert!(!promote_imported_listen(&store, 42, &scrobble, &item)
            .await
            .expect("duplicate promote check"));
        let fact_count = store.count_listen_facts_for_external(42).await.unwrap();
        let track_count_after: i64 = sqlx::query_scalar(
            "SELECT qualified_count FROM track_metrics WHERE track_uri = 'spotify:track:imported'",
        )
        .fetch_one(store.reader())
        .await
        .unwrap();
        assert_eq!(fact_count, 1);
        assert_eq!(track_count_after, 1);
    }

    #[tokio::test]
    async fn client_retries_lastfm_rate_limit_code_29() {
        let _guard = crate::ENV_LOCK.lock().await;
        let server = MockServer::start().await;
        std::env::set_var("SPOTUIFY_LASTFM_API_BASE_URL", server.uri());
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "error": 29,
                "message": "Rate Limit Exceeded"
            })))
            .expect(3)
            .mount(&server)
            .await;

        let err = LastfmClient::from_env_or_default("key".to_string())
            .recent_tracks_with_progress("alice", None, None, |_, _, _| {})
            .await
            .expect_err("rate limit should fail after bounded retries");

        std::env::remove_var("SPOTUIFY_LASTFM_API_BASE_URL");
        assert!(err.to_string().contains("rate limited"));
    }
}
