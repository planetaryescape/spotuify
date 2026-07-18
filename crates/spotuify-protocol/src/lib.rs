//! IPC wire protocol shared between the spotuify daemon, CLI, TUI, and MCP server.
//!
//! All Request/Response/Event types live here. Per
//! `docs/blueprint/01-architecture.md` §"Dependency rules", this crate depends
//! only on `spotuify-core` for domain types. It must never import storage,
//! search, HTTP, or any other concern.

pub mod agent_playlists;
pub mod analytics;
pub mod event_log;
pub mod ipc_client;
pub mod ipc_stream;
pub mod operations;
pub mod output;
pub mod paths;

#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub use agent_playlists::{
    CandidateIssue, CandidateStatus, PlaylistCreateMetadata, PlaylistCreatePreview,
    PlaylistMutationPreview, PlaylistPlan, PlaylistTrackSelection, ResolvedTrackCandidate,
};
pub use analytics::{
    AnalyticsImportRunStatus, AnalyticsImportSummary, AnalyticsImportUndoSummary, ExportTarget,
    RebuildReport, RediscoveryCandidate, SearchHistoryEntry, SearchMode, SinceWindow, TopEntry,
    TopKind, UnresolvedScrobble,
};
pub use event_log::{findings_from, EventLog, LoggedEvent, LoggedKind};
pub use ipc_client::{default_socket_path, IpcClient};
pub use operations::{
    Operation, OperationId, OperationKind, OperationSource, OperationStatus, PreState, ReversalPlan,
};
pub use output::OutputFormat;
pub use spotuify_core::HabitWindow;
pub use spotuify_core::{HabitBucket, RepeatMode};

use bytes::BytesMut;
use serde::{Deserialize, Serialize};
use tokio_util::codec::{Decoder, Encoder, LengthDelimitedCodec};

use spotuify_core::{
    ClientPreferences, Device, MediaItem, MediaKind, Notification, Playback, Playlist,
    ProviderCatalog, ProviderId, Queue, Recurrence, Reminder, ResolvedTarget, SyncedLyrics,
};

