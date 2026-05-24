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

pub use agent_playlists::{
    CandidateIssue, CandidateStatus, PlaylistCreateMetadata, PlaylistCreatePreview,
    PlaylistMutationPreview, PlaylistPlan, PlaylistTrackSelection, ResolvedTrackCandidate,
};
pub use analytics::{
    ExportTarget, RebuildReport, RediscoveryCandidate, SearchHistoryEntry, SearchMode, SinceWindow,
    TopEntry, TopKind,
};
pub use event_log::{findings_from, EventLog, LoggedEvent, LoggedKind};
pub use ipc_client::{default_socket_path, IpcClient};
pub use operations::{
    Operation, OperationId, OperationKind, OperationSource, OperationStatus, PreState, ReversalPlan,
};
pub use output::OutputFormat;
pub use spotuify_core::HabitBucket;
pub use spotuify_core::HabitWindow;

use bytes::BytesMut;
use serde::{Deserialize, Serialize};
use tokio_util::codec::{Decoder, Encoder, LengthDelimitedCodec};

use spotuify_core::{Device, MediaItem, MediaKind, Playback, Playlist, Queue, SyncedLyrics};

pub const IPC_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcMessage {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<OperationSource>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum Request {
    Ping,
    /// Opt this IPC connection into daemon event broadcasts.
    ///
    /// One-shot request clients should not receive unsolicited events;
    /// event-stream clients send this once before waiting on `next_event`.
    SubscribeEvents,
    Shutdown,
    GetDaemonStatus,
    GetDoctorReport,
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
    },
    /// Single-page fetch used for scroll-triggered "load more" on a
    /// specific pane. Emits exactly one `DaemonEvent::SearchPage` or
    /// `DaemonEvent::SearchFailed` and no `SearchComplete`.
    SearchPage {
        query: String,
        kind: MediaKind,
        offset: u32,
        version: u64,
    },
    Reindex,
    CacheStatus,
    LibraryList {
        limit: u32,
    },
    LogsTail {
        lines: usize,
    },
    Sync {
        target: SyncTargetData,
    },
    RecentlyPlayed,
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
    PlaylistsList,
    PlaylistTracks {
        playlist: String,
        #[serde(default)]
        wait: bool,
    },
    /// Fetch an artist's own releases (albums + singles).
    ArtistAlbums {
        artist: String,
    },
    /// Fetch the track listing of a given album.
    AlbumTracks {
        album: String,
    },
    PlaylistAddItems {
        playlist: String,
        uris: Vec<String>,
    },
    PlaylistRemoveItems {
        playlist: String,
        uris: Vec<String>,
    },
    PlaylistCreate {
        name: String,
        description: Option<String>,
        uris: Vec<String>,
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
    },
    AnalyticsPrune {
        apply: bool,
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
    /// Reload the on-disk config and (optionally) restart the player
    /// only if `[player].backend` changed.
    Reload,
    /// Force-rebuild the upstream Spotify session. Useful after VPN
    /// flap / network change for embedded librespot.
    Reconnect,
    /// Drop the daemon's cached Spotify token + clear the
    /// `auth_revoked` latch so the next operation re-reads fresh
    /// credentials from keychain/disk. Fired by clients after they've
    /// completed an interactive OAuth flow (TUI's LoginModal flow,
    /// CLI's auto-retry on AuthRevoked).
    ReloadAuth,
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
    pub fn category(&self) -> IpcCategory {
        match self {
            Self::Ping
            | Self::SubscribeEvents
            | Self::Shutdown
            | Self::GetDaemonStatus
            | Self::GetDoctorReport
            | Self::LogsTail { .. }
            | Self::Sync { .. }
            | Self::Reconnect
            | Self::ReloadAuth
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
            | Self::AnalyticsPrune { .. }
            | Self::OpsLog { .. }
            | Self::OpsShow { .. }
            | Self::OpsUndo { .. }
            | Self::OpsRedo { .. }
            | Self::SearchCachePrune { .. } => IpcCategory::SpotuifyPlatform,
            Self::SetVizEnabled { .. }
            | Self::SetVizSource { .. }
            | Self::GetVizStatus
            | Self::SetVizFocus { .. } => IpcCategory::ClientSpecific,
            Self::PlaybackGet
            | Self::PlaybackCommand { .. }
            | Self::DevicesList
            | Self::DeviceTransfer { .. }
            | Self::Search { .. }
            | Self::SearchStream { .. }
            | Self::SearchPage { .. }
            | Self::LibraryList { .. }
            | Self::RecentlyPlayed
            | Self::Image { .. }
            | Self::CoverArt { .. }
            | Self::QueueGet
            | Self::QueueAdd { .. }
            | Self::PlaylistsList
            | Self::PlaylistTracks { .. }
            | Self::ArtistAlbums { .. }
            | Self::AlbumTracks { .. }
            | Self::PlaylistAddItems { .. }
            | Self::PlaylistRemoveItems { .. }
            | Self::PlaylistCreate { .. }
            | Self::LibrarySave { .. }
            | Self::LibraryUnsave { .. }
            | Self::LyricsGet { .. }
            | Self::LyricsOffsetSet { .. }
            | Self::Reload => IpcCategory::CoreMusic,
        }
    }

    /// Stable short tag used in IPC observability spans and JSON logs.
    /// Matches the serde `rename_all = "kebab-case"` variant tag so log
    /// readers can pivot freely between wire payloads and tracing events.
    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::Ping => "ping",
            Self::SubscribeEvents => "subscribe-events",
            Self::Shutdown => "shutdown",
            Self::GetDaemonStatus => "get-daemon-status",
            Self::GetDoctorReport => "get-doctor-report",
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
            Self::RecentlyPlayed => "recently-played",
            Self::Image { .. } => "image",
            Self::CoverArt { .. } => "cover-art",
            Self::QueueGet => "queue-get",
            Self::QueueAdd { .. } => "queue-add",
            Self::PlaylistsList => "playlists-list",
            Self::PlaylistTracks { .. } => "playlist-tracks",
            Self::ArtistAlbums { .. } => "artist-albums",
            Self::AlbumTracks { .. } => "album-tracks",
            Self::PlaylistAddItems { .. } => "playlist-add-items",
            Self::PlaylistRemoveItems { .. } => "playlist-remove-items",
            Self::PlaylistCreate { .. } => "playlist-create",
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
            Self::AnalyticsPrune { .. } => "analytics-prune",
            Self::OpsLog { .. } => "ops-log",
            Self::OpsShow { .. } => "ops-show",
            Self::OpsUndo { .. } => "ops-undo",
            Self::OpsRedo { .. } => "ops-redo",
            Self::Reload => "reload",
            Self::Reconnect => "reconnect",
            Self::ReloadAuth => "reload-auth",
            Self::WebApiToken { .. } => "web-api-token",
            Self::SearchCachePrune { .. } => "search-cache-prune",
            Self::SetVizEnabled { .. } => "set-viz-enabled",
            Self::SetVizSource { .. } => "set-viz-source",
            Self::GetVizStatus => "get-viz-status",
            Self::SetVizFocus { .. } => "set-viz-focus",
        }
    }
}

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
        state: String,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchSourceData {
    Local,
    Spotify,
    Hybrid,
}

