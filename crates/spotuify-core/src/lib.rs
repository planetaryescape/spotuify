//! Core domain types for spotuify.
//!
//! Per `docs/blueprint/01-architecture.md` §"Dependency rules", this crate has
//! **no internal dependencies**. Every other workspace member may import from
//! it; it imports from nothing in the workspace.
//!
//! These types describe the music domain — what plays, what's queued, what
//! devices exist, what playlists hold. IPC framing, HTTP semantics, storage
//! schema, and TUI rendering belong in other crates.

pub mod actions;
pub mod analytics;
pub mod ids;
mod lyrics_provider;
pub mod provider;
pub mod queue_merge;
pub mod uri;

pub use actions::{CommandKind, CommandResult, PlayContext};
pub use analytics::{
    action_finished_event, listen_qualified_event, now_ms, playback_completed_event,
    playback_paused_event, playback_resumed_event, playback_skipped_event, playback_started_event,
    provider_api_finished_event, qualify_listen, redact_provider_path, search_performed_event,
    AnalyticsEvent, AnalyticsEventKind, AnalyticsSink, AnalyticsSource, BackendLabel, HabitBucket,
    HabitWindow, ListenFact, MeasurementKind, PlaybackSource, Qualification, SkipReason,
    StoredAnalyticsEvent, QUALIFICATION_RULE_VERSION,
};
pub use ids::{AlbumId, ArtistId, PlaylistId, TrackId};
pub use lyrics_provider::{LyricsProvider, LyricsProviderParseError};
pub use provider::{
    AccessOutcome, AccessUnavailable, CatalogCaps, ClientPreferences, CollectionRequest,
    FreshnessProbe, LibraryCaps, LibraryRequest, MusicProvider, Mutation, MutationCompletion,
    MutationFailure, MutationOutcome, MutationReceipt, PageContinuation, PageRequest, PlayRequest,
    PlaySource, PlaylistCaps, PlaylistInsertion, PlaylistItemRef, ProviderCaps, ProviderCatalog,
    ProviderDescriptor, ProviderError, ProviderExtras, ProviderExtrasCaps, ProviderId,
    ProviderIdError, ProviderPage, ProviderResult, QueueAddRequest, RemoteTransport,
    RequestContext, RequestPriority, ResolvedTarget, SearchCaps, SearchRequest, TargetClaim,
    TransportCaps, TransportCommand, TransportDevice, TransportOutcome,
};
pub use uri::{ResourceUri, UriError, UriScheme, UriSchemeError};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Playback {
    pub item: Option<MediaItem>,
    pub device: Option<Device>,
    pub is_playing: bool,
    pub progress_ms: u64,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    /// Phase 4 — when this snapshot was sampled by the daemon (Unix
    /// epoch ms). `None` on legacy payloads from older daemons. Clients
    /// can use it to compute staleness without trusting their own
    /// clock-skew with the daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampled_at_ms: Option<i64>,
    /// Provider-reported state-transition time (Unix epoch ms), not when the
    /// response was sampled. The Spotify adapter maps its playback
    /// `timestamp` here. `None` outside remote-poll snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_timestamp_ms: Option<i64>,
    /// Provenance of this snapshot. Lets clients distinguish
    /// authoritative `PlayerEvent`/`CommandResult` state from
    /// best-effort `Cache`/`RecentFallback`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PlaybackStateSource>,
}

/// Phase 4 — where a `Playback` snapshot came from. Highest-trust first.
/// Kebab-case wire format matches the other protocol enums.
///
/// Distinct from `analytics::PlaybackSource` (which records how the user
/// *got to* a track — playlist, queue, library, ...). This describes
/// *how the daemon learned* the current state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlaybackStateSource {
    /// Local player event stream — sub-100ms after the audio actually
    /// changed state. Spotify adapter: librespot/spotifyd events.
    PlayerEvent,
    /// `CommandResult.playback` returned by `actions::execute` right
    /// after the mutation API call.
    CommandResult,
    /// Background provider playback-state poll. Eventually consistent.
    ///
    /// Serialization stays on the legacy `web-api-poll` label during the
    /// compatibility window so released clients continue to decode new
    /// daemon snapshots. New peers also accept the neutral `remote-poll`
    /// label for the eventual wire cutover.
    #[serde(rename = "web-api-poll", alias = "remote-poll")]
    RemotePoll,
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
    /// True when the provider reported an active playback session at the
    /// time the snapshot was taken. False when the snapshot is being
    /// served from cache (the provider currently has no active session, so
    /// its queue endpoint returned empty and we are showing the last
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

