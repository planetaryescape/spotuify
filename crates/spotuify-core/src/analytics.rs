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

// --- Phase 10: listen qualification ---

/// Bumped when the qualification rule below changes. Stamped on every
/// `listen_facts` row so historic data stays computable when the
/// formula evolves.
pub const QUALIFICATION_RULE_VERSION: u32 = 1;

/// Result of evaluating the qualification rule on a single listen.
/// `threshold_ms` is exposed so callers can render "X of Y" progress UI.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Qualification {
    pub qualified: bool,
    pub threshold_ms: i64,
    pub rule_version: u32,
}

/// Compute whether an audible play time qualifies as a durable listen
/// for the given track duration.
///
/// Rule (Phase 10, blueprint `16-analytics.md`):
/// > duration_ms > 30s AND audible_ms >= max(30s, min(50% of duration, 4min))
///
/// Tracks at or below 30s never qualify. Audible time below 30s never
/// qualifies. The percentage formula is capped at 4 minutes so a 60-min
/// podcast doesn't require 30 minutes of audible play to count.
pub fn qualify_listen(duration_ms: i64, audible_ms: i64) -> Qualification {
    const FLOOR_MS: i64 = 30_000;
    const CAP_MS: i64 = 240_000;
    let threshold_ms = if duration_ms <= FLOOR_MS {
        // Below the duration floor, no threshold is meaningful — but
        // we still expose the floor so progress bars render sanely.
        FLOOR_MS
    } else {
        let half = duration_ms / 2;
        let capped = half.min(CAP_MS);
        capped.max(FLOOR_MS)
    };
    let qualified = duration_ms > FLOOR_MS && audible_ms >= threshold_ms;
    Qualification {
        qualified,
        threshold_ms,
        rule_version: QUALIFICATION_RULE_VERSION,
    }
}

/// Why a listen ended. Stored on every `listen_facts` row; drives the
/// "skip vs completion" distinction in derived metrics.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// User pressed Next.
    UserNext,
    /// User pressed Previous.
    UserPrevious,
    /// Track ran to completion naturally.
    TrackEnd,
    /// Player crashed or surfaced an error mid-track.
    Error,
    /// Connect device disconnected (AirPods unpaired, speaker lost
    /// network, etc.). Per blueprint, this NEVER counts as qualified
    /// regardless of audible time accumulated.
    SessionDied,
}

impl SkipReason {
    pub fn label(&self) -> &'static str {
        match self {
            Self::UserNext => "user_next",
            Self::UserPrevious => "user_previous",
            Self::TrackEnd => "track_end",
            Self::Error => "error",
            Self::SessionDied => "session_died",
        }
    }
}

/// How the user got to a track. Recorded per listen so habit analytics
/// can answer "what % of my listens come from playlists vs queue?".
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackSource {
    Search,
    Playlist,
    Album,
    Queue,
    Library,
    Agent,
    Radio,
    Unknown,
}

impl PlaybackSource {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Playlist => "playlist",
            Self::Album => "album",
            Self::Queue => "queue",
            Self::Library => "library",
            Self::Agent => "agent",
            Self::Radio => "radio",
            Self::Unknown => "unknown",
        }
    }
}

/// Which backend produced the listen. Lets analytics segment by player
/// engine when investigating drift.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendLabel {
    Embedded,
    Spotifyd,
    Connect,
}

impl BackendLabel {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Embedded => "embedded",
            Self::Spotifyd => "spotifyd",
            Self::Connect => "connect",
        }
    }
}

/// Provenance/measurement source for a listen fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementKind {
    /// Observed by spotuify's playback/session tracker.
    ObservedPlayback,
    /// Imported Last.fm scrobble. Carries no stop/progress timeline;
    /// audible_ms is the scrobble qualification lower bound.
    LastfmScrobbleImport,
}

impl MeasurementKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::ObservedPlayback => "observed_playback",
            Self::LastfmScrobbleImport => "lastfm_scrobble_import",
        }
    }
}

/// One row in `listen_facts`. Built by the daemon's SessionTracker at
/// every track finalisation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ListenFact {
    pub id: Option<i64>,
    pub session_id: String,
    pub track_uri: String,
    pub artist_uri: Option<String>,
    pub album_uri: Option<String>,
    /// Playback context the track was played from (playlist/album/artist
    /// URI). Enables playlist-level top-k. `None` for context-less plays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_uri: Option<String>,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
    pub duration_ms: i64,
    pub elapsed_ms: i64,
    pub audible_ms: i64,
    pub completion_ratio: f64,
    pub qualified: bool,
    pub qualification_rule_version: u32,
    pub skip_reason: Option<SkipReason>,
    pub source: Option<PlaybackSource>,
    pub backend: Option<BackendLabel>,
    pub private_session: bool,
    #[serde(default = "default_measurement_kind")]
    pub measurement_kind: MeasurementKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_scrobble_id: Option<i64>,
    pub created_at_ms: i64,
}

