//! Core domain types for spotuify.
//!
//! Per `docs/blueprint/01-architecture.md` §"Dependency rules", this crate has
//! **no internal dependencies**. Every other workspace member may import from
//! it; it imports from nothing in the workspace.
//!
//! These types describe the music domain — what plays, what's queued, what
//! devices exist, what playlists hold. IPC framing, HTTP semantics, storage
//! schema, and TUI rendering belong in other crates.

pub mod analytics;
pub mod ids;

pub use analytics::{
    action_finished_event, listen_qualified_event, now_ms, playback_completed_event,
    playback_paused_event, playback_resumed_event, playback_skipped_event, playback_started_event,
    qualify_listen, redact_spotify_path, search_performed_event, spotify_api_finished_event,
    AnalyticsEvent, AnalyticsEventKind, AnalyticsSink, AnalyticsSource, BackendLabel, HabitBucket,
    HabitWindow, ListenFact, PlaybackSource, Qualification, SkipReason, StoredAnalyticsEvent,
    QUALIFICATION_RULE_VERSION,
};
pub use ids::{AlbumId, ArtistId, PlaylistId, TrackId};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Playback {
    pub item: Option<MediaItem>,
    pub device: Option<Device>,
    pub is_playing: bool,
    pub progress_ms: u64,
    pub shuffle: bool,
    pub repeat: String,
    /// Phase 4 — when this snapshot was sampled by the daemon (Unix
    /// epoch ms). `None` on legacy payloads from older daemons. Clients
    /// can use it to compute staleness without trusting their own
    /// clock-skew with the daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampled_at_ms: Option<i64>,
    /// Phase 4 — Spotify Web API `timestamp` field: the last
    /// state-transition time according to Spotify, not when the response
    /// was generated. `None` outside `WebApiPoll` snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_timestamp_ms: Option<i64>,
    /// Phase 4 — provenance of this snapshot. Lets clients distinguish
    /// authoritative `PlayerEvent`/`CommandResult` state from
    /// best-effort `Cache`/`RecentFallback`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PlaybackStateSource>,
}

/// Phase 4 — where a `Playback` snapshot came from. Highest-trust first.
/// Kebab-case wire format matches `MediaKind`/`BackendKind` conventions.
///
/// Distinct from `analytics::PlaybackSource` (which records how the user
/// *got to* a track — playlist, queue, library, ...). This describes
/// *how the daemon learned* the current state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlaybackStateSource {
    /// Local librespot / spotifyd event stream — sub-100ms after the
    /// audio actually changed state.
    PlayerEvent,
    /// `CommandResult.playback` returned by `actions::execute` right
    /// after the mutation API call.
    CommandResult,
    /// Background `GET /me/player` poll. Eventually consistent.
    WebApiPoll,
    /// On-disk `playback_snapshots` row read at daemon startup or
    /// during cold-start, before any live signal landed.
    Cache,
    /// Synthesized "last played" from `recent_items` when no real
    /// playback snapshot exists. Always paused.
    RecentFallback,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Queue {
    pub currently_playing: Option<MediaItem>,
    pub items: Vec<MediaItem>,
    /// True when Spotify reported an active playback session at the
    /// time the snapshot was taken. False when the snapshot is being
    /// served from cache (Spotify currently has no active session, so
    /// the queue endpoint returned empty and we are showing the last
    /// known items). Defaults to false for backward-compat with older
    /// peers that don't set the field — they get treated as cached.
    #[serde(default)]
    pub session_active: bool,
    /// Milliseconds since the epoch when the snapshot was captured.
    /// `0` means unknown (default-constructed). Matches the `i64`
    /// convention used by `Playback::sampled_at_ms` and the store's
    /// `fetched_at_ms` columns.
    #[serde(default)]
    pub as_of_ms: i64,
}

impl Queue {
    // Each URI appears at most once in the queue; we keep the first
    // occurrence. Re-adding an already-queued track is a no-op upstream,
    // so this normalises any duplicates that snuck in from earlier
    // appends or from a Spotify queue endpoint that returned them.
    pub fn dedupe_items(&mut self) {
        let mut seen: std::collections::HashSet<String> =
            std::collections::HashSet::with_capacity(self.items.len());
        self.items.retain(|item| seen.insert(item.uri.clone()));
    }
}

