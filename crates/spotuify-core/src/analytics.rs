//! Analytics event types shared across the workspace.
//!
//! The recording implementation (`AnalyticsStore` over SQLite) lives in
//! the binary's `src/analytics.rs` and impls [`AnalyticsSink`] defined
//! here. Producers (e.g. `SpotifyClient`) hold an
//! `Option<Arc<dyn AnalyticsSink>>` rather than a concrete store so the
//! crate seam stays clean.

use serde::{Deserialize, Serialize};
use std::str::FromStr;

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
    type Err = String;

    fn from_str(value: &str) -> Result<Self, String> {
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
            other => Err(format!("unknown analytics event kind `{other}`")),
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
    type Err = String;

    fn from_str(value: &str) -> Result<Self, String> {
        match value {
            "cli" => Ok(Self::Cli),
            "tui" => Ok(Self::Tui),
            "spotify_api" => Ok(Self::SpotifyApi),
            "daemon" => Ok(Self::Daemon),
            other => Err(format!("unknown analytics source `{other}`")),
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
    pub payload: serde_json::Value,
}

/// Read-back shape from the persisted event log. Identical to
/// [`AnalyticsEvent`] plus the auto-assigned `id` and the
/// `received_at_ms` timestamp recorded at insertion time.
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
    pub payload: serde_json::Value,
}

/// Decouples the spotify HTTP client (producer) from the SQLite-backed
/// analytics store (consumer). The binary's `AnalyticsStore` impls
/// this; `SpotifyClient` holds `Option<Arc<dyn AnalyticsSink>>` so it
/// can be built in spotuify-spotify without dragging sqlx in.
#[async_trait::async_trait]
pub trait AnalyticsSink: Send + Sync + std::fmt::Debug {
    /// Persist (or otherwise consume) the event. Failures are
    /// swallowed inside the impl per Phase 6 design -- analytics
    /// recording is best-effort and must not block the producer.
    async fn record(&self, event: &AnalyticsEvent);
}

/// Current wall-clock time in unix milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Build a `SpotifyApiFinished` event. Used by `SpotifyClient` at
/// every HTTP round-trip. Path is redacted before persistence to
/// avoid leaking URIs, search queries, and `ids=` parameters.
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

/// Build a `SearchPerformed` event. Used by CLI/TUI/daemon when a
/// user-initiated search completes.
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

/// Build an `ActionFinished` event. Emitted on every mutating command.
pub fn action_finished_event(
    source: AnalyticsSource,
    action: &str,
    subject_uri: Option<&str>,
    result: &str,
    payload: serde_json::Value,
    occurred_at_ms: i64,
) -> AnalyticsEvent {
    let mut payload = match payload {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    payload.insert(
        "action".to_string(),
        serde_json::Value::String(action.to_string()),
    );
    payload.insert(
        "result".to_string(),
        serde_json::Value::String(result.to_string()),
    );
    AnalyticsEvent {
        kind: AnalyticsEventKind::ActionFinished,
        occurred_at_ms,
        source,
        subject_uri: subject_uri.map(str::to_string),
        search_query: None,
        search_query_hash: None,
        payload: serde_json::Value::Object(payload),
    }
}

fn normalize_search_query(query: &str) -> String {
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn sha256_hex(value: &str) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(value.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Strip URI / search-query / market params from a Spotify API path
/// so the analytics event log doesn't carry user data.
pub fn redact_spotify_path(path: &str) -> String {
    let Some((base, query)) = path.split_once('?') else {
        return path.to_string();
    };
    let query = query
        .split('&')
        .filter_map(|pair| {
            let (key, _value) = pair.split_once('=')?;
            const REDACT: &[&str] = &["q", "ids", "uri", "uris", "market"];
            if REDACT.iter().any(|k| k.eq_ignore_ascii_case(key)) {
                Some(format!("{key}=<redacted>"))
            } else {
                Some(pair.to_string())
            }
        })
        .collect::<Vec<_>>()
        .join("&");
    if query.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{query}")
    }
}