impl SearchSourceData {
    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Spotify => "spotify",
            Self::Hybrid => "hybrid",
        }
    }
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
    },
}

impl Response {
    pub fn error(message: impl Into<String>) -> Self {
        let message = message.into();
        let kind = classify_error_kind(&message);
        Self::Error {
            message,
            code: kind.as_code().to_string(),
            retryable: error_looks_retryable(kind),
            kind,
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
    #[default]
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
            Self::Internal => "internal",
        }
    }
}

fn classify_error_kind(message: &str) -> IpcErrorKind {
    let lower = message.to_ascii_lowercase();
    // Specific cases first — "refresh token revoked" / "session expired"
    // would otherwise be swallowed by the generic `Auth` arm.
    if lower.contains("refresh token revoked")
        || lower.contains("session expired")
        || lower.contains("invalid_grant")
    {
        IpcErrorKind::AuthRevoked
    } else if lower.contains("auth") || lower.contains("oauth") || lower.contains("login") {
        IpcErrorKind::Auth
    } else if lower.contains("rate limit") || lower.contains("rate limited") {
        IpcErrorKind::RateLimited
    } else if lower.contains("timeout") || lower.contains("timed out") || lower.contains("dns") {
        IpcErrorKind::Network
    } else if lower.contains("spotify") || lower.contains("device") {
        IpcErrorKind::Provider
    } else if lower.contains("unsupported") || lower.contains("not supported") {
        IpcErrorKind::Unsupported
    } else if lower.contains("invalid") || lower.contains("expected") {
        IpcErrorKind::InvalidRequest
    } else {
        IpcErrorKind::Internal
    }
}