/// IPC protocol version. Bumped to 6 for update-awareness + the podcast
/// overhaul: the `check-update` request + `update-available` event + `update-status`
/// response (clients surface "a newer release exists" with the right upgrade
/// command), the `episode-feed` request (a date-ordered episode feed across all
/// followed shows), and the `date` search sort. v5 added the artist-discography
/// browser (`followed-artists` + `album_group`/`in_library`); v3 added listening
/// reminders + notifications; v2 added `saved-tracks`/`show-episodes`/
/// `queue-add-many` + enriched `MediaItem`. Clients gate their UI on
/// `protocol_version >= IPC_PROTOCOL_VERSION`.
pub const IPC_PROTOCOL_VERSION: u32 = 7;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcMessage {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<OperationSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_id: Option<MutationId>,
    pub payload: IpcPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[allow(clippy::large_enum_variant)]
pub enum IpcPayload {
    Request(Request),
    Response(Response),
    Event(DaemonEvent),
}

impl IpcPayload {
    /// Coarse discriminant for diagnostics and log correlation. Cheap and
    /// never allocates. For a response, pair this with the `request_id` to
    /// recover the exact request kind from the `ipc.request` span (the span
    /// records `request_kind` for the same id before the response is sent).
    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::Request(req) => req.kind_label(),
            Self::Response(Response::Ok { .. }) => "response-ok",
            Self::Response(Response::Error { .. }) => "response-error",
            Self::Event(_) => "event",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PlaylistItemMutationAction {
    Add,
    Remove,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum Request {
    Ping,
    /// Opt this IPC connection into daemon event broadcasts.
    ///
    /// One-shot request clients should not receive unsolicited events;
    /// event-stream clients send this once before waiting on `next_event`.
    SubscribeEvents {
        /// Opt into provider-policy events added after the released v7 wire.
        /// Released clients omit this field and receive only event variants
        /// they can decode.
        #[serde(default)]
        provider_policy: bool,
    },
    Shutdown,
    GetDaemonStatus,
    GetDoctorReport,
    /// Cached startup snapshot for event-driven clients.
    ///
    /// This is deliberately read-only: it must not trigger Spotify
    /// refreshes. The daemon's own warm/sync loops own live provider
    /// reads so opening a TUI does not spend Web API budget before the
    /// user presses play.
    ClientSeed,
    /// Enumerate configured providers and their semantic capabilities.
    ProvidersList,
    /// Ask one or all provider adapters to normalize a raw user target.
    ResolveTarget {
        input: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_kinds: Option<Vec<MediaKind>>,
    },
    /// Enumerate local audio outputs and the persisted selection.
    ListAudioOutputs,
    PlaybackGet,
    PlaybackCommand {
        command: PlaybackCommand,
    },
    DevicesList,
    DeviceTransfer {
        device: String,
    },
    Search {
        query: String,
        scope: SearchScopeData,
        source: SearchSourceData,
        limit: u32,
        /// Provider route for local filtering and hybrid searches. When
        /// `source` is `Remote`, this must be absent or equal that provider.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
        /// Explicit set of kinds to return, overriding `scope` when present.
        /// Lets clients filter to arbitrary subsets ("podcasts only", "tracks
        /// + artists"). `None` falls back to the kinds implied by `scope`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kinds: Option<Vec<MediaKind>>,
        /// Result ordering applied by the daemon after fetch. `None` keeps
        /// Spotify's relevance order.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sort: Option<SearchSortData>,
    },
    /// Streaming, daemon-orchestrated search. Daemon acks immediately
    /// with `ResponseData::SearchStarted` and then publishes
    /// `DaemonEvent::SearchPage` / `DaemonEvent::SearchFailed` events on
    /// the existing event broadcast, followed by
    /// `DaemonEvent::SearchComplete` when the initial fanout is done.
    /// Clients must `SubscribeEvents` before sending this if they want
    /// the page events.
    ///
    /// On Spotify source, the daemon fans out 6 kinds × 3 pages = 18
    /// per-type page requests. On Local/Hybrid source, the daemon emits
    /// the Tantivy result as a single page event.
    SearchStream {
        query: String,
        scope: SearchScopeData,
        source: SearchSourceData,
        version: u64,
        /// Must match `source` when the source is `Remote`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    /// Single-page fetch used for scroll-triggered "load more" on a
    /// specific pane. Emits exactly one `DaemonEvent::SearchPage` or
    /// `DaemonEvent::SearchFailed` and no `SearchComplete`.
    SearchPage {
        query: String,
        kind: MediaKind,
        offset: u32,
        version: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    Reindex,
    CacheStatus,
    LibraryList {
        limit: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    /// Liked songs — the user's saved tracks (`GET /me/tracks`). Distinct from
    /// `LibraryList`, which returns saved albums/shows. Live provider read with
    /// `added_at_ms` populated; falls back to the cache when offline.
    SavedTracks {
        limit: u32,
        offset: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    /// Subscribed podcasts — the user's saved shows (`GET /me/shows`),
    /// served from the synced library cache.
    SavedShows {
        limit: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    /// Episodes of a single show, carrying provider-neutral listened state
    /// and a typed release date.
    ShowEpisodes {
        show: String,
        limit: u32,
        offset: u32,
    },
    LogsTail {
        lines: usize,
    },
    Sync {
        target: SyncTargetData,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    RecentlyPlayed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    Image {
        url: String,
    },
    CoverArt {
        url: String,
    },
    QueueGet,
    QueueAdd {
        uri: String,
    },
    /// Append many URIs to the queue in one request. Spotify's queue endpoint
    /// takes a single URI, so the daemon loops internally and returns one
    /// aggregate receipt + a single undo entry. Used for "queue all".
    QueueAddMany {
        uris: Vec<String>,
    },
    PlaylistsList {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    PlaylistTracks {
        playlist: String,
        #[serde(default)]
        wait: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    /// Fetch an artist's full discography. The daemon returns every album
    /// group (album/single/compilation/appears-on), each tagged with
    /// `album_group` and `in_library`, so clients section + filter locally.
    ArtistAlbums {
        artist: String,
    },
    /// List the artists the user follows (cache-backed; the discography
    /// browser's entry point).
    FollowedArtists {
        limit: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    /// Follow an artist (`PUT /me/following?type=artist`). Optimistic
    /// `LibraryChanged`; marks the artist `followed=1` in the cache.
    ArtistFollow {
        artist: String,
    },
    /// Unfollow an artist (`DELETE /me/following?type=artist`).
    ArtistUnfollow {
        artist: String,
    },
    /// Listening history grouped into sessions (gap-based), merging the local
    /// `listen_facts` with Spotify recently-played. Powers the history page's
    /// session-albums and chronological views.
    ListenSessions {
        limit: u32,
    },
    /// Fetch the track listing of a given album.
    AlbumTracks {
        album: String,
    },
    PlaylistAddItems {
        playlist: String,
        uris: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    PlaylistRemoveItems {
        playlist: String,
        uris: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    /// Read-only validation for playlist item mutations. This is a distinct
    /// command rather than a `dry_run` field on the write requests: an older
    /// daemon may ignore unknown fields and must never execute a preview as a
    /// real mutation.
    PlaylistItemsPreview {
        playlist: String,
        uris: Vec<String>,
        action: PlaylistItemMutationAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    PlaylistCreate {
        name: String,
        description: Option<String>,
        uris: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    /// Read-only validation for playlist creation. Kept distinct from the
    /// write command so an older daemon rejects it instead of ignoring a
    /// `dry_run` field and creating the playlist.
    PlaylistCreatePreview {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        uris: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    /// "Delete" a playlist the user owns. Spotify models deletion as
    /// the owner unfollowing the playlist, which `DELETE
    /// /v1/playlists/{id}/followers` performs. Not currently
    /// reversible — recovering an unfollowed playlist would mean
    /// recreating it and re-adding every item, which we don't snapshot.
    PlaylistUnfollow {
        playlist: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    /// Replace a playlist's cover art with a custom JPEG. `image_base64`
    /// carries the base64-encoded JPEG bytes (the daemon passes it
    /// through to `PUT /v1/playlists/{id}/images` as a raw text body
    /// with `Content-Type: image/jpeg`). Spotify caps the encoded body
    /// at 256 KB; reject larger payloads at the CLI before they reach
    /// the daemon. Not reversible — Spotify gives no read-back of the
    /// prior image bytes.
    PlaylistSetImage {
        playlist: String,
        image_base64: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    LibrarySave {
        uri: Option<String>,
        current: bool,
    },
    LibraryUnsave {
        uri: String,
    },
    LyricsGet {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        track_uri: Option<String>,
        #[serde(default)]
        force_refresh: bool,
    },
    LyricsOffsetSet {
        track_uri: String,
        offset_ms: i64,
    },

    // --- Phase 10: analytics derivations ---
    AnalyticsRebuild {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since_ms: Option<i64>,
    },
    AnalyticsTop {
        kind: TopKind,
        since_window: SinceWindow,
        limit: u32,
    },
    AnalyticsHabits {
        window: HabitWindow,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since_ms: Option<i64>,
    },
    AnalyticsSearch {
        mode: SearchMode,
        limit: u32,
    },
    AnalyticsRediscovery {
        gap_days: u32,
    },
    AnalyticsExport {
        target: ExportTarget,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since_ms: Option<i64>,
    },
    AnalyticsImport {
        target: ExportTarget,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        username: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_key: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_ms: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to_ms: Option<i64>,
        #[serde(default)]
        apply: bool,
    },
    AnalyticsImportStatus {
        run_id: String,
    },
    AnalyticsImportUnresolved {
        run_id: String,
    },
    AnalyticsImportUndo {
        run_id: String,
        #[serde(default)]
        dry_run: bool,
        #[serde(default)]
        force: bool,
    },
    AnalyticsPrune {
        apply: bool,
    },

    // --- Mercury-backed discovery (related artists + radio) ---
    RelatedArtists {
        artist: String,
    },
    RadioStart {
        seed_uri: String,
        /// Preview the resolved station without starting playback.
        #[serde(default)]
        dry_run: bool,
    },

    // --- Phase 12: operation log + undo ---
    OpsLog {
        limit: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since_ms: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<OperationSource>,
    },
    OpsShow {
        operation_id: OperationId,
        #[serde(default)]
        with_diff: bool,
    },
    OpsUndo {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operation_id: Option<OperationId>,
        #[serde(default)]
        dry_run: bool,
        #[serde(default)]
        force: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bulk_since_ms: Option<i64>,
    },
    OpsRedo {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operation_id: Option<OperationId>,
    },

    // --- Phase 13 — QoL / spec-compliance requests ---
    /// Reload the on-disk config. Provider topology changes require a daemon
    /// restart; runtime-safe settings are applied in place.
    Reload,
    /// Force-rebuild the upstream Spotify session. Useful after VPN
    /// flap / network change for embedded librespot.
    Reconnect,
    /// Rebind the embedded player's local audio output device without a
    /// daemon restart: updates the backend's device selection, rebuilds
    /// the Spirc + sink chain in-process, then resumes the interrupted
    /// track at its prior position. `None` follows the system default.
    SetAudioOutput {
        #[serde(default)]
        device: Option<String>,
    },
    /// Drop the daemon's cached Spotify token + clear the
    /// `auth_revoked` latch. Kept for compatibility; daemon-owned auth
    /// sessions now reload automatically after credential persistence.
    ReloadAuth,
    /// Start a daemon-owned interactive authentication session. Omitted
    /// provider/method select the configured default provider and auth mode.
    AuthStart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        method: Option<String>,
    },
    /// Read the latest state of a daemon-owned authentication session.
    AuthPoll {
        session_id: AuthSessionId,
    },
    /// Cancel a daemon-owned authentication session and its callback listener.
    AuthCancel {
        session_id: AuthSessionId,
    },
    /// Read a secret-free snapshot of the configured provider's auth state.
    AuthStatus {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    /// Atomically end auth sessions, remove all credential kinds, and clear
    /// daemon/player auth state for the configured provider.
    AuthLogout {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    /// Mint a Web API bearer from the daemon's first-party librespot
    /// session (login5). Lets CLI-direct clients (doctor, onboarding's
    /// initial sync) make authenticated Web API calls in first-party
    /// mode, where only the daemon holds the session that can mint.
    /// `force` requests a freshly minted token (used after a 401).
    WebApiToken {
        #[serde(default)]
        force: bool,
    },
    /// Prune old search-cache entries (`search_runs` / `search_results`).
    SearchCachePrune {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        older_than_ms: Option<i64>,
    },

    // --- Phase 17: audio visualization ---
    /// Enable or disable the spectrum visualizer. When false the
    /// daemon stops the FFT ticker and ceases broadcasting
    /// `SpectrumFrame` events.
    SetVizEnabled {
        enabled: bool,
    },
    /// Select the visualization source. `Auto` lets the daemon pick
    /// based on the active backend.
    SetVizSource {
        kind: VizSourceKindData,
    },
    /// Snapshot the visualizer's current configuration + active source
    /// + diagnostics. Used by the CLI `viz status` command.
    GetVizStatus,
    /// TUI focus hint — the daemon throttles FFT to 1 Hz when focused
    /// is false to keep CPU off background terminals.
    SetVizFocus {
        focused: bool,
    },

    // --- Listening reminders + notifications ---
    /// Schedule a reminder for a media item/grouping. The daemon captures a
    /// display snapshot, computes `next_due_at` from `anchor_at_ms` + recurrence
    /// in `tz`, and fires it at the due time.
    ReminderCreate {
        media_uri: String,
        anchor_at_ms: i64,
        #[serde(default)]
        recurrence: Recurrence,
        tz: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// List reminder schedules (active by default).
    RemindersList {
        #[serde(default)]
        include_inactive: bool,
    },
    /// Cancel a reminder schedule (stops future occurrences).
    ReminderCancel {
        id: String,
    },
    /// List inbox notifications (fired occurrences). Excludes archived by default.
    NotificationsList {
        #[serde(default)]
        include_archived: bool,
    },
    /// Act on an inbox notification.
    NotificationAct {
        id: String,
        action: NotificationAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snooze_until_ms: Option<i64>,
    },

    // --- Update awareness ---
    /// Report whether a newer GitHub release exists. The daemon checks
    /// periodically + caches the result; this returns the cache and (when
    /// `force` or the cache is stale) triggers a background refresh. Carries
    /// an `UpgradeHint` describing how this install upgrades (brew/cargo/DMG).
    CheckUpdate {
        #[serde(default)]
        force: bool,
    },

    // --- Podcast episode feed ---
    /// A flat, date-ordered episode feed merged across all followed shows.
    /// The daemon fans out `show-episodes` over the saved shows, merges +
    /// sorts, and caches the result; `refresh` forces a re-fetch.
    EpisodeFeed {
        limit: u32,
        #[serde(default)]
        sort: EpisodeSort,
        #[serde(default)]
        refresh: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
}

/// What to do with a fired notification.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NotificationAction {
    /// Mark seen (no playback).
    Seen,
    /// Play the media now (marks the notification done).
    Play,
    /// Add the media to the queue (marks done).
    Queue,
    /// Reschedule this occurrence to `snooze_until_ms`.
    Snooze,
    /// Dismiss without playing.
    Dismiss,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum IpcCategory {
    CoreMusic,
    SpotuifyPlatform,
    AdminMaintenance,
    ClientSpecific,
}

impl IpcCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::CoreMusic => "core-music",
            Self::SpotuifyPlatform => "spotuify-platform",
            Self::AdminMaintenance => "admin-maintenance",
            Self::ClientSpecific => "client-specific",
        }
    }
}

impl Request {
    /// Remote writes protected by the mutation-dedup lifecycle. Missing keys
    /// remain accepted for compatibility with older clients, but current
    /// clients attach one automatically for these requests.
    pub fn requires_mutation_id(&self) -> bool {
        match self {
            Self::OpsUndo { dry_run, .. } => !dry_run,
            Self::RadioStart { dry_run, .. } => !dry_run,
            Self::OpsRedo { .. } => true,
            request => matches!(
                request,
                Self::QueueAdd { .. }
                    | Self::QueueAddMany { .. }
                    | Self::PlaylistAddItems { .. }
                    | Self::PlaylistRemoveItems { .. }
                    | Self::PlaylistCreate { .. }
                    | Self::PlaylistUnfollow { .. }
                    | Self::PlaylistSetImage { .. }
                    | Self::LibrarySave { .. }
                    | Self::LibraryUnsave { .. }
                    | Self::ArtistFollow { .. }
                    | Self::ArtistUnfollow { .. }
            ),
        }
    }

    pub fn category(&self) -> IpcCategory {
        match self {
            Self::Ping
            | Self::SubscribeEvents { .. }
            | Self::Shutdown
            | Self::GetDaemonStatus
            | Self::GetDoctorReport
            | Self::LogsTail { .. }
            | Self::Sync { .. }
            | Self::Reconnect
            | Self::SetAudioOutput { .. }
            | Self::ListAudioOutputs
            | Self::ReloadAuth
            | Self::AuthStart { .. }
            | Self::AuthPoll { .. }
            | Self::AuthCancel { .. }
            | Self::AuthStatus { .. }
            | Self::AuthLogout { .. }
            | Self::CheckUpdate { .. }
            | Self::WebApiToken { .. } => IpcCategory::AdminMaintenance,
            Self::CacheStatus
            | Self::Reindex
            | Self::AnalyticsRebuild { .. }
            | Self::AnalyticsTop { .. }
            | Self::AnalyticsHabits { .. }
            | Self::AnalyticsSearch { .. }
            | Self::AnalyticsRediscovery { .. }
            | Self::AnalyticsExport { .. }
            | Self::AnalyticsImport { .. }
            | Self::AnalyticsImportStatus { .. }
            | Self::AnalyticsImportUnresolved { .. }
            | Self::AnalyticsImportUndo { .. }
            | Self::AnalyticsPrune { .. }
            | Self::OpsLog { .. }
            | Self::OpsShow { .. }
            | Self::OpsUndo { .. }
            | Self::OpsRedo { .. }
            | Self::SearchCachePrune { .. } => IpcCategory::SpotuifyPlatform,
            Self::SetVizEnabled { .. }
            | Self::SetVizSource { .. }
            | Self::GetVizStatus
            | Self::SetVizFocus { .. }
            | Self::ClientSeed => IpcCategory::ClientSpecific,
            Self::PlaybackGet
            | Self::PlaybackCommand { .. }
            | Self::DevicesList
            | Self::DeviceTransfer { .. }
            | Self::ProvidersList
            | Self::ResolveTarget { .. }
            | Self::Search { .. }
            | Self::SearchStream { .. }
            | Self::SearchPage { .. }
            | Self::LibraryList { .. }
            | Self::RecentlyPlayed { .. }
            | Self::Image { .. }
            | Self::CoverArt { .. }
            | Self::QueueGet
            | Self::QueueAdd { .. }
            | Self::QueueAddMany { .. }
            | Self::SavedTracks { .. }
            | Self::SavedShows { .. }
            | Self::ShowEpisodes { .. }
            | Self::EpisodeFeed { .. }
            | Self::PlaylistsList { .. }
            | Self::PlaylistTracks { .. }
            | Self::PlaylistItemsPreview { .. }
            | Self::ArtistAlbums { .. }
            | Self::FollowedArtists { .. }
            | Self::ArtistFollow { .. }
            | Self::ArtistUnfollow { .. }
            | Self::RelatedArtists { .. }
            | Self::RadioStart { .. }
            | Self::ListenSessions { .. }
            | Self::AlbumTracks { .. }
            | Self::PlaylistAddItems { .. }
            | Self::PlaylistRemoveItems { .. }
            | Self::PlaylistCreate { .. }
            | Self::PlaylistCreatePreview { .. }
            | Self::PlaylistUnfollow { .. }
            | Self::PlaylistSetImage { .. }
            | Self::LibrarySave { .. }
            | Self::LibraryUnsave { .. }
            | Self::LyricsGet { .. }
            | Self::LyricsOffsetSet { .. }
            | Self::ReminderCreate { .. }
            | Self::RemindersList { .. }
            | Self::ReminderCancel { .. }
            | Self::NotificationsList { .. }
            | Self::NotificationAct { .. }
            | Self::Reload => IpcCategory::CoreMusic,
        }
    }

    /// Stable short tag used in IPC observability spans and JSON logs.
    /// Matches the serde `rename_all = "kebab-case"` variant tag so log
    /// readers can pivot freely between wire payloads and tracing events.
    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::Ping => "ping",
            Self::SubscribeEvents { .. } => "subscribe-events",
            Self::Shutdown => "shutdown",
            Self::GetDaemonStatus => "get-daemon-status",
            Self::GetDoctorReport => "get-doctor-report",
            Self::ClientSeed => "client-seed",
            Self::ProvidersList => "providers-list",
            Self::ResolveTarget { .. } => "resolve-target",
            Self::ListAudioOutputs => "list-audio-outputs",
            Self::PlaybackGet => "playback-get",
            Self::PlaybackCommand { .. } => "playback-command",
            Self::DevicesList => "devices-list",
            Self::DeviceTransfer { .. } => "device-transfer",
            Self::Search { .. } => "search",
            Self::SearchStream { .. } => "search-stream",
            Self::SearchPage { .. } => "search-page",
            Self::Reindex => "reindex",
            Self::CacheStatus => "cache-status",
            Self::LibraryList { .. } => "library-list",
            Self::LogsTail { .. } => "logs-tail",
            Self::Sync { .. } => "sync",
            Self::RecentlyPlayed { .. } => "recently-played",
            Self::Image { .. } => "image",
            Self::CoverArt { .. } => "cover-art",
            Self::QueueGet => "queue-get",
            Self::QueueAdd { .. } => "queue-add",
            Self::QueueAddMany { .. } => "queue-add-many",
            Self::SavedTracks { .. } => "saved-tracks",
            Self::SavedShows { .. } => "saved-shows",
            Self::ShowEpisodes { .. } => "show-episodes",
            Self::PlaylistsList { .. } => "playlists-list",
            Self::PlaylistTracks { .. } => "playlist-tracks",
            Self::ArtistAlbums { .. } => "artist-albums",
            Self::FollowedArtists { .. } => "followed-artists",
            Self::ArtistFollow { .. } => "artist-follow",
            Self::ArtistUnfollow { .. } => "artist-unfollow",
            Self::ListenSessions { .. } => "listen-sessions",
            Self::AlbumTracks { .. } => "album-tracks",
            Self::PlaylistAddItems { .. } => "playlist-add-items",
            Self::PlaylistRemoveItems { .. } => "playlist-remove-items",
            Self::PlaylistItemsPreview { .. } => "playlist-items-preview",
            Self::PlaylistCreate { .. } => "playlist-create",
            Self::PlaylistCreatePreview { .. } => "playlist-create-preview",
            Self::PlaylistUnfollow { .. } => "playlist-unfollow",
            Self::PlaylistSetImage { .. } => "playlist-set-image",
            Self::LibrarySave { .. } => "library-save",
            Self::LibraryUnsave { .. } => "library-unsave",
            Self::LyricsGet { .. } => "lyrics-get",
            Self::LyricsOffsetSet { .. } => "lyrics-offset-set",
            Self::AnalyticsRebuild { .. } => "analytics-rebuild",
            Self::AnalyticsTop { .. } => "analytics-top",
            Self::AnalyticsHabits { .. } => "analytics-habits",
            Self::AnalyticsSearch { .. } => "analytics-search",
            Self::AnalyticsRediscovery { .. } => "analytics-rediscovery",
            Self::AnalyticsExport { .. } => "analytics-export",
            Self::AnalyticsImport { .. } => "analytics-import",
            Self::AnalyticsImportStatus { .. } => "analytics-import-status",
            Self::AnalyticsImportUnresolved { .. } => "analytics-import-unresolved",
            Self::AnalyticsImportUndo { .. } => "analytics-import-undo",
            Self::AnalyticsPrune { .. } => "analytics-prune",
            Self::RelatedArtists { .. } => "related-artists",
            Self::RadioStart { .. } => "radio-start",
            Self::OpsLog { .. } => "ops-log",
            Self::OpsShow { .. } => "ops-show",
            Self::OpsUndo { .. } => "ops-undo",
            Self::OpsRedo { .. } => "ops-redo",
            Self::Reload => "reload",
            Self::Reconnect => "reconnect",
            Self::SetAudioOutput { .. } => "set-audio-output",
            Self::ReloadAuth => "reload-auth",
            Self::AuthStart { .. } => "auth-start",
            Self::AuthPoll { .. } => "auth-poll",
            Self::AuthCancel { .. } => "auth-cancel",
            Self::AuthStatus { .. } => "auth-status",
            Self::AuthLogout { .. } => "auth-logout",
            Self::WebApiToken { .. } => "web-api-token",
            Self::SearchCachePrune { .. } => "search-cache-prune",
            Self::SetVizEnabled { .. } => "set-viz-enabled",
            Self::SetVizSource { .. } => "set-viz-source",
            Self::GetVizStatus => "get-viz-status",
            Self::SetVizFocus { .. } => "set-viz-focus",
            Self::ReminderCreate { .. } => "reminder-create",
            Self::RemindersList { .. } => "reminders-list",
            Self::ReminderCancel { .. } => "reminder-cancel",
            Self::NotificationsList { .. } => "notifications-list",
            Self::NotificationAct { .. } => "notification-act",
            Self::CheckUpdate { .. } => "check-update",
            Self::EpisodeFeed { .. } => "episode-feed",
        }
    }

    /// Every `kind_label()` value, sorted. The authoritative roster of
    /// request kinds the protocol exposes, used to enforce client
    /// parity (e.g. the macOS `DaemonRequest` enum). Keep in sync with
    /// `kind_label` — a new variant breaks `kind_label`'s exhaustive
    /// match at compile time, and `request_kinds_roster_matches_kind_label`
    /// fails until it is added here too.
    pub fn all_kind_labels() -> &'static [&'static str] {
        &[
            "album-tracks",
            "analytics-habits",
            "analytics-prune",
            "analytics-rebuild",
            "analytics-rediscovery",
            "analytics-search",
            "analytics-top",
            "artist-albums",
            "artist-follow",
            "artist-unfollow",
            "auth-cancel",
            "auth-logout",
            "auth-poll",
            "auth-start",
            "auth-status",
            "cache-status",
            "check-update",
            "client-seed",
            "cover-art",
            "device-transfer",
            "devices-list",
            "episode-feed",
            "followed-artists",
            "get-daemon-status",
            "get-doctor-report",
            "get-viz-status",
            "image",
            "library-list",
            "library-save",
            "library-unsave",
            "list-audio-outputs",
            "listen-sessions",
            "logs-tail",
            "lyrics-get",
            "lyrics-offset-set",
            "notification-act",
            "notifications-list",
            "ops-log",
            "ops-redo",
            "ops-show",
            "ops-undo",
            "ping",
            "playback-command",
            "playback-get",
            "playlist-add-items",
            "playlist-create",
            "playlist-create-preview",
            "playlist-items-preview",
            "playlist-remove-items",
            "playlist-set-image",
            "playlist-tracks",
            "playlist-unfollow",
            "playlists-list",
            "providers-list",
            "queue-add",
            "queue-add-many",
            "queue-get",
            "radio-start",
            "recently-played",
            "reconnect",
            "reindex",
            "related-artists",
            "reload",
            "reload-auth",
            "reminder-cancel",
            "reminder-create",
            "reminders-list",
            "resolve-target",
            "saved-shows",
            "saved-tracks",
            "search",
            "search-cache-prune",
            "search-page",
            "search-stream",
            "set-audio-output",
            "set-viz-enabled",
            "set-viz-focus",
            "set-viz-source",
            "show-episodes",
            "shutdown",
            "subscribe-events",
            "sync",
            "web-api-token",
        ]
    }
}

/// Sentinel context URI for the user's Liked Songs collection.
///
/// Spotify exposes no play-startable context URI for Liked Songs (its
/// real `spotify:user:…:collection` context rejects an `offset`), so
/// spotuify carries its own sentinel. When a `PlayUri` command sets
/// `context_uri` to this value, the daemon resolves the full ordered
/// Liked Songs list itself and starts playback at the tapped track.
pub const LIKED_SONGS_CONTEXT: &str = "spotuify:collection:liked";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PlaybackCommand {
    Pause,
    Resume,
    Toggle,
    Next,
    Previous,
    PlayUri {
        uri: String,
        /// Optional collection context the tapped `uri` plays inside of.
        ///
        /// - `None` → play `uri` as a lone track/context (unchanged).
        /// - `Some("spotify:album:…"/"spotify:playlist:…"/…)` → load that
        ///   context but start at `uri` (fixes album/playlist row taps).
        /// - `Some(LIKED_SONGS_CONTEXT)` → play the whole Liked Songs
        ///   collection starting at `uri`.
        ///
        /// Serde default + skip-if-none keeps the wire form byte-for-byte
        /// identical to the pre-context single-track command.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_uri: Option<String>,
    },
    Seek {
        position_ms: u64,
    },
    /// Relative seek — daemon resolves the absolute target against its
    /// `PlaybackClock`, so CLI scripts and the TUI can issue `+15s` / `-30s`
    /// without first reading a (possibly stale) cached playback snapshot.
    /// Negative offsets clamp at 0; positive clamps to track duration when known.
    SeekRelative {
        offset_ms: i64,
    },
    Volume {
        volume_percent: u8,
    },
    Shuffle {
        state: bool,
    },
    Repeat {
        state: RepeatMode,
    },
}

impl PlaybackCommand {
    /// Stable short tag used in spans and metrics. Mirrors the serde
    /// `rename_all = "kebab-case"` variant tag.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Toggle => "toggle",
            Self::Next => "next",
            Self::Previous => "previous",
            Self::PlayUri { .. } => "play-uri",
            Self::Seek { .. } => "seek",
            Self::SeekRelative { .. } => "seek-relative",
            Self::Volume { .. } => "volume",
            Self::Shuffle { .. } => "shuffle",
            Self::Repeat { .. } => "repeat",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchScopeData {
    All,
    Track,
    Episode,
    Show,
    Album,
    Artist,
    Playlist,
}

impl SearchScopeData {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Track => "track",
            Self::Episode => "episode",
            Self::Show => "show",
            Self::Album => "album",
            Self::Artist => "artist",
            Self::Playlist => "playlist",
        }
    }
}

/// How the daemon orders search results after fetch. Applied across the merged
/// (multi-kind) result set; `Relevance` preserves Spotify's own ordering.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchSortData {
    Relevance,
    Name,
    Duration,
    Artist,
    /// Order by typed release date (newest first). Useful for episode/show results.
    Date,
}

impl SearchSortData {
    pub fn label(self) -> &'static str {
        match self {
            Self::Relevance => "relevance",
            Self::Name => "name",
            Self::Duration => "duration",
            Self::Artist => "artist",
            Self::Date => "date",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchSourceData {
    Local,
    /// A single named provider. Only `Remote("spotify")` round-trips through
    /// released daemons: it serializes to the bare `"spotify"` legacy string,
    /// while any other provider serializes to `{ "remote": <id> }`, a shape
    /// older daemons reject on decode. Clients must therefore gate non-Spotify
    /// remote sources on a successful `ProvidersList` (proof the peer is new
    /// enough) before sending them.
    Remote(ProviderId),
    Hybrid,
}

impl SearchSourceData {
    /// Legacy default-provider encoding used only when provider discovery is
    /// unavailable (for example, a new client talking to an older daemon).
    pub fn legacy_default_remote() -> Self {
        Self::Remote(ProviderId::new("spotify").expect("legacy provider id is valid"))
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Local => "local",
            Self::Remote(provider) => provider.as_str(),
            Self::Hybrid => "hybrid",
        }
    }
}

impl Serialize for SearchSourceData {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Local => serializer.serialize_str("local"),
            Self::Hybrid => serializer.serialize_str("hybrid"),
            Self::Remote(provider) if provider.as_str() == "spotify" => {
                serializer.serialize_str("spotify")
            }
            Self::Remote(provider) => {
                use serde::ser::SerializeMap as _;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("remote", provider)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for SearchSourceData {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RemoteSource {
            remote: ProviderId,
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum WireSource {
            Legacy(String),
            Remote(RemoteSource),
        }

        match WireSource::deserialize(deserializer)? {
            WireSource::Legacy(value) if value == "local" => Ok(Self::Local),
            WireSource::Legacy(value) if value == "hybrid" => Ok(Self::Hybrid),
            WireSource::Legacy(value) if value == "spotify" => ProviderId::new("spotify")
                .map(Self::Remote)
                .map_err(serde::de::Error::custom),
            WireSource::Legacy(value) => Err(serde::de::Error::custom(format!(
                "unknown search source `{value}`"
            ))),
            WireSource::Remote(source) => Ok(Self::Remote(source.remote)),
        }
    }
}

/// How the cross-show episode feed (`Request::EpisodeFeed`) is ordered.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EpisodeSort {
    /// Most recent release first (the default feed view).
    #[default]
    Newest,
    /// Oldest release first.
    Oldest,
    /// Longest episodes first.
    Duration,
    /// Alphabetical by episode title.
    Title,
    /// Group by show/publisher name (alphabetical).
    Show,
}

impl EpisodeSort {
    pub fn label(self) -> &'static str {
        match self {
            Self::Newest => "newest",
            Self::Oldest => "oldest",
            Self::Duration => "duration",
            Self::Title => "title",
            Self::Show => "show",
        }
    }
}

/// How a given install of spotuify is upgraded, derived by the daemon from the
/// running executable's path. Clients render `command`/`url` verbatim.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UpgradeMethod {
    /// Installed via the Homebrew tap.
    Homebrew,
    /// Installed via `cargo install`.
    Cargo,
    /// Installed via the macOS .app/DMG (bundled CLI on `~/.local/bin`).
    MacApp,
    /// Unknown packaging — point at the releases page.
    Manual,
    /// Running from a `target/` dev build — no upgrade applies.
    Dev,
}

impl UpgradeMethod {
    pub fn label(self) -> &'static str {
        match self {
            Self::Homebrew => "homebrew",
            Self::Cargo => "cargo",
            Self::MacApp => "macapp",
            Self::Manual => "manual",
            Self::Dev => "dev",
        }
    }
}

/// Actionable upgrade guidance for the running install: the method plus the
/// exact command and/or URL a client surfaces to the user.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpgradeHint {
    pub method: UpgradeMethod,
    /// A shell command the user can run to upgrade (None for Dev).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// A URL to open (release page / DMG download) when relevant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SyncTargetData {
    All,
    Playback,
    Queue,
    Devices,
    Playlists,
    Recent,
    Library,
}

impl SyncTargetData {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Playback => "playback",
            Self::Queue => "queue",
            Self::Devices => "devices",
            Self::Playlists => "playlists",
            Self::Recent => "recent",
            Self::Library => "library",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum Response {
    Ok {
        data: ResponseData,
    },
    Error {
        message: String,
        #[serde(default)]
        kind: IpcErrorKind,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        code: String,
        #[serde(default)]
        retryable: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

impl Response {
    pub fn error(message: impl Into<String>) -> Self {
        Self::error_with_kind(message, IpcErrorKind::Internal)
    }

    pub fn error_with_kind(message: impl Into<String>, kind: IpcErrorKind) -> Self {
        Self::error_with_retryable(message, kind, kind.is_retryable())
    }

    pub fn error_with_retryable(
        message: impl Into<String>,
        kind: IpcErrorKind,
        retryable: bool,
    ) -> Self {
        Self::Error {
            message: message.into(),
            code: kind.as_code().to_string(),
            retryable,
            kind,
            provider: None,
            detail: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum IpcErrorKind {
    Auth,
    /// Spotify refresh token has been revoked (logged out elsewhere /
    /// password reset / app deauthorized). Distinct from `Auth` so
    /// clients can detect this specific case and offer an inline
    /// re-authentication flow rather than dumping a raw error.
    AuthRevoked,
    InvalidRequest,
    Network,
    Provider,
    RateLimited,
    Unsupported,
    /// The daemon abandoned the request after its category deadline.
    /// Retryable: a transient stall (slow Spotify call, contended lock)
    /// may clear on a second attempt.
    Timeout,
    /// `serde(other)`: an error kind from a newer daemon decodes here
    /// instead of failing the whole Response (which read as an IPC
    /// protocol error and killed the request).
    #[default]
    #[serde(other)]
    Internal,
}

impl IpcErrorKind {
    pub fn as_code(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::AuthRevoked => "auth_revoked",
            Self::InvalidRequest => "invalid_request",
            Self::Network => "network",
            Self::Provider => "provider",
            Self::RateLimited => "rate_limited",
            Self::Unsupported => "unsupported",
            Self::Timeout => "timeout",
            Self::Internal => "internal",
        }
    }

    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            IpcErrorKind::Network | IpcErrorKind::RateLimited | IpcErrorKind::Timeout
        )
    }
}

/// One listening session: a run of consecutively-played tracks bounded by a
/// gap larger than the sessionization threshold. `tracks` are newest-first
/// within the session; `context_label` is the dominant context (album/playlist
/// name) when one stands out, for the session-albums view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListenSession {
    pub session_id: String,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
    pub track_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_label: Option<String>,
    pub tracks: Vec<MediaItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[allow(clippy::large_enum_variant)]
pub enum ResponseData {
    Pong,
    Shutdown,
    DaemonStatus {
        status: DaemonStatus,
    },
    DoctorReport {
        report: DoctorReport,
    },
    Playback {
        playback: Playback,
    },
    Devices {
        devices: Vec<Device>,
    },
    ProviderList {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default_provider: Option<ProviderId>,
        providers: Vec<spotuify_core::ProviderDescriptor>,
    },
    TargetResolved {
        #[serde(default)]
        target: Option<ResolvedTarget>,
    },
    AudioOutputs {
        outputs: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected: Option<String>,
    },
    SearchResults {
        items: Vec<MediaItem>,
    },
    /// Ack for `Request::SearchStream` / `Request::SearchPage`. The
    /// actual results stream back as `DaemonEvent::SearchPage` events on
    /// the broadcast channel; clients filter by `(query, version)`.
    SearchStarted {
        query: String,
        version: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    CacheStatus {
        status: CacheStatus,
    },
    Reindex {
        stats: ReindexStats,
    },
    Sync {
        summary: CacheSyncSummary,
    },
    Image {
        bytes: Vec<u8>,
    },
    CoverArt {
        path: String,
        cache_hit: bool,
        bytes: u64,
        fetched_at_ms: Option<i64>,
    },
    Queue {
        queue: Queue,
    },
    ClientSeed {
        playback: Playback,
        queue: Queue,
        devices: Vec<Device>,
        recent: Vec<MediaItem>,
        viz: VizDiagnostics,
        /// `None` means an older daemon did not expose capabilities. `Some`
        /// with no providers is an explicit empty catalog and must not be
        /// treated as legacy/unknown capability state.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_catalog: Option<ProviderCatalog>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preferences: Option<ClientPreferences>,
        /// Active local-player policy restrictions. Defaulted so released
        /// daemon/client combinations retain the v7 seed shape.
        #[serde(default)]
        provider_policies: Vec<ProviderPolicyNotice>,
    },
    Playlists {
        playlists: Vec<Playlist>,
    },
    MediaItems {
        items: Vec<MediaItem>,
    },
    /// A single page of liked songs (answering `Request::SavedTracks`) that
    /// also carries the library `total` and the page `offset`, so scroll
    /// clients can size the full list and know when to stop paginating.
    /// Distinct from `MediaItems`, which carries no paging metadata — other
    /// callers keep using `MediaItems`.
    SavedTracksPage {
        items: Vec<MediaItem>,
        total: u32,
        offset: u32,
    },
    ListenSessions {
        sessions: Vec<ListenSession>,
    },
    Logs {
        lines: Vec<String>,
    },
    Mutation {
        receipt: CommandReceipt,
    },
    PlaylistCreate {
        receipt: PlaylistCreateReceipt,
    },
    Lyrics {
        lyrics: Option<SyncedLyrics>,
        offset_ms: i64,
    },
    LyricsOffset {
        track_uri: String,
        offset_ms: i64,
    },

    // --- Phase 10: analytics responses ---
    AnalyticsTop {
        entries: Vec<TopEntry>,
    },
    AnalyticsHabits {
        buckets: Vec<HabitBucket>,
    },
    AnalyticsSearch {
        entries: Vec<SearchHistoryEntry>,
    },
    AnalyticsRediscovery {
        candidates: Vec<RediscoveryCandidate>,
    },
    AnalyticsRebuildReport {
        report: RebuildReport,
    },
    AnalyticsPruneReport {
        rows_pruned: u64,
        dry_run: bool,
    },
    AnalyticsImportSummary {
        summary: AnalyticsImportSummary,
    },
    AnalyticsImportRunStatus {
        status: AnalyticsImportRunStatus,
    },
    AnalyticsImportUnresolved {
        entries: Vec<UnresolvedScrobble>,
    },
    AnalyticsImportUndoSummary {
        summary: AnalyticsImportUndoSummary,
    },

    // --- Phase 12: operations responses ---
    Operations {
        ops: Vec<Operation>,
    },
    OperationDetail {
        op: Operation,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<String>,
    },
    OperationUndoResult {
        undo_op_id: OperationId,
        succeeded: u32,
        skipped: u32,
        errors: Vec<String>,
        /// Dry-run only: one human-readable "would undo …" line per
        /// inspected operation. Empty for executed undos.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        preview: Vec<String>,
    },

    // --- Phase 13 — QoL / spec-compliance responses ---
    /// Generic acknowledge with a free-form message. Used by `reload`,
    /// `reconnect`, and `search-cache-prune`.
    Ack {
        message: String,
    },
    /// Snapshot returned by `auth-start`, `auth-poll`, and `auth-cancel`.
    AuthSession {
        session: AuthSessionData,
    },
    /// Secret-free credential and daemon auth-latch snapshot.
    AuthStatus {
        status: AuthStatusData,
    },
    /// Receipt for a completed daemon-owned logout.
    AuthLogout {
        result: AuthLogoutData,
    },
    /// A Web API bearer minted by the daemon (first-party login5).
    /// `None` when the daemon can't mint (not logged in / no session).
    WebApiToken {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<String>,
    },
    /// Phase 13 (P13-J) — search-cache prune result.
    SearchCachePruned {
        pruned_runs: u64,
        pruned_results: u64,
    },

    // --- Phase 17 — visualization responses ---
    /// Snapshot of the viz coordinator's current state. Returned by
    /// `Request::GetVizStatus`.
    VizStatus {
        diagnostics: VizDiagnostics,
    },

    // --- Listening reminders + notifications ---
    Reminders {
        reminders: Vec<Reminder>,
    },
    Notifications {
        notifications: Vec<Notification>,
    },
    ReminderCreated {
        reminder: Reminder,
    },

    // --- Update awareness ---
    /// Result of `Request::CheckUpdate`: whether a newer release exists, the
    /// current + latest versions, the release URL, and how to upgrade.
    UpdateStatus {
        update_available: bool,
        current_version: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        latest_version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        release_url: Option<String>,
        upgrade: UpgradeHint,
        /// When the cached check was performed (epoch ms); None if never checked.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checked_at_ms: Option<i64>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandReceipt {
    pub ok: bool,
    pub action: String,
    pub message: String,
    /// Correlates the eventual `MutationFinalized` event (drives the
    /// CLI's `--wait`). Optional for wire-compat with older daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<ReceiptId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_id: Option<MutationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ReceiptStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiErrorSummary>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub replayed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlaylistCreateReceipt {
    pub ok: bool,
    pub action: String,
    pub playlist_id: String,
    pub playlist_uri: String,
    pub name: String,
    pub added_item_count: usize,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<ReceiptId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_id: Option<MutationId>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub replayed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheStatus {
    pub database_path: String,
    pub index_path: String,
    #[serde(default)]
    pub cover_cache_path: String,
    pub media_items: u32,
    pub devices: u32,
    pub playback_snapshots: u32,
    #[serde(default)]
    pub queue_snapshots: u32,
    #[serde(default)]
    pub queue_items: u32,
    pub playlists: u32,
    pub playlist_items: u32,
    pub recent_items: u32,
    pub library_items: u32,
    pub search_runs: u32,
    pub search_results: u32,
    pub sync_events: u32,
    #[serde(default)]
    pub lyrics_cache: u32,
    #[serde(default)]
    pub lyrics_offsets: u32,
    #[serde(default)]
    pub cover_cache_files: u32,
    #[serde(default)]
    pub cover_cache_bytes: u64,
    #[serde(default)]
    pub cover_cache_oldest_entry_ms: Option<i64>,
    #[serde(default)]
    pub cover_cache_ttl_secs: u64,
    #[serde(default)]
    pub cover_cache_max_bytes: u64,
    pub index_documents: u64,
    pub last_sync_at_ms: Option<i64>,
    pub last_search_at_ms: Option<i64>,
    #[serde(default)]
    pub freshness: CacheFreshnessStatus,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheFreshnessStatus {
    pub media_items: FreshnessCounts,
    pub devices: FreshnessCounts,
    pub playback_snapshots: FreshnessCounts,
    #[serde(default)]
    pub queue_snapshots: FreshnessCounts,
    #[serde(default)]
    pub queue_items: FreshnessCounts,
    pub playlists: FreshnessCounts,
    pub playlist_items: FreshnessCounts,
    pub recent_items: FreshnessCounts,
    pub library_items: FreshnessCounts,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FreshnessCounts {
    pub fresh: u32,
    pub stale_but_usable: u32,
    pub refreshing: u32,
    pub failed_refresh: u32,
    pub unknown: u32,
    pub max_sync_generation: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReindexStats {
    pub indexed: u32,
    pub index_documents: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheSyncSummary {
    pub target: SyncTargetData,
    /// Provider identity for a provider-scoped pass. Aggregate summaries from
    /// older callers leave this unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderId>,
    pub playback_snapshots: u32,
    #[serde(default)]
    pub queue_snapshots: u32,
    #[serde(default)]
    pub queue_items: u32,
    pub devices: u32,
    pub playlists: u32,
    pub playlist_items: u32,
    pub recent_items: u32,
    pub library_items: u32,
    pub media_items: u32,
    /// Terminal outcome for this provider pass or aggregate pass. Omitted on
    /// success so the legacy wire shape remains byte-for-byte compatible.
    #[serde(default, skip_serializing_if = "SyncCompletionStatus::is_succeeded")]
    pub status: SyncCompletionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Aggregate-only provider outcomes. Old clients ignore this additive
    /// field; provider-scoped summaries leave it empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_outcomes: Vec<ProviderSyncOutcome>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncCompletionStatus {
    #[default]
    Succeeded,
    Partial,
    Failed,
}

impl SyncCompletionStatus {
    fn is_succeeded(&self) -> bool {
        *self == Self::Succeeded
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderSyncOutcome {
    pub provider: ProviderId,
    pub status: SyncCompletionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Stable identity for an active local-player provider-policy restriction.
/// Both fields participate in identity: a new reason from the same provider
/// must not inherit a dismissal for an older restriction.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProviderPolicyNotice {
    pub provider: ProviderId,
    pub reason: String,
}

// Note: DaemonEvent no longer derives `Eq` because `SpectrumFrame`
// carries `f32` payloads (FFT band magnitudes). `PartialEq` is retained
// for tests that need approximate comparisons; no internal callers
// require strict `Eq`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum DaemonEvent {
    ShutdownRequested,
    PlaybackChanged {
        action: String,
        /// Phase 3 (push model) — daemon embeds the freshly-computed
        /// `PlaybackClock` snapshot so subscribers (TUI, MCP) can apply
        /// directly without a follow-up `PlaybackGet` round-trip. Old
        /// clients ignore the field and fall back to the cache-first
        /// fetch path — graceful degrade.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        playback: Option<Playback>,
    },
    QueueChanged {
        action: String,
        uris: Vec<String>,
        /// Phase 3 — daemon embeds the just-persisted queue when known.
        /// Old clients ignore.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queue: Option<Queue>,
    },
    DevicesChanged {
        action: String,
        /// Phase 3 — daemon embeds the just-persisted device list when
        /// known. Old clients ignore.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        devices: Option<Vec<Device>>,
    },
    PlaylistsChanged {
        action: String,
        playlist: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    LibraryChanged {
        action: String,
        uris: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    SearchUpdated {
        query: String,
        count: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    /// Streaming search page result. Emitted once per resolved
    /// `(kind, offset)` pair by `Request::SearchStream` (one per kind ×
    /// initial-pages) and `Request::SearchPage` (one total). Empty
    /// `items` signals the pane is exhausted at this offset — Spotify's
    /// `total` field is unreliable (see plan), so empty-page is the
    /// canonical stop signal for successful requests only.
    SearchPage {
        query: String,
        kind: MediaKind,
        offset: u32,
        version: u64,
        items: Vec<MediaItem>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    /// Emitted once after a `Request::SearchStream`'s initial fanout has
    /// resolved (all 18 page tasks joined). Not emitted for scroll-
    /// triggered `Request::SearchPage` fetches.
    SearchComplete {
        query: String,
        version: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    /// Emitted when a streaming search or scroll-page request fails.
    /// Unlike `SearchPage { items: [] }`, this means the pane/request did
    /// not resolve successfully and clients must clear loading without
    /// marking the pane exhausted.
    SearchFailed {
        query: String,
        version: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<MediaKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offset: Option<u32>,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    /// Daemon-to-client signal that this subscriber's broadcast
    /// receiver lagged behind the channel's buffer and `skipped`
    /// events were dropped before reaching the wire. Clients that
    /// maintain push-driven state (e.g., the TUI's playback / queue /
    /// devices) MUST treat their view as potentially stale on receipt
    /// and re-seed via one-shot RPCs (`PlaybackGet`, `QueueGet`,
    /// `DevicesList`).
    EventStreamLagged {
        skipped: u64,
    },
    SyncStarted {
        target: SyncTargetData,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },
    SyncFinished {
        summary: CacheSyncSummary,
    },
    MutationFinished {
        action: String,
        message: String,
    },

    // Phase 6.7 — new typed events.
    //
    // RateLimited: emitted when the rate-limit middleware honours a 429
    // Retry-After. Clients show a countdown chip. `scope` is the symbolic
    // endpoint label, not a URL with user data.
    RateLimited {
        retry_after_secs: u64,
        scope: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },

    // AuthError: emitted on 401 after refresh fails, on 403 with required
    // scope mismatch, and on revoked refresh tokens.
    AuthError {
        kind: AuthErrorKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<ProviderId>,
    },

    // MutationAccepted: emitted as soon as a mutation request is
    // persisted as a pending receipt -- before Spotify is called.
    // Clients can show optimistic UI keyed on receipt_id.
    MutationAccepted {
        receipt_id: ReceiptId,
        action: String,
    },

    // MutationFinalized: emitted when a pending mutation transitions to
    // confirmed or failed. Distinct from the legacy MutationFinished
    // (which carries action+message) -- this one carries receipt_id and
    // typed status so the TUI can flip the spinner without parsing
    // strings.
    MutationFinalized {
        receipt_id: ReceiptId,
        status: ReceiptStatus,
        message: String,
    },

    // SchemaCompat: emitted when the compat normalizer (Phase 6.2)
    // backfilled keys. Tells us what Spotify changed without grepping
    // logs.
    SchemaCompat {
        endpoint: String,
        missing_keys: Vec<String>,
    },

    // Phase 9 — embedded librespot player lifecycle.
    //
    // PlayerReady: the active PlayerBackend registered a Connect device
    // and is accepting playback commands. Emitted on every successful
    // (re)init, including spotifyd subprocess startup and embedded
    // librespot Spirc handshake.
    PlayerReady {
        device_id: String,
        name: String,
    },

    // PlayerDegraded: a transient backend hiccup the daemon expects to
    // recover from (Spirc outer-timeout, audio sink panic budget warn).
    // Does NOT clear creds. Treated as best-effort UI signal — see
    // PlayerFailed for the terminal case.
    PlayerDegraded {
        reason: String,
    },

    /// The installed local-player facet cannot play because of a provider
    /// policy (for example, account tier or regional availability). The
    /// daemon redacts `reason` before this event reaches the wire.
    ProviderPolicy {
        provider: ProviderId,
        reason: String,
    },

    /// Exact provider-policy identity that recovered. Carrying the old reason
    /// prevents a delayed clear from removing a newer policy for the provider.
    ProviderPolicyCleared {
        provider: ProviderId,
        reason: String,
    },

    // Released-daemon compatibility only. New daemons emit ProviderPolicy;
    // retaining this unit variant lets current clients decode historical or
    // in-flight `premium-required` events during a rolling upgrade.
    PremiumRequired,

    // SessionDisconnected: librespot Session went invalid (network drop,
    // server boot, etc.). Daemon will attempt cached-creds recovery.
    SessionDisconnected {
        reason: String,
    },

    // PlayerFailed: terminal backend failure after the restart budget
    // ran out. CLI commands that need playback return errors until the
    // user runs `spotuify reconnect`.
    PlayerFailed {
        reason: String,
        restarts: u32,
    },

    // --- Phase 10: analytics lifecycle ---
    //
    // ListenQualified: emitted when a listen crosses the qualification
    // threshold. Drives the shell-hook bridge for ListenBrainz / Last.fm
    // scrobblers and unlocks the in-tree Wrapped-style metrics.
    ListenQualified {
        track_uri: String,
        duration_ms: i64,
        audible_ms: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artist_uri: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        album_uri: Option<String>,
    },

    /// Progress/status for daemon-owned historical analytics imports.
    /// TUI clients can refresh analytics panels on this event; CLIs use
    /// the direct command response/status subcommands.
    AnalyticsImportProgress {
        run_id: String,
        provider: String,
        username: String,
        phase: String,
        fetched: u64,
        stored: u64,
        resolved: u64,
        promoted: u64,
        unresolved: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },

    // --- Phase 12: operation log lifecycle ---
    //
    // OperationRecorded: every mutating handler emits one of these when
    // the operations row goes from pending → succeeded/failed. Drives the
    // TUI Operations panel refresh.
    OperationRecorded {
        operation_id: OperationId,
        kind: OperationKind,
        source: OperationSource,
    },

    // OperationUndone: emitted after `ops undo` (or MCP undo_last) commits
    // the inverse operation. `success=false` means the reversal failed
    // (conflict without --force, target deleted, etc.).
    OperationUndone {
        undo_op_id: OperationId,
        original_op_id: OperationId,
        success: bool,
    },

    /// Phase 13 (P13-I) — emitted after `Request::Reload` or `Reconnect`
    /// so TUI clients know to refresh their cached config view.
    ConfigReloaded,

    /// Phase 17 — real-time spectrum frame for the visualizer.
    ///
    /// Broadcast at the configured `target_fps` (default 30 Hz) while
    /// playback is active and the visualizer is enabled. `bands` is
    /// always length 12 (NUM_BANDS) with values normalized 0.0..=1.0.
    /// `peak` is the overall peak band magnitude. `timestamp_ms` is
    /// a monotonic capture time for client-side jitter compensation.
    SpectrumFrame {
        bands: Vec<f32>,
        peak: f32,
        timestamp_ms: u64,
    },

    /// Phase 17 — emitted when the visualizer's active source changes
    /// (config change, backend swap, loopback device hot-plug). TUI
    /// clients refresh hint bars + doctor reports. `active` is the
    /// rich `VizActiveSource` so loopback variants (cpal vs pipewire)
    /// are visible in the UI.
    VizSourceChanged {
        active: VizActiveSource,
        configured: VizSourceKindData,
        /// Phase 7 — human-readable setup hint surfaced verbatim in the
        /// TUI when the active source is `None` ("install BlackHole",
        /// "switch to embedded backend", etc).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hint: Option<String>,
        /// Legacy backend label at the moment of the change. Kept as a
        /// string for wire compatibility; player selection is provider-owned.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        backend_kind: Option<String>,
    },

    // --- Listening reminders ---
    /// A reminder fired: a new inbox notification exists. Carries the full
    /// notification so subscribers can show it without a follow-up fetch.
    ReminderDue {
        notification: Notification,
    },
    /// Reminder schedules changed (created / cancelled / acted). Clients
    /// re-sync their reminder list (and macOS re-schedules OS notifications).
    RemindersChanged {
        action: String,
    },

    // --- Update awareness ---
    /// Emitted once when the daemon first observes a newer GitHub release than
    /// the running build. Clients show an upgrade banner with `upgrade.command`.
    UpdateAvailable {
        latest_version: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        release_url: Option<String>,
        upgrade: UpgradeHint,
    },
    /// The daemon resolved to first-party-only Spotify auth, which Spotify
    /// rate-limits heavily (chronic 429s). Clients show a dismissible
    /// banner recommending migration to dev-app auth. `can_login_dev_app`
    /// is `true` when a dev-app client_id is configured (so
    /// `spotuify login --dev-app` works); `false` means recommend
    /// `spotuify onboard` (the user has no BYO app yet). Carries only a
    /// bool — never any credential material.
    AuthMigrationRecommended {
        can_login_dev_app: bool,
    },
    /// Forward-compat: an event variant this build doesn't know.
    /// Clients ignore it instead of killing the whole IPC stream the
    /// way an unknown tag used to.
    #[serde(other)]
    Unknown,
}

/// Redact token-shaped substrings before user-visible events are logged,
/// stored, or broadcast. This intentionally targets long credential-like
/// blobs, not ordinary short IDs or Spotify URIs.
pub fn redact_sensitive_text(input: &str) -> String {
    redact_sensitive_text_with(input, looks_sensitive_token)
}

fn redact_sensitive_text_with(input: &str, predicate: fn(&str) -> bool) -> String {
    let contextual = redact_labeled_secrets(input);
    let mut out = String::with_capacity(contextual.len());
    let mut run = String::new();
    for ch in contextual.chars() {
        if is_token_char(ch) {
            run.push(ch);
        } else {
            flush_redaction_run(&mut out, &mut run, predicate);
            out.push(ch);
        }
    }
    flush_redaction_run(&mut out, &mut run, predicate);
    out
}

const SECRET_LABELS: &[&str] = &[
    "access_token",
    "access-token",
    "refresh_token",
    "refresh-token",
    "client_secret",
    "client-secret",
    "authorization",
    "password",
    "passwd",
    "api_key",
    "api-key",
    "apikey",
    "secret",
    "token",
];

/// Redact credentials whose short value is only identifiable from a nearby
/// label. The entropy-based pass below cannot safely classify values such as
/// `x`, so recognize common assignment, JSON, and Authorization-header forms
/// before token-shape redaction.
fn redact_labeled_secrets(input: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut copied_to = 0;
    let mut search_from = 0;

    while let Some((start, end)) = next_labeled_secret_span(input, &lower, search_from) {
        out.push_str(&input[copied_to..start]);
        out.push_str("<redacted>");
        copied_to = end;
        search_from = end;
    }
    out.push_str(&input[copied_to..]);
    out
}

fn next_labeled_secret_span(input: &str, lower: &str, from: usize) -> Option<(usize, usize)> {
    SECRET_LABELS
        .iter()
        .flat_map(|label| {
            lower[from..]
                .match_indices(label)
                .filter_map(move |(offset, _)| labeled_secret_span(input, from + offset, label))
        })
        .min_by_key(|(start, _)| *start)
}

fn labeled_secret_span(input: &str, key_start: usize, label: &str) -> Option<(usize, usize)> {
    let bytes = input.as_bytes();
    if key_start > 0 && is_secret_label_char(bytes[key_start - 1]) {
        return None;
    }
    let key_end = key_start + label.len();
    if key_end < bytes.len() && is_secret_label_char(bytes[key_end]) {
        return None;
    }

    let quoted_key = key_start > 0 && matches!(bytes[key_start - 1], b'\'' | b'"');
    let key_quote = quoted_key.then(|| bytes[key_start - 1]);
    let mut cursor = key_end;
    if let Some(quote) = key_quote {
        if bytes.get(cursor).copied() != Some(quote) {
            return None;
        }
        cursor += 1;
    }
    cursor = skip_ascii_whitespace(bytes, cursor);
    if !matches!(bytes.get(cursor), Some(b'=') | Some(b':')) {
        return None;
    }
    cursor += 1;
    cursor = skip_ascii_whitespace(bytes, cursor);

    let value_end = if label == "authorization" {
        consume_authorization_value(input, cursor)?
    } else {
        consume_secret_value(input, cursor)?
    };
    let start = if quoted_key { key_start - 1 } else { key_start };
    Some((start, value_end))
}

fn consume_secret_value(input: &str, start: usize) -> Option<usize> {
    let first = input[start..].chars().next()?;
    if matches!(first, '\'' | '"') {
        let value_start = start + first.len_utf8();
        let mut escaped = false;
        for (offset, ch) in input[value_start..].char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == first {
                return (offset > 0).then_some(value_start + offset + first.len_utf8());
            }
        }
        return None;
    }

    let mut end = start;
    for (offset, ch) in input[start..].char_indices() {
        if ch.is_whitespace() || matches!(ch, ',' | ';' | '}' | ']' | ')') {
            break;
        }
        end = start + offset + ch.len_utf8();
    }
    (end > start).then_some(end)
}

fn consume_authorization_value(input: &str, start: usize) -> Option<usize> {
    if matches!(input[start..].chars().next()?, '\'' | '"') {
        return consume_secret_value(input, start);
    }
    let scheme_end = consume_secret_value(input, start)?;
    let credential_start = skip_ascii_whitespace(input.as_bytes(), scheme_end);
    if credential_start == scheme_end {
        return Some(scheme_end);
    }
    consume_secret_value(input, credential_start)
}

fn skip_ascii_whitespace(bytes: &[u8], mut cursor: usize) -> usize {
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    cursor
}

fn is_secret_label_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

/// Maximum user-visible size of a provider-policy explanation. This is a
/// character bound rather than a byte bound so truncation cannot split UTF-8.
pub const PROVIDER_POLICY_REASON_MAX_CHARS: usize = 512;

/// Redact and bound a provider-policy explanation before it reaches IPC,
/// diagnostics, or a client renderer.
pub fn sanitize_provider_policy_reason(input: &str) -> String {
    // Policy reasons are adapter-authored and can quote an upstream response.
    // Be stricter here than the shared event redactor: any uninterrupted
    // alphabetic credential-length run is hidden from IPC. Keeping this rule
    // policy-specific avoids erasing unusually long prose words in unrelated
    // search, mutation, and lifecycle diagnostics.
    let redacted = redact_sensitive_text_with(input, looks_sensitive_provider_policy_token);
    let mut chars = redacted.chars();
    let prefix = chars
        .by_ref()
        .take(PROVIDER_POLICY_REASON_MAX_CHARS)
        .collect::<String>();
    if chars.next().is_none() {
        return prefix;
    }

    let mut bounded = prefix
        .chars()
        .take(PROVIDER_POLICY_REASON_MAX_CHARS.saturating_sub(1))
        .collect::<String>();
    bounded.push('…');
    bounded
}

pub fn sanitize_daemon_event(event: DaemonEvent) -> DaemonEvent {
    match event {
        DaemonEvent::SearchFailed {
            query,
            version,
            kind,
            offset,
            message,
            provider,
        } => DaemonEvent::SearchFailed {
            query,
            version,
            kind,
            offset,
            message: redact_sensitive_text(&message),
            provider,
        },
        DaemonEvent::MutationFinished { action, message } => DaemonEvent::MutationFinished {
            action,
            message: redact_sensitive_text(&message),
        },
        DaemonEvent::MutationFinalized {
            receipt_id,
            status,
            message,
        } => DaemonEvent::MutationFinalized {
            receipt_id,
            status,
            message: redact_sensitive_text(&message),
        },
        DaemonEvent::PlayerDegraded { reason } => DaemonEvent::PlayerDegraded {
            reason: redact_sensitive_text(&reason),
        },
        DaemonEvent::ProviderPolicy { provider, reason } => DaemonEvent::ProviderPolicy {
            provider,
            reason: sanitize_provider_policy_reason(&reason),
        },
        DaemonEvent::ProviderPolicyCleared { provider, reason } => {
            DaemonEvent::ProviderPolicyCleared {
                provider,
                reason: sanitize_provider_policy_reason(&reason),
            }
        }
        DaemonEvent::SessionDisconnected { reason } => DaemonEvent::SessionDisconnected {
            reason: redact_sensitive_text(&reason),
        },
        DaemonEvent::PlayerFailed { reason, restarts } => DaemonEvent::PlayerFailed {
            reason: redact_sensitive_text(&reason),
            restarts,
        },
        DaemonEvent::AnalyticsImportProgress {
            run_id,
            provider,
            username,
            phase,
            fetched,
            stored,
            resolved,
            promoted,
            unresolved,
            message,
        } => DaemonEvent::AnalyticsImportProgress {
            run_id,
            provider,
            username,
            phase,
            fetched,
            stored,
            resolved,
            promoted,
            unresolved,
            message: message.map(|message| redact_sensitive_text(&message)),
        },
        other => other,
    }
}

/// Adapt an event to the capabilities declared by one subscriber.
///
/// Released v7 clients had `premium-required` but no catch-all event variant,
/// so sending them a generic provider-policy event terminates their stream.
/// Preserve the one exact historical semantic and suppress policy events that
/// cannot be represented honestly on that wire.
pub fn daemon_event_for_subscriber(
    event: DaemonEvent,
    provider_policy_capable: bool,
) -> Option<DaemonEvent> {
    if provider_policy_capable {
        return Some(event);
    }
    match event {
        DaemonEvent::ProviderPolicy { provider, reason }
            if provider.as_str() == "spotify"
                && reason == spotuify_core::PREMIUM_REQUIRED_POLICY_REASON =>
        {
            Some(DaemonEvent::PremiumRequired)
        }
        DaemonEvent::ProviderPolicy { .. } | DaemonEvent::ProviderPolicyCleared { .. } => None,
        event => Some(event),
    }
}

fn is_token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '+' | '/' | '=')
}

fn flush_redaction_run(out: &mut String, run: &mut String, predicate: fn(&str) -> bool) {
    if run.is_empty() {
        return;
    }
    if predicate(run) {
        out.push_str("<redacted>");
    } else {
        out.push_str(run);
    }
    run.clear();
}

fn looks_sensitive_token(value: &str) -> bool {
    if looks_sensitive_assignment(value) {
        return true;
    }
    if value.len() < 32 || !value.chars().any(|ch| ch.is_ascii_alphabetic()) {
        return false;
    }

    let has_digit = value.chars().any(|ch| ch.is_ascii_digit());
    let has_strong_token_punctuation = value.chars().any(|ch| matches!(ch, '_' | '+' | '/' | '='));
    let has_upper = value.chars().any(|ch| ch.is_ascii_uppercase());
    let has_lower = value.chars().any(|ch| ch.is_ascii_lowercase());
    let all_alpha = value.chars().all(|ch| ch.is_ascii_alphabetic());

    has_digit
        || has_strong_token_punctuation
        // Long mixed-case runs are characteristic of base64/base64url even
        // when a particular opaque credential happens to contain no digit.
        || (value.len() >= 40 && has_upper && has_lower)
        // Keep real prose words (including the longest common dictionary
        // examples) visible while still covering long all-alpha credentials.
        || (value.len() >= 48 && all_alpha)
}

fn looks_sensitive_assignment(value: &str) -> bool {
    let Some((key, assigned)) = value.split_once('=') else {
        return false;
    };
    if assigned.is_empty() {
        return false;
    }
    matches!(
        key.to_ascii_lowercase().as_str(),
        "token"
            | "access_token"
            | "access-token"
            | "refresh_token"
            | "refresh-token"
            | "api_key"
            | "api-key"
            | "apikey"
            | "client_secret"
            | "client-secret"
            | "secret"
            | "password"
            | "passwd"
            | "authorization"
    )
}

fn looks_sensitive_provider_policy_token(value: &str) -> bool {
    looks_sensitive_token(value)
        || (value.len() >= 32 && value.chars().all(|ch| ch.is_ascii_alphabetic()))
}

/// Phase 17 — wire-format viz source kind. Mirrors
/// `spotuify_audio::VizSourceKind` so the protocol crate stays free of
/// audio-implementation dependencies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VizSourceKindData {
    #[default]
    Auto,
    Sink,
    Loopback,
    None,
}

impl VizSourceKindData {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "sink" => Self::Sink,
            "loopback" => Self::Loopback,
            "none" | "off" | "disabled" => Self::None,
            _ => Self::Auto,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Sink => "sink",
            Self::Loopback => "loopback",
            Self::None => "none",
        }
    }
}

#[cfg(test)]
mod request_category_tests {
    use super::{IpcCategory, PlaybackCommand, Request, SearchScopeData, SearchSourceData};

    #[test]
    fn request_categories_keep_music_platform_admin_and_client_state_separate() {
        assert_eq!(Request::PlaybackGet.category(), IpcCategory::CoreMusic);
        assert_eq!(
            Request::Search {
                query: "bowie".to_string(),
                scope: SearchScopeData::All,
                source: SearchSourceData::Hybrid,
                limit: 10,
                provider: None,
                kinds: None,
                sort: None,
            }
            .category(),
            IpcCategory::CoreMusic
        );
        assert_eq!(
            Request::PlaybackCommand {
                command: PlaybackCommand::Pause,
            }
            .category(),
            IpcCategory::CoreMusic
        );
        assert_eq!(
            Request::CacheStatus.category(),
            IpcCategory::SpotuifyPlatform
        );
        assert_eq!(
            Request::SubscribeEvents {
                provider_policy: true,
            }
            .category(),
            IpcCategory::AdminMaintenance
        );
        assert_eq!(Request::ClientSeed.category(), IpcCategory::ClientSpecific);
        assert_eq!(Request::ProvidersList.category(), IpcCategory::CoreMusic);
        assert_eq!(
            Request::ResolveTarget {
                input: "spotify:track:one".to_string(),
                provider: None,
                expected_kinds: None,
            }
            .category(),
            IpcCategory::CoreMusic
        );
        assert_eq!(
            Request::ListAudioOutputs.category(),
            IpcCategory::AdminMaintenance
        );
        assert_eq!(
            Request::SetVizFocus { focused: true }.category(),
            IpcCategory::ClientSpecific
        );
    }
}

/// Phase 17 — concrete active source as reported by doctor + viz status.
/// Distinguishes which loopback implementation is in use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VizActiveSource {
    Sink,
    LoopbackCpal,
    LoopbackPipewire,
    #[default]
    None,
}

/// Phase 17 — diagnostics surfaced by `Request::GetVizStatus` and embedded
/// in `DoctorReport.viz`. Provides everything the CLI/TUI/doctor need to
/// explain what the visualizer is doing right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VizDiagnostics {
    pub enabled: bool,
    pub configured_source: VizSourceKindData,
    pub active_source: VizActiveSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loopback_device_name: Option<String>,
    pub dropped_frames_5min: u64,
    pub target_fps: u8,
    /// Optional human-readable setup hint (e.g. macOS BlackHole install).
    /// Surfaced verbatim in `doctor` output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Phase 0 (observability) — `true` when the daemon currently has
    /// playback active. Lets the TUI distinguish "flat spectrum because
    /// nothing is playing" from "flat spectrum because no PCM source".
    #[serde(default)]
    pub playing: bool,
    /// Phase 0 — milliseconds since the analyzer last produced a frame.
    /// `None` when never produced. > 2000 typically means the source
    /// stalled (loopback device disappeared, embedded sink went silent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_frame_age_ms: Option<u64>,
    /// Legacy playback backend label at diagnostics time. Kept as a string
    /// for wire compatibility; player selection is provider-owned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_kind: Option<String>,
}

impl Default for VizDiagnostics {
    fn default() -> Self {
        Self {
            enabled: false,
            configured_source: VizSourceKindData::Auto,
            active_source: VizActiveSource::None,
            sample_rate: None,
            loopback_device_name: None,
            dropped_frames_5min: 0,
            target_fps: 30,
            hint: None,
            playing: false,
            last_frame_age_ms: None,
            backend_kind: None,
        }
    }
}

/// Auth error categories. Mirrors `spotuify_spotify::error::AuthErrorKind`
/// so the daemon event stream stays typed without dragging the Spotify
/// crate into the protocol. Stable; remapping is a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthErrorKind {
    /// No stored Spotify credentials exist yet. Recovery: run login.
    NotLoggedIn,
    ExpiredRefresh,
    InvalidGrant,
    Forbidden,
    /// Stored token lacks one or more required scopes. Emitted at daemon
    /// startup so the TUI can prompt re-auth proactively.
    ScopeReauthRequired,
}

/// Phase 6.6 mutation receipt — two-stage lifecycle.
///
/// Distinct from the legacy [`CommandReceipt`] (which is synchronous
/// {ok, action, message}). A `Receipt` is persisted to SQLite at issue
/// time so it survives daemon crash; the daemon recovers pending receipts
/// at startup and reconciles them.
///
/// Lifecycle:
///   Pending → MutationAccepted event
///   Pending → Confirmed → MutationFinalized event
///   Pending → Failed     → MutationFinalized event
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Receipt {
    pub receipt_id: ReceiptId,
    pub action: String,
    pub status: ReceiptStatus,
    pub message: String,
    pub started_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiErrorSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    Pending,
    Confirmed,
    Failed,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Stable identifier for a daemon-owned interactive auth attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AuthSessionId(pub uuid::Uuid);

impl AuthSessionId {
    pub fn new_v7() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}

impl std::fmt::Display for AuthSessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for AuthSessionId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        uuid::Uuid::parse_str(value).map(Self)
    }
}

/// Serializable state machine for interactive provider authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AuthSessionState {
    Starting,
    AwaitingUser {
        authorization_url: String,
        redirect_uri: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        browser_error: Option<String>,
    },
    Waiting {
        authorization_url: String,
        redirect_uri: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        browser_error: Option<String>,
    },
    Authorized,
    Failed {
        message: String,
    },
    Cancelled,
}

impl AuthSessionState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Authorized | Self::Failed { .. } | Self::Cancelled
        )
    }
}

/// Pollable snapshot for one daemon-owned authentication session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthSessionData {
    pub session_id: AuthSessionId,
    pub provider: ProviderId,
    pub method: String,
    pub state: AuthSessionState,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
}

/// Provider auth behavior selected by the configured adapter factory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthStrategyData {
    None,
    SpotifyOauth,
}

/// Provider-selected authentication method for the next unqualified login.
///
/// Clients treat this as presentation/setup metadata only. The daemon remains
/// authoritative and resolves the method again when `AuthStart` arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethodData {
    DevApp,
    FirstParty,
}

/// Credential kinds whose presence can be reported without exposing secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthCredentialKind {
    DevApp,
    FirstParty,
}

/// Secret-free metadata for one credential kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthCredentialStatus {
    pub kind: AuthCredentialKind,
    pub present: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub missing_scopes: Vec<String>,
}

/// Daemon-owned auth status. This type deliberately has no token fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthStatusData {
    pub provider: ProviderId,
    pub strategy: AuthStrategyData,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_method: Option<AuthMethodData>,
    pub auth_required: bool,
    pub auth_revoked: bool,
    #[serde(default)]
    pub credentials: Vec<AuthCredentialStatus>,
}

/// Result of an atomic provider logout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthLogoutData {
    pub provider: ProviderId,
    pub removed_dev_app: bool,
    pub removed_first_party: bool,
    pub removed_librespot: bool,
    pub auth_required: bool,
}

/// Retry key for a remote mutation. UUIDv7 keeps keys sortable while the
/// newtype prevents accidental interchange with receipt/operation ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MutationId(pub uuid::Uuid);

impl MutationId {
    pub fn new_v7() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}

impl std::fmt::Display for MutationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for MutationId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        uuid::Uuid::parse_str(value).map(Self)
    }
}

/// Newtype around UUID v7 so the serialization is stable and the type is
/// distinct from arbitrary strings in API surfaces. v7 is sortable by
/// insertion time which keeps `ops log` chronological for free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReceiptId(pub uuid::Uuid);

impl ReceiptId {
    pub fn new_v7() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}

impl std::fmt::Display for ReceiptId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Compact summary of a Spotify API failure for embedding in
/// `Receipt.error`. We deliberately don't carry the full response body
/// across IPC -- it's redundant noise and may include URIs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiErrorSummary {
    pub kind: IpcErrorKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonStatus {
    pub running: bool,
    pub socket_path: String,
    pub socket_exists: bool,
    pub socket_reachable: bool,
    pub stale_socket: bool,
    pub daemon_pid: Option<u32>,
    pub uptime_secs: Option<u64>,
    pub protocol_version: u32,
    pub daemon_version: Option<String>,
    pub daemon_build_id: Option<String>,
    /// Live embedded-player audio-flow health, when the embedded backend is
    /// active. `None` for non-embedded backends / older daemons. Carried here
    /// (on the proven `GetDaemonStatus` path) so `doctor` can surface it
    /// without `GetDoctorReport`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_health: Option<AudioHealth>,
}

/// Embedded-player audio-flow snapshot for diagnostics. Lets `doctor`
/// distinguish a session/network drop (`connected=false`) from an
/// audio-route/keepalive failure (`connected=true, samples_advancing=false`
/// while playing — "playing but silent").
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AudioHealth {
    pub connected: bool,
    pub is_playing: bool,
    pub we_are_active: bool,
    pub samples_advancing: bool,
    pub reconnect_attempts: u32,
    pub current_backoff_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_stall_ms: Option<i64>,
}

/// Phase 13 (P13-K) — three-variant health class. `Unhealthy` is
/// distinct from `Degraded` so monitoring scripts and the doctor TUI
/// can act differently on "running with a soft failure" vs "cannot
/// reach Spotify / no auth / daemon down".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum HealthClass {
    #[default]
    Healthy,
    Degraded,
    Unhealthy,
}

impl HealthClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Unhealthy => "unhealthy",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DoctorFindingSeverity {
    #[default]
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DoctorFindingCategory {
    Auth,
    Config,
    Daemon,
    Device,
    Network,
    Player,
    #[default]
    Generic,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DoctorFinding {
    pub category: DoctorFindingCategory,
    pub severity: DoctorFindingSeverity,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remediation: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DoctorCheck {
    pub name: String,
    pub ok: bool,
    pub message: String,
    /// Wall-clock duration of the check, in milliseconds. MUST stay `u64`:
    /// `serde_json` (built here without `arbitrary_precision`) cannot
    /// serialize `u128` and returns "u128 is not supported", which failed
    /// the whole `DoctorReport` encode and silently closed the IPC
    /// connection (the GetDoctorReport "Connection closed" bug). A timing
    /// in milliseconds fits `u64` for ~584 million years.
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceSummary {
    pub name: String,
    pub kind: String,
    pub active: bool,
    pub restricted: bool,
    pub has_id: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceDiagnostics {
    pub preferred_configured: Option<String>,
    pub preferred_visible: bool,
    pub active_device: Option<DeviceSummary>,
    pub restricted_devices: Vec<DeviceSummary>,
    pub visible_unrestricted_devices: Vec<DeviceSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DoctorReport {
    pub healthy: bool,
    pub health_class: HealthClass,
    pub config_path: String,
    pub config_ok: bool,
    pub config_error: Option<String>,
    pub logs_path: String,
    pub client_id: Option<String>,
    pub client_secret_present: Option<bool>,
    pub redirect_uri: Option<String>,
    pub keychain_token: DoctorCheck,
    pub daemon: DaemonStatus,
    pub api_checks: Vec<DoctorCheck>,
    pub device_diagnostics: Option<DeviceDiagnostics>,
    pub recommended_next_steps: Vec<String>,
    pub findings: Vec<DoctorFinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemDiagnostics>,
    /// Phase 17 — audio visualization diagnostics. None when viz is
    /// off (default); Some(_) when it has been enabled at any point
    /// in the current daemon lifetime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viz: Option<VizDiagnostics>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SystemDiagnostics {
    pub media_controls_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_controls_bus_name: Option<String>,
    pub hooks_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_timeout_ms: Option<u64>,
    pub notifications_enabled: bool,
    pub discord_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discord_application_id: Option<String>,
}

/// Maximum size of a single length-delimited IPC frame.
///
/// 16 MiB is deliberately above tokio-util's 8 MB default: legitimate
/// frames carry album-art byte payloads (`Image`/`CoverArt`) and
/// `ClientSeed` snapshots with hundreds of queue items. The socket is
/// local-only (0600, owner-only), so the larger cap is not a
/// memory-DoS surface; oversize frames are still rejected by the
/// codec before any allocation of the payload.
pub const MAX_IPC_FRAME_BYTES: usize = 16 * 1024 * 1024;

pub struct IpcCodec {
    inner: LengthDelimitedCodec,
}

impl IpcCodec {
    pub fn new() -> Self {
        Self {
            inner: LengthDelimitedCodec::builder()
                .length_field_length(4)
                .max_frame_length(MAX_IPC_FRAME_BYTES)
                .new_codec(),
        }
    }
}

impl Default for IpcCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder for IpcCodec {
    type Item = IpcMessage;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        match self.inner.decode(src)? {
            Some(frame) => serde_json::from_slice(&frame)
                .map(Some)
                .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err)),
            None => Ok(None),
        }
    }
}

impl Encoder<IpcMessage> for IpcCodec {
    type Error = std::io::Error;

    fn encode(&mut self, item: IpcMessage, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let json = serde_json::to_vec(&item).map_err(|err| {
            // A serialize failure here propagates as an `io::Error`; the
            // sink returns `Err` and the connection task tears the socket
            // down. Historically that happened with no log, so the client
            // saw a bare EOF ("Connection closed") and the cause was
            // invisible — this is exactly how the GetDoctorReport response
            // failure stayed hidden. Name the payload so it is greppable.
            tracing::error!(
                request_id = item.id,
                payload = item.payload.kind_label(),
                error = %err,
                "failed to serialize IPC message for the wire",
            );
            std::io::Error::new(std::io::ErrorKind::InvalidData, err)
        })?;
        self.inner.encode(json.into(), dst)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic, clippy::unwrap_used)]

    use super::{
        redact_sensitive_text, sanitize_daemon_event, sanitize_provider_policy_reason,
        AuthCredentialKind, AuthCredentialStatus, AuthMethodData, AuthSessionData, AuthSessionId,
        AuthSessionState, AuthStatusData, AuthStrategyData, DaemonEvent, EpisodeSort, IpcCategory,
        IpcErrorKind, IpcMessage, IpcPayload, PlaybackCommand, Request, Response, ResponseData,
        SearchSortData, UpgradeHint, UpgradeMethod, PROVIDER_POLICY_REASON_MAX_CHARS,
    };
    use spotuify_core::ProviderId;

    #[test]
    fn codec_rejects_frames_larger_than_the_documented_limit() {
        // The 4-byte length prefix is attacker-influencable on a shared
        // socket; the codec must refuse oversize frames up front instead
        // of allocating for them.
        use tokio_util::codec::Decoder as _;
        let mut codec = super::IpcCodec::new();
        let oversize = (super::MAX_IPC_FRAME_BYTES as u32) + 1;
        let mut src = bytes::BytesMut::new();
        src.extend_from_slice(&oversize.to_be_bytes());
        src.extend_from_slice(&[0_u8; 16]);
        let err = codec.decode(&mut src).expect_err("oversize frame accepted");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);

        // A frame at the boundary still decodes (truncated payload just
        // yields Ok(None) while the codec waits for more bytes).
        let mut codec = super::IpcCodec::new();
        let mut src = bytes::BytesMut::new();
        src.extend_from_slice(&(super::MAX_IPC_FRAME_BYTES as u32).to_be_bytes());
        src.extend_from_slice(&[0_u8; 16]);
        assert!(codec
            .decode(&mut src)
            .expect("boundary frame rejected")
            .is_none());
    }

    #[test]
    fn doctor_report_round_trips_through_the_codec() {
        // Regression for the GetDoctorReport "Connection closed" bug: a
        // fully-populated DoctorReport response must encode AND decode
        // cleanly through `IpcCodec`. The original failure was a response
        // payload that `serde_json::to_vec` rejected at the encoder, which
        // the send site swallowed and turned into a silent socket EOF.
        // Exercising every Option=Some / non-empty-Vec field across the
        // report and its nested diagnostics (DaemonStatus, AudioHealth,
        // DeviceDiagnostics, SystemDiagnostics, VizDiagnostics) guards every
        // future field addition against the same regression.
        use tokio_util::codec::{Decoder as _, Encoder as _};

        let check = |name: &str| super::DoctorCheck {
            name: name.to_string(),
            ok: true,
            message: "ok".to_string(),
            elapsed_ms: 12,
        };
        let device = super::DeviceSummary {
            name: "spotuify-hume".to_string(),
            kind: "computer".to_string(),
            active: true,
            restricted: false,
            has_id: true,
        };
        let report = super::DoctorReport {
            healthy: true,
            health_class: super::HealthClass::Degraded,
            config_path: "/cfg/config.toml".to_string(),
            config_ok: true,
            config_error: Some("none".to_string()),
            logs_path: "/logs/spotuify.log".to_string(),
            client_id: Some("client".to_string()),
            client_secret_present: Some(true),
            redirect_uri: Some("http://127.0.0.1/callback".to_string()),
            keychain_token: check("keychain"),
            daemon: super::DaemonStatus {
                running: true,
                socket_path: "/sock".to_string(),
                socket_exists: true,
                socket_reachable: true,
                stale_socket: false,
                daemon_pid: Some(4242),
                uptime_secs: Some(99),
                protocol_version: 1,
                daemon_version: Some("0.1.71".to_string()),
                daemon_build_id: Some("abc123".to_string()),
                audio_health: Some(super::AudioHealth {
                    connected: true,
                    is_playing: true,
                    we_are_active: true,
                    samples_advancing: false,
                    reconnect_attempts: 2,
                    current_backoff_ms: 4_000,
                    last_stall_ms: Some(1_234),
                }),
            },
            api_checks: vec![check("web-api"), check("playlists")],
            device_diagnostics: Some(super::DeviceDiagnostics {
                preferred_configured: Some("spotuify-hume".to_string()),
                preferred_visible: true,
                active_device: Some(device.clone()),
                restricted_devices: vec![device.clone()],
                visible_unrestricted_devices: vec![device.clone()],
            }),
            recommended_next_steps: vec!["spotuify reconnect".to_string()],
            findings: vec![super::DoctorFinding {
                category: super::DoctorFindingCategory::Player,
                severity: super::DoctorFindingSeverity::Warning,
                message: "connected but no audio flowing".to_string(),
                remediation: vec!["spotuify reconnect".to_string()],
            }],
            system: Some(super::SystemDiagnostics {
                media_controls_enabled: true,
                media_controls_bus_name: Some("org.mpris.MediaPlayer2.spotuify".to_string()),
                hooks_enabled: true,
                hook_command: Some("/bin/hook".to_string()),
                hook_timeout_ms: Some(500),
                notifications_enabled: true,
                discord_enabled: true,
                discord_application_id: Some("123".to_string()),
            }),
            viz: Some(super::VizDiagnostics {
                enabled: true,
                sample_rate: Some(44_100),
                loopback_device_name: Some("BlackHole".to_string()),
                hint: Some("install BlackHole".to_string()),
                playing: true,
                last_frame_age_ms: Some(16),
                backend_kind: Some("embedded".to_string()),
                ..Default::default()
            }),
        };

        let message = IpcMessage {
            id: 7,
            source: None,
            mutation_id: None,
            payload: IpcPayload::Response(Response::Ok {
                data: ResponseData::DoctorReport {
                    report: report.clone(),
                },
            }),
        };

        let mut codec = super::IpcCodec::new();
        let mut buf = bytes::BytesMut::new();
        codec
            .encode(message, &mut buf)
            .expect("a fully-populated DoctorReport must serialize for the wire");
        let decoded = codec
            .decode(&mut buf)
            .expect("decoding the encoded report must not error")
            .expect("a complete frame must be present after encoding");

        let IpcPayload::Response(Response::Ok {
            data: ResponseData::DoctorReport {
                report: decoded_report,
            },
        }) = decoded.payload
        else {
            panic!("decoded payload was not a DoctorReport response");
        };
        assert_eq!(decoded_report, report);
    }

    #[test]
    fn error_kind_roundtrips_and_typed_constructor_sets_code_and_retryability() {
        // The CLI keys off the exact `IpcErrorKind` to decide whether
        // to prompt for re-auth; daemon call sites must set it
        // structurally rather than relying on message text.
        let response =
            Response::error_with_kind("Spotify refresh token revoked", IpcErrorKind::AuthRevoked);
        assert!(matches!(
            response,
            Response::Error {
                kind: IpcErrorKind::AuthRevoked,
                ref code,
                retryable: false,
                ..
            } if code == "auth_revoked"
        ));
        let retryable =
            Response::error_with_kind("Spotify rate limited", IpcErrorKind::RateLimited);
        assert!(matches!(
            retryable,
            Response::Error {
                kind: IpcErrorKind::RateLimited,
                retryable: true,
                ..
            }
        ));
        assert!(matches!(
            Response::error("plain internal failure"),
            Response::Error {
                kind: IpcErrorKind::Internal,
                ..
            }
        ));
        // JSON round-trip via serde.
        let json = serde_json::to_string(&IpcErrorKind::AuthRevoked).unwrap();
        assert_eq!(json, "\"auth_revoked\"");
        let back: IpcErrorKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, IpcErrorKind::AuthRevoked);
        // Wire code for log pivots.
        assert_eq!(IpcErrorKind::AuthRevoked.as_code(), "auth_revoked");
    }

    #[test]
    fn reload_auth_request_serializes_and_classifies_as_admin() {
        let req = Request::ReloadAuth;
        assert_eq!(req.kind_label(), "reload-auth");
        assert_eq!(req.category(), IpcCategory::AdminMaintenance);
        let json = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, Request::ReloadAuth));
    }

    #[test]
    fn auth_session_requests_and_state_roundtrip() {
        let session_id = AuthSessionId::new_v7();
        let defaults: Request = serde_json::from_str(r#"{"cmd":"auth-start"}"#).unwrap();
        assert_eq!(
            defaults,
            Request::AuthStart {
                provider: None,
                method: None,
            }
        );
        let requests = [
            Request::AuthStart {
                provider: Some(ProviderId::new("spotify").unwrap()),
                method: Some("dev_app".to_string()),
            },
            Request::AuthPoll { session_id },
            Request::AuthCancel { session_id },
            Request::AuthStatus {
                provider: Some(ProviderId::new("spotify").unwrap()),
            },
            Request::AuthLogout {
                provider: Some(ProviderId::new("spotify").unwrap()),
            },
        ];
        for request in requests {
            assert_eq!(request.category(), IpcCategory::AdminMaintenance);
            let json = serde_json::to_string(&request).unwrap();
            let decoded: Request = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, request);
        }

        let session = AuthSessionData {
            session_id,
            provider: ProviderId::new("spotify").unwrap(),
            method: "dev_app".to_string(),
            state: AuthSessionState::AwaitingUser {
                authorization_url: "https://accounts.spotify.test/authorize".to_string(),
                redirect_uri: "http://127.0.0.1:8888/callback".to_string(),
                browser_error: Some("headless".to_string()),
            },
            created_at_ms: 10,
            expires_at_ms: 20,
        };
        let json = serde_json::to_string(&ResponseData::AuthSession {
            session: session.clone(),
        })
        .unwrap();
        let decoded: ResponseData = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            decoded,
            ResponseData::AuthSession { session: decoded } if decoded == session
        ));
    }

    #[test]
    fn request_kind_labels_are_kebab_case_and_match_serde_tag() {
        // Phase 0 — the IPC span uses `kind_label()` as its `request_kind`
        // field. Every variant must return a kebab-case string that matches
        // the serde `cmd` tag, so log readers can pivot freely between
        // tracing JSON and wire payloads.
        let kinds = [
            (Request::Ping, "ping"),
            (Request::ClientSeed, "client-seed"),
            (Request::PlaybackGet, "playback-get"),
            (
                Request::PlaybackCommand {
                    command: PlaybackCommand::Pause,
                },
                "playback-command",
            ),
            (Request::QueueGet, "queue-get"),
            (Request::GetVizStatus, "get-viz-status"),
            (
                Request::SubscribeEvents {
                    provider_policy: true,
                },
                "subscribe-events",
            ),
        ];
        for (req, expected) in kinds {
            assert_eq!(req.kind_label(), expected, "kind_label for {req:?}");
        }
    }

    #[test]
    fn playback_command_labels_match_serde_tag() {
        let cases = [
            (PlaybackCommand::Pause, "pause"),
            (PlaybackCommand::Resume, "resume"),
            (PlaybackCommand::Toggle, "toggle"),
            (PlaybackCommand::Next, "next"),
            (PlaybackCommand::Previous, "previous"),
            (PlaybackCommand::Seek { position_ms: 0 }, "seek"),
            (
                PlaybackCommand::SeekRelative { offset_ms: 0 },
                "seek-relative",
            ),
        ];
        for (cmd, expected) in cases {
            assert_eq!(cmd.label(), expected, "label for {cmd:?}");
        }
    }

    #[test]
    fn ipc_category_labels_are_stable() {
        assert_eq!(IpcCategory::CoreMusic.label(), "core-music");
        assert_eq!(IpcCategory::SpotuifyPlatform.label(), "spotuify-platform");
        assert_eq!(IpcCategory::AdminMaintenance.label(), "admin-maintenance");
        assert_eq!(IpcCategory::ClientSpecific.label(), "client-specific");
    }

    #[test]
    fn request_category_links_to_kind_label_for_telemetry() {
        // The IPC span records both `request_kind` AND `category` so a
        // log dashboard can group by category. Spot-check that the two
        // labelings stay in sync.
        let pause = Request::PlaybackCommand {
            command: PlaybackCommand::Pause,
        };
        assert_eq!(pause.kind_label(), "playback-command");
        assert_eq!(pause.category(), IpcCategory::CoreMusic);
    }

    #[test]
    fn daemon_event_sanitizer_redacts_token_shaped_reasons() {
        let raw_token = "OWZhZWQzM2QtNjI1NC00MzEwLWFhZGMTNzEzZjBjMjM2U2VjcmV0MTIz";
        let event = sanitize_daemon_event(DaemonEvent::SessionDisconnected {
            reason: format!("session disconnected for {raw_token}; reconnecting"),
        });
        match event {
            DaemonEvent::SessionDisconnected { reason } => {
                assert!(reason.contains("<redacted>"));
                assert!(!reason.contains(raw_token));
            }
            other => panic!("expected SessionDisconnected, got {other:?}"),
        }

        let event = sanitize_daemon_event(DaemonEvent::ProviderPolicy {
            provider: ProviderId::new("nebula").unwrap(),
            reason: format!("region restricted for {raw_token}"),
        });
        match event {
            DaemonEvent::ProviderPolicy { provider, reason } => {
                assert_eq!(provider.as_str(), "nebula");
                assert!(reason.contains("<redacted>"));
                assert!(!reason.contains(raw_token));
            }
            other => panic!("expected ProviderPolicy, got {other:?}"),
        }
    }

    #[test]
    fn sensitive_text_redacts_alpha_only_credentials_without_eating_prose() {
        let alpha_only = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
        let all_lower = "a".repeat(64);
        let longest_prose_word = "pneumonoultramicroscopicsilicovolcanoconiosis";
        let hyphenated_prose = "provider-account-restriction-remains-actionable";

        assert_eq!(redact_sensitive_text(alpha_only), "<redacted>");
        assert_eq!(redact_sensitive_text(&all_lower), "<redacted>");
        assert_eq!(
            redact_sensitive_text(longest_prose_word),
            longest_prose_word
        );
        assert_eq!(redact_sensitive_text(hyphenated_prose), hyphenated_prose);
    }

    #[test]
    fn provider_policy_reason_is_redacted_and_unicode_safely_bounded() {
        let alpha_only = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
        let raw = format!("policy {alpha_only} {}", "🎵".repeat(600));
        let sanitized = sanitize_provider_policy_reason(&raw);

        assert!(!sanitized.contains(alpha_only));
        assert!(sanitized.contains("<redacted>"));
        assert_eq!(sanitized.chars().count(), PROVIDER_POLICY_REASON_MAX_CHARS);
        assert!(sanitized.ends_with('…'));
    }

    #[test]
    fn provider_policy_alpha_credentials_redact_at_exact_boundary() {
        let below = "a".repeat(31);
        let boundary = "b".repeat(32);
        let within_previous_gap = "c".repeat(47);

        assert_eq!(sanitize_provider_policy_reason(&below), below);
        assert_eq!(sanitize_provider_policy_reason(&boundary), "<redacted>");
        assert_eq!(
            sanitize_provider_policy_reason(&within_previous_gap),
            "<redacted>"
        );
    }

    #[test]
    fn short_secret_assignments_are_redacted_without_entropy_threshold() {
        for value in [
            "token=Ab1Cd2Ef3Gh4",
            "token=\"x\"",
            "token = x",
            "access_token=short",
            "{\"access_token\":\"short\"}",
            "refresh-token=tiny",
            "api_key=1234",
            "client_secret=x",
            "password=p",
            "Authorization: Bearer x",
        ] {
            let redacted = redact_sensitive_text(value);
            assert!(redacted.contains("<redacted>"), "{value}: {redacted}");
            assert!(!redacted.contains("short"), "{value}: {redacted}");
            assert!(!redacted.ends_with(" x"), "{value}: {redacted}");
            let policy = sanitize_provider_policy_reason(value);
            assert!(policy.contains("<redacted>"), "{value}: {policy}");
            assert!(!policy.contains("short"), "{value}: {policy}");
            assert!(!policy.ends_with(" x"), "{value}: {policy}");
        }
        assert_eq!(redact_sensitive_text("reason=ordinary"), "reason=ordinary");
        assert_eq!(
            redact_sensitive_text("ordinary provider policy prose remains visible"),
            "ordinary provider policy prose remains visible"
        );
    }

    #[test]
    fn quoted_secret_redaction_skips_escaped_delimiters_and_consumes_suffix() {
        let json = r#"{"password":"a\"b"}"#;
        let single_quoted = "token='a\\'b'";

        assert_eq!(redact_sensitive_text(json), "{<redacted>}");
        assert_eq!(redact_sensitive_text(single_quoted), "<redacted>");
        assert_eq!(sanitize_provider_policy_reason(json), "{<redacted>}");
        assert_eq!(sanitize_provider_policy_reason(single_quoted), "<redacted>");
        for sanitized in [
            redact_sensitive_text(json),
            redact_sensitive_text(single_quoted),
            sanitize_provider_policy_reason(json),
            sanitize_provider_policy_reason(single_quoted),
        ] {
            assert!(!sanitized.contains("b\""), "{sanitized}");
            assert!(!sanitized.contains("b'"), "{sanitized}");
        }
    }

    #[test]
    fn provider_policy_redactor_preserves_ordinary_prose() {
        let prose = "account tier or regional availability prevents local playback";
        assert_eq!(sanitize_provider_policy_reason(prose), prose);
    }

    #[test]
    fn seek_relative_round_trips_through_serde() {
        let raw = serde_json::to_string(&Request::PlaybackCommand {
            command: PlaybackCommand::SeekRelative { offset_ms: -30_000 },
        })
        .unwrap();
        assert!(raw.contains("\"cmd\":\"playback-command\""));
        // PlaybackCommand uses kebab-case with externally-tagged variants
        // for payload-carrying variants ({"seek-relative":{"offset_ms":..}}).
        // The exact serde shape isn't part of the public contract; the
        // round-trip is.
        let parsed: Request = serde_json::from_str(&raw).unwrap();
        assert!(matches!(
            parsed,
            Request::PlaybackCommand {
                command: PlaybackCommand::SeekRelative { offset_ms: -30_000 }
            }
        ));
    }

    #[test]
    fn play_uri_without_context_matches_legacy_wire_form() {
        // The pre-context wire form is `{"play-uri":{"uri":"…"}}` — no
        // `context-uri` key. `#[serde(default, skip_serializing_if)]` must
        // keep both the serialized bytes AND the deserialization of the old
        // form byte-for-byte identical, so an old client stays compatible.
        let cmd = PlaybackCommand::PlayUri {
            uri: "spotify:track:abc".to_string(),
            context_uri: None,
        };
        let raw = serde_json::to_string(&cmd).unwrap();
        assert_eq!(raw, r#"{"play-uri":{"uri":"spotify:track:abc"}}"#);
        assert!(!raw.contains("context"));

        // An old client that omits the field still deserializes.
        let legacy: PlaybackCommand =
            serde_json::from_str(r#"{"play-uri":{"uri":"spotify:track:abc"}}"#).unwrap();
        assert_eq!(legacy, cmd);
    }

    #[test]
    fn play_uri_with_context_round_trips() {
        let cmd = PlaybackCommand::PlayUri {
            uri: "spotify:track:abc".to_string(),
            context_uri: Some(crate::LIKED_SONGS_CONTEXT.to_string()),
        };
        let raw = serde_json::to_string(&cmd).unwrap();
        // `rename_all = "kebab-case"` renames variants, not struct-variant
        // fields, so the field stays snake_case on the wire (matching the
        // sibling `position_ms` / `offset_ms` fields).
        assert!(raw.contains("\"context_uri\":\"spotuify:collection:liked\""));
        let parsed: PlaybackCommand = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed, cmd);
        assert_eq!(cmd.label(), "play-uri");
    }

    #[test]
    fn request_wire_shape_is_kebab_case_and_tagged() {
        let raw = serde_json::to_string(&IpcMessage {
            id: 7,
            source: None,
            mutation_id: None,
            payload: IpcPayload::Request(Request::GetDaemonStatus),
        })
        .unwrap();

        assert!(raw.contains("\"type\":\"Request\""));
        assert!(raw.contains("\"cmd\":\"get-daemon-status\""));
        assert!(!raw.contains("\"source\""));
    }

    #[test]
    fn music_request_wire_shape_is_kebab_case_and_typed() {
        let raw = serde_json::to_string(&IpcMessage {
            id: 8,
            source: Some(super::OperationSource::Cli),
            mutation_id: None,
            payload: IpcPayload::Request(Request::Search {
                query: "luther vandross".to_string(),
                scope: super::SearchScopeData::Track,
                source: super::SearchSourceData::Hybrid,
                limit: 10,
                provider: None,
                kinds: None,
                sort: None,
            }),
        })
        .unwrap();

        assert!(raw.contains("\"cmd\":\"search\""));
        assert!(raw.contains("\"source\":\"cli\""));
        assert!(raw.contains("\"query\":\"luther vandross\""));
        assert!(raw.contains("\"scope\":\"track\""));
        assert!(raw.contains("\"source\":\"hybrid\""));

        let raw = serde_json::to_string(&IpcMessage {
            id: 9,
            source: None,
            mutation_id: None,
            payload: IpcPayload::Request(Request::PlaybackCommand {
                command: super::PlaybackCommand::Next,
            }),
        })
        .unwrap();

        assert!(raw.contains("\"cmd\":\"playback-command\""));
        assert!(raw.contains("\"command\":\"next\""));
    }

    #[test]
    fn v2_library_and_queue_requests_round_trip() {
        for (request, tag) in [
            (
                Request::SavedTracks {
                    limit: 50,
                    offset: 0,
                    provider: None,
                },
                "saved-tracks",
            ),
            (
                Request::SavedShows {
                    limit: 200,
                    provider: None,
                },
                "saved-shows",
            ),
            (
                Request::ShowEpisodes {
                    show: "spotify:show:abc".to_string(),
                    limit: 50,
                    offset: 0,
                },
                "show-episodes",
            ),
            (
                Request::QueueAddMany {
                    uris: vec!["spotify:track:1".to_string(), "spotify:track:2".to_string()],
                },
                "queue-add-many",
            ),
            (
                Request::FollowedArtists {
                    limit: 200,
                    provider: None,
                },
                "followed-artists",
            ),
            (
                Request::ArtistFollow {
                    artist: "spotify:artist:abc".to_string(),
                },
                "artist-follow",
            ),
            (
                Request::ArtistUnfollow {
                    artist: "spotify:artist:abc".to_string(),
                },
                "artist-unfollow",
            ),
            (Request::ListenSessions { limit: 50 }, "listen-sessions"),
            (
                Request::EpisodeFeed {
                    limit: 100,
                    sort: EpisodeSort::Oldest,
                    refresh: true,
                    provider: None,
                },
                "episode-feed",
            ),
        ] {
            assert_eq!(request.kind_label(), tag);
            assert_eq!(request.category(), IpcCategory::CoreMusic);
            let raw = serde_json::to_string(&IpcMessage {
                id: 1,
                source: None,
                mutation_id: None,
                payload: IpcPayload::Request(request.clone()),
            })
            .unwrap();
            assert!(raw.contains(&format!("\"cmd\":\"{tag}\"")), "wire: {raw}");
            let decoded: IpcMessage = serde_json::from_str(&raw).unwrap();
            match decoded.payload {
                IpcPayload::Request(decoded) => assert_eq!(decoded, request),
                other => panic!("expected request, got {other:?}"),
            }
        }
    }

    #[test]
    fn check_update_request_is_admin_and_round_trips() {
        let request = Request::CheckUpdate { force: true };
        assert_eq!(request.kind_label(), "check-update");
        assert_eq!(request.category(), IpcCategory::AdminMaintenance);
        let raw = serde_json::to_string(&IpcMessage {
            id: 1,
            source: None,
            mutation_id: None,
            payload: IpcPayload::Request(request.clone()),
        })
        .unwrap();
        assert!(raw.contains("\"cmd\":\"check-update\""), "wire: {raw}");
        let decoded: IpcMessage = serde_json::from_str(&raw).unwrap();
        match decoded.payload {
            IpcPayload::Request(decoded) => assert_eq!(decoded, request),
            other => panic!("expected request, got {other:?}"),
        }
    }

    #[test]
    fn update_status_response_round_trips() {
        let original = ResponseData::UpdateStatus {
            update_available: true,
            current_version: "0.1.47".to_string(),
            latest_version: Some("0.1.48".to_string()),
            release_url: Some(
                "https://github.com/planetaryescape/spotuify/releases/tag/v0.1.48".to_string(),
            ),
            upgrade: UpgradeHint {
                method: UpgradeMethod::Homebrew,
                command: Some("brew upgrade planetaryescape/spotuify/spotuify".to_string()),
                url: None,
            },
            checked_at_ms: Some(1_700_000_000_000),
        };
        let raw = serde_json::to_string(&original).unwrap();
        assert!(raw.contains("\"kind\":\"update-status\""), "wire: {raw}");
        let decoded: ResponseData = serde_json::from_str(&raw).unwrap();
        match decoded {
            ResponseData::UpdateStatus {
                update_available,
                latest_version,
                upgrade,
                ..
            } => {
                assert!(update_available);
                assert_eq!(latest_version.as_deref(), Some("0.1.48"));
                assert_eq!(upgrade.method, UpgradeMethod::Homebrew);
            }
            other => panic!("expected update-status, got {other:?}"),
        }
    }

    #[test]
    fn saved_tracks_page_response_round_trips() {
        let original = ResponseData::SavedTracksPage {
            items: vec![spotuify_core::MediaItem::default()],
            total: 4200,
            offset: 50,
        };
        let raw = serde_json::to_string(&original).unwrap();
        assert!(
            raw.contains("\"kind\":\"saved-tracks-page\""),
            "wire: {raw}"
        );
        assert!(raw.contains("\"total\":4200"), "wire: {raw}");
        assert!(raw.contains("\"offset\":50"), "wire: {raw}");
        let decoded: ResponseData = serde_json::from_str(&raw).unwrap();
        match decoded {
            ResponseData::SavedTracksPage {
                items,
                total,
                offset,
            } => {
                assert_eq!(items.len(), 1);
                assert_eq!(total, 4200);
                assert_eq!(offset, 50);
            }
            other => panic!("expected saved-tracks-page, got {other:?}"),
        }
    }

    #[test]
    fn update_available_event_round_trips() {
        let event = DaemonEvent::UpdateAvailable {
            latest_version: "0.1.48".to_string(),
            release_url: None,
            upgrade: UpgradeHint {
                method: UpgradeMethod::Cargo,
                command: Some(
                    "cargo install --git https://github.com/planetaryescape/spotuify --tag v0.1.48 --locked spotuify"
                        .to_string(),
                ),
                url: None,
            },
        };
        let raw = serde_json::to_string(&event).unwrap();
        assert!(
            raw.contains("\"event\":\"update-available\""),
            "wire: {raw}"
        );
        let decoded: DaemonEvent = serde_json::from_str(&raw).unwrap();
        assert_eq!(decoded, event);
    }

    #[test]
    fn episode_sort_and_search_date_sort_labels() {
        assert_eq!(EpisodeSort::default(), EpisodeSort::Newest);
        assert_eq!(EpisodeSort::Oldest.label(), "oldest");
        assert_eq!(SearchSortData::Date.label(), "date");
        // round-trip the lowercase serde tags
        for sort in [
            EpisodeSort::Newest,
            EpisodeSort::Oldest,
            EpisodeSort::Duration,
            EpisodeSort::Title,
            EpisodeSort::Show,
        ] {
            let raw = serde_json::to_string(&sort).unwrap();
            let back: EpisodeSort = serde_json::from_str(&raw).unwrap();
            assert_eq!(sort, back);
        }
    }

    #[test]
    fn reminder_requests_round_trip_and_are_core_music() {
        for (request, tag) in [
            (
                Request::ReminderCreate {
                    media_uri: "spotify:album:abc".to_string(),
                    anchor_at_ms: 1_700_000_000_000,
                    recurrence: spotuify_core::Recurrence::Weekly,
                    tz: "America/New_York".to_string(),
                    message: Some("revisit".to_string()),
                },
                "reminder-create",
            ),
            (
                Request::RemindersList {
                    include_inactive: true,
                },
                "reminders-list",
            ),
            (
                Request::ReminderCancel {
                    id: "r1".to_string(),
                },
                "reminder-cancel",
            ),
            (
                Request::NotificationsList {
                    include_archived: false,
                },
                "notifications-list",
            ),
            (
                Request::NotificationAct {
                    id: "n1".to_string(),
                    action: crate::NotificationAction::Snooze,
                    snooze_until_ms: Some(1_700_000_900_000),
                },
                "notification-act",
            ),
        ] {
            assert_eq!(request.kind_label(), tag);
            assert_eq!(request.category(), IpcCategory::CoreMusic);
            let raw = serde_json::to_string(&IpcMessage {
                id: 1,
                source: None,
                mutation_id: None,
                payload: IpcPayload::Request(request.clone()),
            })
            .unwrap();
            assert!(raw.contains(&format!("\"cmd\":\"{tag}\"")), "wire: {raw}");
            let decoded: IpcMessage = serde_json::from_str(&raw).unwrap();
            match decoded.payload {
                IpcPayload::Request(decoded) => assert_eq!(decoded, request),
                other => panic!("expected request, got {other:?}"),
            }
        }
    }

    #[test]
    fn tui_refresh_request_wire_shape_is_kebab_case_and_typed() {
        let raw = serde_json::to_string(&IpcMessage {
            id: 10,
            source: None,
            mutation_id: None,
            payload: IpcPayload::Request(Request::RecentlyPlayed { provider: None }),
        })
        .unwrap();

        assert!(raw.contains("\"cmd\":\"recently-played\""));

        let raw = serde_json::to_string(&IpcMessage {
            id: 11,
            source: None,
            mutation_id: None,
            payload: IpcPayload::Request(Request::Image {
                url: "https://example.invalid/cover.png".to_string(),
            }),
        })
        .unwrap();

        assert!(raw.contains("\"cmd\":\"image\""));
        assert!(raw.contains("\"url\":\"https://example.invalid/cover.png\""));
    }

    #[test]
    fn cover_art_request_wire_shape_is_kebab_case_and_returns_local_path() {
        let raw = serde_json::to_string(&IpcMessage {
            id: 12,
            source: None,
            mutation_id: None,
            payload: IpcPayload::Request(Request::CoverArt {
                url: "https://i.scdn.co/image/abc".to_string(),
            }),
        })
        .unwrap();

        assert!(raw.contains("\"cmd\":\"cover-art\""));
        assert!(raw.contains("\"url\":\"https://i.scdn.co/image/abc\""));

        let raw = serde_json::to_string(&IpcMessage {
            id: 13,
            source: None,
            mutation_id: None,
            payload: IpcPayload::Response(Response::Ok {
                data: ResponseData::CoverArt {
                    path: "/tmp/abc.jpg".to_string(),
                    cache_hit: true,
                    bytes: 42,
                    fetched_at_ms: Some(1_700_000_000_000),
                },
            }),
        })
        .unwrap();

        assert!(raw.contains("\"kind\":\"cover-art\""));
        assert!(raw.contains("\"path\":\"/tmp/abc.jpg\""));
        assert!(raw.contains("\"cache_hit\":true"));
    }

    #[test]
    fn playlist_create_request_wire_shape_is_kebab_case_and_typed() {
        let raw = serde_json::to_string(&IpcMessage {
            id: 14,
            source: None,
            mutation_id: Some(super::MutationId::new_v7()),
            payload: IpcPayload::Request(Request::PlaylistCreate {
                name: "Exile and Return".to_string(),
                description: None,
                uris: vec!["spotify:track:1".to_string()],
                provider: None,
            }),
        })
        .unwrap();

        assert!(raw.contains("\"cmd\":\"playlist-create\""));
        assert!(raw.contains("\"name\":\"Exile and Return\""));
        assert!(raw.contains("\"uris\":[\"spotify:track:1\"]"));
    }

    #[test]
    fn committed_ops_require_mutation_ids_but_undo_preview_does_not() {
        let operation_id = super::OperationId::new_v7();
        assert!(!Request::OpsUndo {
            operation_id: Some(operation_id),
            dry_run: true,
            force: false,
            bulk_since_ms: None,
        }
        .requires_mutation_id());
        assert!(Request::OpsUndo {
            operation_id: Some(operation_id),
            dry_run: false,
            force: false,
            bulk_since_ms: None,
        }
        .requires_mutation_id());
        assert!(Request::OpsRedo {
            operation_id: Some(operation_id),
        }
        .requires_mutation_id());
        assert!(!Request::RadioStart {
            seed_uri: "spotify:track:seed".to_string(),
            dry_run: true,
        }
        .requires_mutation_id());
        assert!(Request::RadioStart {
            seed_uri: "spotify:track:seed".to_string(),
            dry_run: false,
        }
        .requires_mutation_id());
    }

    #[test]
    fn ops_mutation_id_survives_ipc_round_trip() {
        let mutation_id = super::MutationId::new_v7();
        let message = IpcMessage {
            id: 16,
            source: None,
            mutation_id: Some(mutation_id),
            payload: IpcPayload::Request(Request::OpsRedo {
                operation_id: Some(super::OperationId::new_v7()),
            }),
        };

        let raw = serde_json::to_string(&message).unwrap();
        let decoded: IpcMessage = serde_json::from_str(&raw).unwrap();

        assert_eq!(decoded.mutation_id, Some(mutation_id));
        assert!(matches!(
            decoded.payload,
            IpcPayload::Request(Request::OpsRedo { .. })
        ));
    }

    #[test]
    fn lyrics_request_wire_shape_is_kebab_case_and_typed() {
        let raw = serde_json::to_string(&IpcMessage {
            id: 15,
            source: None,
            mutation_id: None,
            payload: IpcPayload::Request(Request::LyricsGet {
                track_uri: Some("spotify:track:abc".to_string()),
                force_refresh: true,
            }),
        })
        .unwrap();

        assert!(raw.contains("\"cmd\":\"lyrics-get\""));
        assert!(raw.contains("\"track_uri\":\"spotify:track:abc\""));
        assert!(raw.contains("\"force_refresh\":true"));
    }

    #[test]
    fn auth_status_wire_shape_cannot_carry_tokens() {
        let raw = serde_json::to_string(&ResponseData::AuthStatus {
            status: AuthStatusData {
                provider: ProviderId::new("spotify").unwrap(),
                strategy: AuthStrategyData::SpotifyOauth,
                preferred_method: Some(AuthMethodData::DevApp),
                auth_required: false,
                auth_revoked: false,
                credentials: vec![AuthCredentialStatus {
                    kind: AuthCredentialKind::DevApp,
                    present: true,
                    expires_at_ms: Some(1_700_000_000_000),
                    scopes: vec!["streaming".to_string()],
                    missing_scopes: Vec::new(),
                }],
            },
        })
        .unwrap();

        assert!(raw.contains("\"preferred_method\":\"dev_app\""));
        assert!(!raw.contains("access_token"));
        assert!(!raw.contains("refresh_token"));
        assert!(!raw.contains("token-sentinel-never-on-wire"));
    }

    #[test]
    fn legacy_auth_status_without_preferred_method_decodes_additively() {
        let status: AuthStatusData = serde_json::from_value(serde_json::json!({
            "provider": "archive",
            "strategy": "spotify_oauth",
            "auth_required": true,
            "auth_revoked": false,
            "credentials": []
        }))
        .unwrap();

        assert_eq!(status.preferred_method, None);
        assert_eq!(
            serde_json::to_value(status)
                .unwrap()
                .get("preferred_method"),
            None
        );
    }
}