/// Playback repeat behavior shared by protocol, provider, and player layers.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    #[default]
    Off,
    Context,
    Track,
}

impl RepeatMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Context => "context",
            Self::Track => "track",
        }
    }

    pub fn parse(value: &str) -> Result<Self, RepeatModeParseError> {
        match value {
            "off" => Ok(Self::Off),
            "context" => Ok(Self::Context),
            "track" => Ok(Self::Track),
            other => Err(RepeatModeParseError {
                value: other.to_string(),
            }),
        }
    }
}

impl std::fmt::Display for RepeatMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepeatModeParseError {
    pub value: String,
}

impl std::fmt::Display for RepeatModeParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "repeat mode `{}` invalid (expected off, context, track)",
            self.value
        )
    }
}

impl std::error::Error for RepeatModeParseError {}

/// A provider-neutral release date with explicit precision.
///
/// The wire representation remains the legacy scalar string (`YYYY`,
/// `YYYY-MM`, or `YYYY-MM-DD`) so released clients keep decoding it. Provider
/// adapters parse their native date fields into this type before constructing
/// a [`MediaItem`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReleaseDate {
    pub year: u16,
    pub month: Option<u8>,
    pub day: Option<u8>,
}

impl ReleaseDate {
    pub fn new(
        year: u16,
        month: Option<u8>,
        day: Option<u8>,
    ) -> Result<Self, ReleaseDateParseError> {
        let value = match (month, day) {
            (None, None) => format!("{year:04}"),
            (Some(month), None) => format!("{year:04}-{month:02}"),
            (Some(month), Some(day)) => format!("{year:04}-{month:02}-{day:02}"),
            (None, Some(day)) => format!("{year:04}-??-{day:02}"),
        };
        validate_release_date(year, month, day)
            .map_err(|reason| ReleaseDateParseError { value, reason })?;
        Ok(Self { year, month, day })
    }
}

fn validate_release_date(
    year: u16,
    month: Option<u8>,
    day: Option<u8>,
) -> Result<(), &'static str> {
    if year > 9999 {
        return Err("year must use at most four digits");
    }
    match (month, day) {
        (None, None) => Ok(()),
        (None, Some(_)) => Err("day requires a month"),
        (Some(month), None) if (1..=12).contains(&month) => Ok(()),
        (Some(_), None) => Err("month must be between 01 and 12"),
        (Some(month), Some(day)) => {
            chrono::NaiveDate::from_ymd_opt(i32::from(year), u32::from(month), u32::from(day))
                .map(|_| ())
                .ok_or("date is not valid")
        }
    }
}

impl std::str::FromStr for ReleaseDate {
    type Err = ReleaseDateParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let parts = value.split('-').collect::<Vec<_>>();
        let valid_widths = matches!(parts.as_slice(), [year] if year.len() == 4)
            || matches!(parts.as_slice(), [year, month] if year.len() == 4 && month.len() == 2)
            || matches!(parts.as_slice(), [year, month, day] if year.len() == 4 && month.len() == 2 && day.len() == 2);
        if !valid_widths {
            return Err(ReleaseDateParseError {
                value: value.to_string(),
                reason: "expected YYYY, YYYY-MM, or YYYY-MM-DD",
            });
        }
        let parse_error = || ReleaseDateParseError {
            value: value.to_string(),
            reason: "date components must be decimal numbers",
        };
        let year = parts[0].parse::<u16>().map_err(|_| parse_error())?;
        let month = parts
            .get(1)
            .map(|part| part.parse::<u8>().map_err(|_| parse_error()))
            .transpose()?;
        let day = parts
            .get(2)
            .map(|part| part.parse::<u8>().map_err(|_| parse_error()))
            .transpose()?;
        Self::new(year, month, day).map_err(|err| ReleaseDateParseError {
            value: value.to_string(),
            reason: err.reason,
        })
    }
}

impl std::fmt::Display for ReleaseDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.month, self.day) {
            (None, None) => write!(f, "{:04}", self.year),
            (Some(month), None) => write!(f, "{:04}-{month:02}", self.year),
            (Some(month), Some(day)) => write!(f, "{:04}-{month:02}-{day:02}", self.year),
            (None, Some(_)) => unreachable!("ReleaseDate validates day requires month"),
        }
    }
}

impl Serialize for ReleaseDate {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ReleaseDate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseDateParseError {
    pub value: String,
    pub reason: &'static str,
}

impl std::fmt::Display for ReleaseDateParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid release date `{}`: {}", self.value, self.reason)
    }
}

impl std::error::Error for ReleaseDateParseError {}