fn error_looks_retryable(kind: IpcErrorKind) -> bool {
    matches!(kind, IpcErrorKind::Network | IpcErrorKind::RateLimited)
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
    SearchResults {
        items: Vec<MediaItem>,
    },
    /// Ack for `Request::SearchStream` / `Request::SearchPage`. The
    /// actual results stream back as `DaemonEvent::SearchPage` events on
    /// the broadcast channel; clients filter by `(query, version)`.
    SearchStarted {
        query: String,
        version: u64,
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
    Playlists {
        playlists: Vec<Playlist>,
    },
    MediaItems {
        items: Vec<MediaItem>,
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
    },

    // --- Phase 13 — QoL / spec-compliance responses ---
    /// Generic acknowledge with a free-form message. Used by `reload`,
    /// `reconnect`, and `search-cache-prune`.
    Ack {
        message: String,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandReceipt {
    pub ok: bool,
    pub action: String,
    pub message: String,
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
    },
    LibraryChanged {
        action: String,
        uris: Vec<String>,
    },
    SearchUpdated {
        query: String,
        count: usize,
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
    },
    /// Emitted once after a `Request::SearchStream`'s initial fanout has
    /// resolved (all 18 page tasks joined). Not emitted for scroll-
    /// triggered `Request::SearchPage` fetches.
    SearchComplete {
        query: String,
        version: u64,
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
    },

    // AuthError: emitted on 401 after refresh fails, on 403 with required
    // scope mismatch, and on revoked refresh tokens.
    AuthError {
        kind: AuthErrorKind,
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

    // PremiumRequired: Spotify account is not Premium; embedded librespot
    // cannot stream. Sticky — clients should keep showing the banner
    // until the user reconnects.
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
        /// Phase 7 — backend kind at the moment of the change. The TUI
        /// uses it to phrase the hint correctly.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        backend_kind: Option<spotuify_core::BackendKind>,
    },
}

/// Redact token-shaped substrings before user-visible events are logged,
/// stored, or broadcast. This intentionally targets long credential-like
/// blobs, not ordinary short IDs or Spotify URIs.
pub fn redact_sensitive_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut run = String::new();
    for ch in input.chars() {
        if is_token_char(ch) {
            run.push(ch);
        } else {
            flush_redaction_run(&mut out, &mut run);
            out.push(ch);
        }
    }
    flush_redaction_run(&mut out, &mut run);
    out
}

pub fn sanitize_daemon_event(event: DaemonEvent) -> DaemonEvent {
    match event {
        DaemonEvent::SearchFailed {
            query,
            version,
            kind,
            offset,
            message,
        } => DaemonEvent::SearchFailed {
            query,
            version,
            kind,
            offset,
            message: redact_sensitive_text(&message),
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
        DaemonEvent::SessionDisconnected { reason } => DaemonEvent::SessionDisconnected {
            reason: redact_sensitive_text(&reason),
        },
        DaemonEvent::PlayerFailed { reason, restarts } => DaemonEvent::PlayerFailed {
            reason: redact_sensitive_text(&reason),
            restarts,
        },
        other => other,
    }
}

fn is_token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '+' | '/' | '=')
}

fn flush_redaction_run(out: &mut String, run: &mut String) {
    if run.is_empty() {
        return;
    }
    if looks_sensitive_token(run) {
        out.push_str("<redacted>");
    } else {
        out.push_str(run);
    }
    run.clear();
}