fn default_measurement_kind() -> MeasurementKind {
    MeasurementKind::ObservedPlayback
}

/// Habit-rollup bucket size.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HabitWindow {
    Day,
    Week,
    Month,
}

impl HabitWindow {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
        }
    }
}

/// One row in `habit_metrics`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HabitBucket {
    pub bucket: HabitWindow,
    pub bucket_start_ms: i64,
    pub listening_minutes: f64,
    pub unique_tracks: u32,
    pub unique_artists: u32,
    pub sessions: u32,
    pub top_hour_of_day: Option<u8>,
    pub exploration_ratio: f64,
    pub repeat_ratio: f64,
}

// --- Phase 10: playback event builders ---

#[allow(clippy::too_many_arguments)]
pub fn playback_started_event(
    source: AnalyticsSource,
    track_uri: &str,
    position_ms: i64,
    device_id: Option<&str>,
    private_session: bool,
    occurred_at_ms: i64,
) -> AnalyticsEvent {
    AnalyticsEvent {
        kind: AnalyticsEventKind::PlaybackStarted,
        occurred_at_ms,
        source,
        subject_uri: Some(track_uri.to_string()),
        search_query: None,
        search_query_hash: None,
        payload: serde_json::json!({
            "position_ms": position_ms,
            "device_id": device_id,
            "private_session": private_session,
        }),
    }
}

pub fn playback_paused_event(
    source: AnalyticsSource,
    track_uri: &str,
    position_ms: i64,
    occurred_at_ms: i64,
) -> AnalyticsEvent {
    AnalyticsEvent {
        kind: AnalyticsEventKind::PlaybackPaused,
        occurred_at_ms,
        source,
        subject_uri: Some(track_uri.to_string()),
        search_query: None,
        search_query_hash: None,
        payload: serde_json::json!({ "position_ms": position_ms }),
    }
}

pub fn playback_resumed_event(
    source: AnalyticsSource,
    track_uri: &str,
    position_ms: i64,
    occurred_at_ms: i64,
) -> AnalyticsEvent {
    AnalyticsEvent {
        kind: AnalyticsEventKind::PlaybackResumed,
        occurred_at_ms,
        source,
        subject_uri: Some(track_uri.to_string()),
        search_query: None,
        search_query_hash: None,
        payload: serde_json::json!({ "position_ms": position_ms }),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn playback_skipped_event(
    source: AnalyticsSource,
    track_uri: &str,
    position_ms: i64,
    elapsed_ms: i64,
    skip_reason: SkipReason,
    private_session: bool,
    occurred_at_ms: i64,
) -> AnalyticsEvent {
    AnalyticsEvent {
        kind: AnalyticsEventKind::PlaybackSkipped,
        occurred_at_ms,
        source,
        subject_uri: Some(track_uri.to_string()),
        search_query: None,
        search_query_hash: None,
        payload: serde_json::json!({
            "position_ms": position_ms,
            "elapsed_ms": elapsed_ms,
            "skip_reason": skip_reason.label(),
            "private_session": private_session,
        }),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn playback_completed_event(
    source: AnalyticsSource,
    track_uri: &str,
    elapsed_ms: i64,
    audible_ms: i64,
    completion_ratio: f64,
    qualification: Qualification,
    private_session: bool,
    occurred_at_ms: i64,
) -> AnalyticsEvent {
    AnalyticsEvent {
        kind: AnalyticsEventKind::PlaybackCompleted,
        occurred_at_ms,
        source,
        subject_uri: Some(track_uri.to_string()),
        search_query: None,
        search_query_hash: None,
        payload: serde_json::json!({
            "elapsed_ms": elapsed_ms,
            "audible_ms": audible_ms,
            "completion_ratio": completion_ratio,
            "qualified": qualification.qualified,
            "qualification_rule_version": qualification.rule_version,
            "qualification_threshold_ms": qualification.threshold_ms,
            "private_session": private_session,
        }),
    }
}

pub fn listen_qualified_event(
    source: AnalyticsSource,
    track_uri: &str,
    duration_ms: i64,
    audible_ms: i64,
    artist_uri: Option<&str>,
    album_uri: Option<&str>,
    occurred_at_ms: i64,
) -> AnalyticsEvent {
    AnalyticsEvent {
        kind: AnalyticsEventKind::ListenQualified,
        occurred_at_ms,
        source,
        subject_uri: Some(track_uri.to_string()),
        search_query: None,
        search_query_hash: None,
        payload: serde_json::json!({
            "duration_ms": duration_ms,
            "audible_ms": audible_ms,
            "artist_uri": artist_uri,
            "album_uri": album_uri,
            "qualification_rule_version": QUALIFICATION_RULE_VERSION,
        }),
    }
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