/// Provider-neutral album grouping used by discography clients.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum AlbumGroup {
    Album,
    Single,
    Compilation,
    AppearsOn,
    Other(String),
}

impl AlbumGroup {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Album => "album",
            Self::Single => "single",
            Self::Compilation => "compilation",
            Self::AppearsOn => "appears_on",
            Self::Other(value) => value,
        }
    }
}

impl From<String> for AlbumGroup {
    fn from(value: String) -> Self {
        match value.as_str() {
            "album" => Self::Album,
            "single" => Self::Single,
            "compilation" => Self::Compilation,
            "appears_on" => Self::AppearsOn,
            _ => Self::Other(value),
        }
    }
}

impl From<&str> for AlbumGroup {
    fn from(value: &str) -> Self {
        Self::from(value.to_string())
    }
}

impl std::fmt::Display for AlbumGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for AlbumGroup {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AlbumGroup {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self::from(String::deserialize(deserializer)?))
    }
}

/// Provenance of a media item. This records where metadata was obtained; it
/// is not the provider identity used for persistence keys.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ItemSource {
    Provider(String),
    Mercury,
    Local,
}

impl ItemSource {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Provider(provider) => provider,
            Self::Mercury => "mercury",
            Self::Local => "local",
        }
    }
}

impl From<String> for ItemSource {
    fn from(value: String) -> Self {
        match value.as_str() {
            "mercury" => Self::Mercury,
            "local" => Self::Local,
            _ => Self::Provider(value),
        }
    }
}

impl From<&str> for ItemSource {
    fn from(value: &str) -> Self {
        Self::from(value.to_string())
    }
}

impl std::fmt::Display for ItemSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for ItemSource {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ItemSource {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self::from(String::deserialize(deserializer)?))
    }
}