fn looks_sensitive_token(value: &str) -> bool {
    value.len() >= 32
        && value.chars().any(|ch| ch.is_ascii_alphabetic())
        && value.chars().any(|ch| ch.is_ascii_digit())
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
            Request::SubscribeEvents.category(),
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
    /// Phase 0 — active playback backend at diagnostics time. Lets the
    /// TUI form the correct hint ("switch to embedded for sink tap").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_kind: Option<spotuify_core::BackendKind>,
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
    pub elapsed_ms: u128,
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

pub struct IpcCodec {
    inner: LengthDelimitedCodec,
}

impl IpcCodec {
    pub fn new() -> Self {
        Self {
            inner: LengthDelimitedCodec::builder()
                .length_field_length(4)
                .max_frame_length(16 * 1024 * 1024)
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
        let json = serde_json::to_vec(&item)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        self.inner.encode(json.into(), dst)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        classify_error_kind, sanitize_daemon_event, DaemonEvent, IpcCategory, IpcErrorKind,
        IpcMessage, IpcPayload, PlaybackCommand, Request, Response, ResponseData,
    };

    #[test]
    fn auth_revoked_kind_classifies_and_roundtrips() {
        // The CLI keys off the exact `IpcErrorKind` to decide whether
        // to prompt for re-auth — the classifier must catch the daemon's
        // canonical messages.
        assert_eq!(
            classify_error_kind("Spotify refresh token revoked — re-login required"),
            IpcErrorKind::AuthRevoked
        );
        assert_eq!(
            classify_error_kind("Spotify session expired"),
            IpcErrorKind::AuthRevoked
        );
        assert_eq!(
            classify_error_kind("token exchange failed: invalid_grant"),
            IpcErrorKind::AuthRevoked
        );
        // Generic auth messages keep the broad `Auth` kind.
        assert_eq!(classify_error_kind("login required"), IpcErrorKind::Auth);
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
    fn request_kind_labels_are_kebab_case_and_match_serde_tag() {
        // Phase 0 — the IPC span uses `kind_label()` as its `request_kind`
        // field. Every variant must return a kebab-case string that matches
        // the serde `cmd` tag, so log readers can pivot freely between
        // tracing JSON and wire payloads.
        let kinds = [
            (Request::Ping, "ping"),
            (Request::PlaybackGet, "playback-get"),
            (
                Request::PlaybackCommand {
                    command: PlaybackCommand::Pause,
                },
                "playback-command",
            ),
            (Request::QueueGet, "queue-get"),
            (Request::GetVizStatus, "get-viz-status"),
            (Request::SubscribeEvents, "subscribe-events"),
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
    fn request_wire_shape_is_kebab_case_and_tagged() {
        let raw = serde_json::to_string(&IpcMessage {
            id: 7,
            source: None,
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
            payload: IpcPayload::Request(Request::Search {
                query: "luther vandross".to_string(),
                scope: super::SearchScopeData::Track,
                source: super::SearchSourceData::Hybrid,
                limit: 10,
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
            payload: IpcPayload::Request(Request::PlaybackCommand {
                command: super::PlaybackCommand::Next,
            }),
        })
        .unwrap();

        assert!(raw.contains("\"cmd\":\"playback-command\""));
        assert!(raw.contains("\"command\":\"next\""));
    }

    #[test]
    fn tui_refresh_request_wire_shape_is_kebab_case_and_typed() {
        let raw = serde_json::to_string(&IpcMessage {
            id: 10,
            source: None,
            payload: IpcPayload::Request(Request::RecentlyPlayed),
        })
        .unwrap();

        assert!(raw.contains("\"cmd\":\"recently-played\""));

        let raw = serde_json::to_string(&IpcMessage {
            id: 11,
            source: None,
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
            payload: IpcPayload::Request(Request::PlaylistCreate {
                name: "Exile and Return".to_string(),
                description: None,
                uris: vec!["spotify:track:1".to_string()],
            }),
        })
        .unwrap();

        assert!(raw.contains("\"cmd\":\"playlist-create\""));
        assert!(raw.contains("\"name\":\"Exile and Return\""));
        assert!(raw.contains("\"uris\":[\"spotify:track:1\"]"));
    }

    #[test]
    fn lyrics_request_wire_shape_is_kebab_case_and_typed() {
        let raw = serde_json::to_string(&IpcMessage {
            id: 15,
            source: None,
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
}