/// Which player implementation the daemon should use to register a
/// Spotify Connect device and stream audio. Spotuify is librespot-only
/// as of 2026-05-16 — the enum is kept as a forward-compat marker
/// (lets us add a future backend without a breaking wire change) but
/// has only one variant.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendKind {
    /// In-process librespot Player + Spirc. Single binary, gapless,
    /// mercury bus, local PCM for the visualizer. Sole supported
    /// backend post-Phase-0-cleanup; no Web-API or subprocess
    /// fallbacks remain.
    #[default]
    Embedded,
}

impl BackendKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Embedded => "embedded",
        }
    }

    /// Parse the user-facing string form used in config.toml and the
    /// `--backend` CLI flag. Returns the typo verbatim in the error so
    /// users can see what they typed.
    pub fn parse(value: &str) -> Result<Self, BackendKindParseError> {
        match value {
            "embedded" => Ok(Self::Embedded),
            other => Err(BackendKindParseError {
                value: other.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendKindParseError {
    pub value: String,
}

impl std::fmt::Display for BackendKindParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown player backend `{}`; only `embedded` is supported",
            self.value
        )
    }
}

impl std::error::Error for BackendKindParseError {}

#[cfg(test)]
mod backend_kind_tests {
    use super::BackendKind;

    #[test]
    fn label_is_lowercase_kebab() {
        assert_eq!(BackendKind::Embedded.label(), "embedded");
    }

    #[test]
    fn parse_round_trips_through_label() {
        let parsed =
            BackendKind::parse(BackendKind::Embedded.label()).expect("backend label should parse");
        assert_eq!(parsed, BackendKind::Embedded);
    }

    #[test]
    fn parse_typo_echoes_value_in_error() {
        let err = BackendKind::parse("embeded").expect_err("backend typo should error");
        assert!(err.value.contains("embeded"));
        assert!(err.to_string().contains("embeded"));
    }

    #[test]
    fn parse_rejects_old_spotifyd_and_connect_labels() {
        // Phase 0 cleanup removed the spotifyd subprocess and the
        // Web-API ConnectOnly backend. Old config.toml values must
        // surface a clear error rather than silently fall back.
        assert!(BackendKind::parse("spotifyd").is_err());
        assert!(BackendKind::parse("connect").is_err());
    }

    #[test]
    fn default_is_embedded() {
        assert_eq!(BackendKind::default(), BackendKind::Embedded);
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    #[default]
    Track,
    Episode,
    Show,
    Album,
    Artist,
    Playlist,
}

impl MediaKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Episode => "episode",
            Self::Show => "show",
            Self::Album => "album",
            Self::Artist => "artist",
            Self::Playlist => "playlist",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct MediaItem {
    pub id: Option<String>,
    pub uri: String,
    pub name: String,
    pub subtitle: String,
    pub context: String,
    pub duration_ms: u64,
    pub image_url: Option<String>,
    pub kind: MediaKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explicit: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_playable: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Device {
    pub id: Option<String>,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub is_active: bool,
    pub is_restricted: bool,
    pub volume_percent: Option<u8>,
    pub supports_volume: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub owner: String,
    pub tracks_total: u64,
    pub image_url: Option<String>,
    /// Spotify's playlist-version token (Phase 6.4 schema, Phase 6.5
    /// sync gate). When equal to the local copy, the daemon skips the
    /// expensive `/playlists/{id}/tracks` refetch. Optional because
    /// older cached rows + non-Spotify-sourced playlists may not have
    /// one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LyricsProvider {
    SpotifyMercury,
    Lrclib,
}

impl LyricsProvider {
    pub fn label(&self) -> &'static str {
        match self {
            Self::SpotifyMercury => "spotify-mercury",
            Self::Lrclib => "lrclib",
        }
    }

    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "spotify-mercury" | "spotify" => Some(Self::SpotifyMercury),
            "lrclib" => Some(Self::Lrclib),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LyricLine {
    pub start_ms: u64,
    pub text: String,
    pub is_rtl: bool,
}

pub fn active_lyric_line_index(
    lines: &[LyricLine],
    position_ms: u64,
    offset_ms: i64,
) -> Option<usize> {
    if lines.is_empty() {
        return None;
    }
    let adjusted = if offset_ms.is_negative() {
        position_ms.saturating_sub(offset_ms.unsigned_abs())
    } else {
        position_ms.saturating_add(offset_ms as u64)
    };
    let idx = lines.partition_point(|line| line.start_ms <= adjusted);
    Some(idx.saturating_sub(1))
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SyncedLyrics {
    pub provider: LyricsProvider,
    pub track_uri: String,
    pub lines: Vec<LyricLine>,
    pub fetched_at_ms: i64,
    pub synced: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_kind_round_trips_through_json_lowercase() {
        let kinds = [
            MediaKind::Track,
            MediaKind::Episode,
            MediaKind::Show,
            MediaKind::Album,
            MediaKind::Artist,
            MediaKind::Playlist,
        ];
        for kind in kinds {
            let encoded = serde_json::to_string(&kind).expect("media kind should serialize");
            let decoded: MediaKind =
                serde_json::from_str(&encoded).expect("media kind should deserialize");
            assert_eq!(kind, decoded);
            assert_eq!(encoded.trim_matches('"'), kind.label());
        }
    }

    #[test]
    fn media_item_omits_optional_fields_when_none() {
        let item = MediaItem {
            id: None,
            uri: "spotify:track:abc".to_string(),
            name: "Song".to_string(),
            subtitle: String::new(),
            context: String::new(),
            duration_ms: 1000,
            image_url: None,
            kind: MediaKind::Track,
            source: None,
            freshness: None,
            explicit: None,
            is_playable: None,
        };
        let json = serde_json::to_value(&item).expect("media item should serialize");
        let obj = json.as_object().expect("media item JSON should be object");
        assert!(!obj.contains_key("source"));
        assert!(!obj.contains_key("freshness"));
        assert!(!obj.contains_key("explicit"));
        assert!(!obj.contains_key("is_playable"));
    }

    #[test]
    fn playback_default_is_paused_empty() {
        let p = Playback::default();
        assert!(p.item.is_none());
        assert!(p.device.is_none());
        assert!(!p.is_playing);
        assert_eq!(p.progress_ms, 0);
    }

    #[test]
    fn device_renames_kind_to_type_in_json() {
        let device = Device {
            id: Some("dev1".to_string()),
            name: "Phone".to_string(),
            kind: "smartphone".to_string(),
            is_active: false,
            is_restricted: false,
            volume_percent: Some(50),
            supports_volume: true,
        };
        let json = serde_json::to_value(&device).expect("device should serialize");
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("smartphone")
        );
        assert!(json.get("kind").is_none());
    }

    #[test]
    fn active_lyric_line_index_uses_offset_adjusted_position() {
        let lines = vec![lyric_line(1_000), lyric_line(2_000), lyric_line(5_000)];

        assert_eq!(active_lyric_line_index(&lines, 2_500, 0), Some(1));
        assert_eq!(active_lyric_line_index(&lines, 1_500, 700), Some(1));
        assert_eq!(active_lyric_line_index(&lines, 2_500, -700), Some(0));
    }

    fn lyric_line(start_ms: u64) -> LyricLine {
        LyricLine {
            start_ms,
            text: start_ms.to_string(),
            is_rtl: false,
        }
    }

    fn queue_item(uri: &str) -> MediaItem {
        MediaItem {
            uri: uri.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn dedupe_items_keeps_first_occurrence_and_collapses_runs() {
        let mut queue = Queue {
            currently_playing: None,
            items: vec![
                queue_item("spotify:track:a"),
                queue_item("spotify:track:a"),
                queue_item("spotify:track:b"),
                queue_item("spotify:track:a"),
                queue_item("spotify:track:c"),
                queue_item("spotify:track:b"),
            ],
            ..Default::default()
        };
        queue.dedupe_items();
        let uris: Vec<&str> = queue.items.iter().map(|i| i.uri.as_str()).collect();
        assert_eq!(uris, vec!["spotify:track:a", "spotify:track:b", "spotify:track:c"]);
    }

    #[test]
    fn dedupe_items_leaves_currently_playing_alone_even_when_in_items() {
        let mut queue = Queue {
            currently_playing: Some(queue_item("spotify:track:now")),
            items: vec![
                queue_item("spotify:track:now"),
                queue_item("spotify:track:next"),
            ],
            ..Default::default()
        };
        queue.dedupe_items();
        assert_eq!(queue.items.len(), 2);
        assert_eq!(queue.items[0].uri, "spotify:track:now");
        assert_eq!(queue.items[1].uri, "spotify:track:next");
    }
}

#[cfg(test)]
mod dev_dependencies_imports {
    // Required because serde_json is a dev-dependency of this crate but not a
    // direct dependency. The test module uses it via `serde_json::*` paths.
    #[allow(unused_imports)]
    use serde_json as _;
}