/// A provider-neutral page of domain items.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub offset: u64,
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
    pub source: Option<ItemSource>,
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
    /// Episode resume position in milliseconds. Spotify adapter: maps from
    /// `resume_point.resume_position_ms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_position_ms: Option<u64>,
    /// Episode listened state. Spotify adapter: maps from
    /// `resume_point.fully_played`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fully_played: Option<bool>,
    /// Parsed release date for episodes/albums, preserving provider precision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_date: Option<ReleaseDate>,
    /// Album grouping relative to an artist. Spotify adapter: maps
    /// `album_group`, falling back to `album_type`. Unknown provider values
    /// remain available through [`AlbumGroup::Other`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_group: Option<AlbumGroup>,
    /// Whether this item is in the user's library (e.g. a saved album).
    /// Tagged by the daemon when listing an artist's discography so clients
    /// can offer an "in library only" filter without a refetch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_library: Option<bool>,
    /// Album URI for a track, so clients can navigate from a track to its
    /// album. `None` for non-track items or when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_uri: Option<String>,
    /// Contributing artists with their URIs, so clients can navigate from a
    /// track/album to each artist. Empty when unknown (older cached rows).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artists: Vec<ArtistRef>,
    /// Primary genre, when known. Provider adapters populate it best-effort;
    /// it flows live rather than being persisted, like `album_group`.
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

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub owner: String,
    pub tracks_total: u64,
    pub image_url: Option<String>,
    /// Opaque provider version token used to skip unchanged playlist-track
    /// refetches. Missing tokens fail open and trigger a refetch.
    /// Rust uses the neutral name; the compatibility-stage wire key remains
    /// `snapshot_id` so released clients can decode new daemon responses.
    /// New peers also accept `version_token` input for the eventual cutover.
    #[serde(
        default,
        rename = "snapshot_id",
        alias = "version_token",
        skip_serializing_if = "Option::is_none"
    )]
    pub version_token: Option<String>,
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
    #![allow(clippy::unwrap_used)]

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
    fn release_date_preserves_precision_and_legacy_scalar_wire_shape() {
        let year = "1999".parse::<ReleaseDate>().expect("year precision");
        let month = "1999-07".parse::<ReleaseDate>().expect("month precision");
        let day = "2000-02-29".parse::<ReleaseDate>().expect("leap day");

        assert_eq!((year.year, year.month, year.day), (1999, None, None));
        assert_eq!((month.year, month.month, month.day), (1999, Some(7), None));
        assert_eq!(day.to_string(), "2000-02-29");
        assert_eq!(serde_json::to_string(&day).unwrap(), "\"2000-02-29\"");
        assert_eq!(
            serde_json::from_str::<ReleaseDate>("\"1999-07\"").unwrap(),
            month
        );
        assert!("2001-02-29".parse::<ReleaseDate>().is_err());
        assert!("2024-13".parse::<ReleaseDate>().is_err());
        assert!("2024-1-01".parse::<ReleaseDate>().is_err());
    }

    #[test]
    fn album_group_preserves_unknown_provider_values_on_scalar_wire() {
        let known = AlbumGroup::from("appears_on");
        let other = AlbumGroup::from("soundtrack");

        assert_eq!(known, AlbumGroup::AppearsOn);
        assert_eq!(other, AlbumGroup::Other("soundtrack".to_string()));
        assert_eq!(serde_json::to_string(&known).unwrap(), "\"appears_on\"");
        assert_eq!(
            serde_json::from_str::<AlbumGroup>("\"soundtrack\"").unwrap(),
            other
        );
    }

    #[test]
    fn item_source_is_typed_in_core_and_remains_a_scalar_on_wire() {
        let provider = ItemSource::from("provider-a");
        assert_eq!(provider, ItemSource::Provider("provider-a".to_string()));
        assert_eq!(ItemSource::from("mercury"), ItemSource::Mercury);
        assert_eq!(ItemSource::from("local"), ItemSource::Local);
        assert_eq!(serde_json::to_string(&provider).unwrap(), "\"provider-a\"");
        assert_eq!(
            serde_json::from_str::<ItemSource>("\"custom-provider\"").unwrap(),
            ItemSource::Provider("custom-provider".to_string())
        );
    }

    #[test]
    fn repeat_mode_round_trips_and_defaults_off() {
        for mode in [RepeatMode::Off, RepeatMode::Context, RepeatMode::Track] {
            assert_eq!(RepeatMode::parse(mode.label()).unwrap(), mode);
            let encoded = serde_json::to_string(&mode).unwrap();
            assert_eq!(serde_json::from_str::<RepeatMode>(&encoded).unwrap(), mode);
        }
        assert_eq!(RepeatMode::default(), RepeatMode::Off);
        assert!(RepeatMode::parse("loop").is_err());
    }

    #[test]
    fn remote_poll_accepts_neutral_label_but_writes_legacy_wire_label() {
        assert_eq!(
            serde_json::to_string(&PlaybackStateSource::RemotePoll).unwrap(),
            "\"web-api-poll\""
        );
        assert_eq!(
            serde_json::from_str::<PlaybackStateSource>("\"web-api-poll\"").unwrap(),
            PlaybackStateSource::RemotePoll
        );
        assert_eq!(
            serde_json::from_str::<PlaybackStateSource>("\"remote-poll\"").unwrap(),
            PlaybackStateSource::RemotePoll
        );
    }

    #[test]
    fn playlist_uses_neutral_rust_field_and_legacy_wire_key() {
        let playlist = Playlist {
            id: "mix".to_string(),
            name: "Mix".to_string(),
            owner: "Owner".to_string(),
            tracks_total: 3,
            image_url: None,
            version_token: Some("version-1".to_string()),
        };
        let encoded = serde_json::to_value(&playlist).unwrap();
        assert_eq!(encoded["snapshot_id"], "version-1");
        assert!(encoded.get("version_token").is_none());

        let old_client_fixture = r#"{
            "id":"mix","name":"Mix","owner":"Owner","tracks_total":3,
            "image_url":null,"snapshot_id":"legacy-version"
        }"#;
        let old = serde_json::from_str::<Playlist>(old_client_fixture).unwrap();
        assert_eq!(old.version_token.as_deref(), Some("legacy-version"));

        let future_fixture = r#"{
            "id":"mix","name":"Mix","owner":"Owner","tracks_total":3,
            "image_url":null,"version_token":"neutral-version"
        }"#;
        let future = serde_json::from_str::<Playlist>(future_fixture).unwrap();
        assert_eq!(future.version_token.as_deref(), Some("neutral-version"));
    }

    #[test]
    fn generic_page_round_trips() {
        let page = Page {
            items: vec!["one".to_string(), "two".to_string()],
            total: 12,
            offset: 5,
        };
        let encoded = serde_json::to_string(&page).unwrap();
        assert_eq!(
            serde_json::from_str::<Page<String>>(&encoded).unwrap(),
            page
        );
    }

    #[test]
    fn lyrics_provider_round_trips_through_label_display_parse_and_json() {
        let providers = [LyricsProvider::Native, LyricsProvider::Lrclib];
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
    }

    #[test]
    fn media_item_omits_optional_fields_when_none() {
        let item = MediaItem {
            id: None,
            uri: "provider:track:abc".to_string(),
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
            uri: "provider:track:abc".to_string(),
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
