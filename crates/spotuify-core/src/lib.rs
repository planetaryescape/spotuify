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
pub mod queue_merge;

pub use analytics::{
    action_finished_event, listen_qualified_event, now_ms, playback_completed_event,
    playback_paused_event, playback_resumed_event, playback_skipped_event, playback_started_event,
    qualify_listen, redact_spotify_path, search_performed_event, spotify_api_finished_event,
    AnalyticsEvent, AnalyticsEventKind, AnalyticsSink, AnalyticsSource, BackendLabel, HabitBucket,
    HabitWindow, ListenFact, MeasurementKind, PlaybackSource, Qualification, SkipReason,
    StoredAnalyticsEvent, QUALIFICATION_RULE_VERSION,
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

impl std::fmt::Display for MediaKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl std::str::FromStr for MediaKind {
    type Err = MediaKindParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "track" => Ok(Self::Track),
            "episode" => Ok(Self::Episode),
            "show" => Ok(Self::Show),
            "album" => Ok(Self::Album),
            "artist" => Ok(Self::Artist),
            "playlist" => Ok(Self::Playlist),
            other => Err(MediaKindParseError {
                value: other.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaKindParseError {
    pub value: String,
}

impl std::fmt::Display for MediaKindParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown media kind `{}`", self.value)
    }
}

impl std::error::Error for MediaKindParseError {}

/// A named reference to an artist, carrying the URI so clients can navigate
/// from a track/album straight to the artist without re-resolving by name.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ArtistRef {
    pub name: String,
    pub uri: String,
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
    /// Album name for tracks (distinct from `context`, which the player rail
    /// reuses for the playback context label). `None` for non-track items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    /// When the item was saved/added (Unix epoch ms) — `added_at` from
    /// `/me/tracks` or a playlist's `added_at`. Enables "Date Added" sort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at_ms: Option<i64>,
    /// Episode resume position (ms) from Spotify's `resume_point`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_position_ms: Option<u64>,
    /// Episode listened state from Spotify's `resume_point.fully_played`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fully_played: Option<bool>,
    /// Release date (episodes/albums), as Spotify's `YYYY-MM-DD` string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_date: Option<String>,
    /// Album grouping relative to an artist, from Spotify's `album_group`
    /// (falls back to `album_type`): `album` | `single` | `compilation` |
    /// `appears_on`. `None` for non-album items. Drives discography sections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_group: Option<String>,
    /// Whether this item is in the user's library (e.g. a saved album).
    /// Tagged by the daemon when listing an artist's discography so clients
    /// can offer an "in library only" filter without a refetch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_library: Option<bool>,
    /// Album URI for a track (`spotify:album:…`), so clients can navigate from
    /// a track to its album. `None` for non-track items or when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_uri: Option<String>,
    /// Contributing artists with their URIs, so clients can navigate from a
    /// track/album to each artist. Empty when unknown (older cached rows).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artists: Vec<ArtistRef>,
    /// Primary genre, when known. Spotify carries genres on the artist/album
    /// rather than the track, so this is populated best-effort and flows live
    /// from the provider (not persisted), like `album_group`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
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
        value.parse().ok()
    }
}

impl std::fmt::Display for LyricsProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl std::str::FromStr for LyricsProvider {
    type Err = LyricsProviderParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "spotify-mercury" | "spotify" => Ok(Self::SpotifyMercury),
            "lrclib" => Ok(Self::Lrclib),
            other => Err(LyricsProviderParseError {
                value: other.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricsProviderParseError {
    pub value: String,
}

impl std::fmt::Display for LyricsProviderParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown lyrics provider `{}`", self.value)
    }
}

impl std::error::Error for LyricsProviderParseError {}

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

/// How often a reminder repeats. One-shot is `None`; the rest map to a
/// repeating calendar trigger and a next-occurrence computation in the
/// reminder's timezone.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Recurrence {
    #[default]
    None,
    Daily,
    Weekly,
    Monthly,
}

impl Recurrence {
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "none" | "once" | "one-shot" => Some(Self::None),
            "daily" | "day" => Some(Self::Daily),
            "weekly" | "week" => Some(Self::Weekly),
            "monthly" | "month" => Some(Self::Monthly),
            _ => None,
        }
    }

    pub fn is_recurring(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Lifecycle of a reminder *schedule*.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReminderState {
    #[default]
    Active,
    Completed,
    Cancelled,
}

/// A scheduled reminder for a media item or grouping (track/album/playlist/
/// artist/show/episode). The daemon owns it; clients render/act. A media
/// snapshot is captured at creation so it still displays if the item changes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Reminder {
    pub id: String,
    pub media_uri: String,
    pub media_kind: MediaKind,
    pub name: String,
    pub subtitle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    /// First/base due time (Unix epoch ms).
    pub anchor_at_ms: i64,
    pub recurrence: Recurrence,
    /// IANA timezone the anchor/recurrence is computed in.
    pub tz: String,
    /// Next time this reminder will fire (epoch ms). Advances on each fire.
    pub next_due_at_ms: i64,
    pub state: ReminderState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub created_at_ms: i64,
}

/// Lifecycle of a fired reminder *occurrence* (an inbox notification).
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationState {
    #[default]
    Unseen,
    Seen,
    Snoozed,
    Dismissed,
    Done,
}

/// A fired reminder occurrence shown in the notifications inbox. Media fields
/// are denormalized so the row survives the reminder being cancelled.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Notification {
    pub id: String,
    pub reminder_id: String,
    pub media_uri: String,
    pub media_kind: MediaKind,
    pub name: String,
    pub subtitle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    /// The occurrence's scheduled time (epoch ms).
    pub due_at_ms: i64,
    pub fired_at_ms: i64,
    pub state: NotificationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until_ms: Option<i64>,
    /// "played" / "queued" once the user acts on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acted: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
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
            assert_eq!(kind.to_string(), kind.label());
            assert_eq!(
                kind.label().parse::<MediaKind>().expect("label parses"),
                kind
            );
        }
    }

    #[test]
    fn lyrics_provider_round_trips_through_label_display_parse_and_json() {
        let providers = [LyricsProvider::SpotifyMercury, LyricsProvider::Lrclib];
        for provider in providers {
            let encoded =
                serde_json::to_string(&provider).expect("lyrics provider should serialize");
            let decoded: LyricsProvider =
                serde_json::from_str(&encoded).expect("lyrics provider should deserialize");
            assert_eq!(provider, decoded);
            assert_eq!(encoded.trim_matches('"'), provider.label());
            assert_eq!(provider.to_string(), provider.label());
            assert_eq!(
                provider
                    .label()
                    .parse::<LyricsProvider>()
                    .expect("label parses"),
                provider
            );
        }
        assert_eq!(
            "spotify".parse::<LyricsProvider>().expect("alias parses"),
            LyricsProvider::SpotifyMercury
        );
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
            ..Default::default()
        };
        let json = serde_json::to_value(&item).expect("media item should serialize");
        let obj = json.as_object().expect("media item JSON should be object");
        assert!(!obj.contains_key("source"));
        assert!(!obj.contains_key("freshness"));
        assert!(!obj.contains_key("explicit"));
        assert!(!obj.contains_key("is_playable"));
        assert!(!obj.contains_key("album"));
        assert!(!obj.contains_key("added_at_ms"));
        assert!(!obj.contains_key("resume_position_ms"));
        assert!(!obj.contains_key("fully_played"));
        assert!(!obj.contains_key("release_date"));
    }

    #[test]
    fn media_item_serializes_new_optional_fields_when_present() {
        let item = MediaItem {
            uri: "spotify:track:abc".to_string(),
            name: "Song".to_string(),
            duration_ms: 1000,
            kind: MediaKind::Track,
            album: Some("Greatest Hits".to_string()),
            added_at_ms: Some(1_700_000_000_000),
            fully_played: Some(true),
            ..Default::default()
        };
        let json = serde_json::to_value(&item).expect("media item should serialize");
        assert_eq!(
            json.get("album").and_then(|v| v.as_str()),
            Some("Greatest Hits")
        );
        assert_eq!(
            json.get("added_at_ms").and_then(|v| v.as_i64()),
            Some(1_700_000_000_000)
        );
        assert_eq!(
            json.get("fully_played").and_then(|v| v.as_bool()),
            Some(true)
        );
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
}

#[cfg(test)]
mod dev_dependencies_imports {
    // Required because serde_json is a dev-dependency of this crate but not a
    // direct dependency. The test module uses it via `serde_json::*` paths.
    #[allow(unused_imports)]
    use serde_json as _;
}
