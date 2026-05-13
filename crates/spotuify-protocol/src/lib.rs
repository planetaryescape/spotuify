//! IPC wire protocol shared between the spotuify daemon, CLI, TUI, and MCP server.
//!
//! All Request/Response/Event types live here. Per
//! `docs/blueprint/01-architecture.md` §"Dependency rules", this crate depends
//! only on `spotuify-core` for domain types. It must never import storage,
//! search, HTTP, or any other concern.

pub mod event_log;
pub mod ipc_client;
pub mod output;

pub use event_log::{findings_from, EventLog, LoggedEvent, LoggedKind};
pub use ipc_client::{default_socket_path, IpcClient};
pub use output::OutputFormat;

use bytes::BytesMut;
use serde::{Deserialize, Serialize};
use tokio_util::codec::{Decoder, Encoder, LengthDelimitedCodec};

use spotuify_core::{Device, MediaItem, Playback, Playlist, Queue};

pub const IPC_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcMessage {
    pub id: u64,
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
    QueueGet,
    QueueAdd {
        uri: String,
    },
    PlaylistsList,
    PlaylistTracks {
        playlist: String,
    },
    PlaylistAddItems {
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PlaybackCommand {
    Pause,
    Resume,
    Toggle,
    Next,
    Previous,
    PlayUri { uri: String },
    Seek { position_ms: u64 },
    Volume { volume_percent: u8 },
    Shuffle { state: bool },
    Repeat { state: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchScopeData {
    All,
    Track,
    Episode,
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
    InvalidRequest,
    Network,
    Provider,
    RateLimited,
    Unsupported,
    #[default]
    Internal,
}

impl IpcErrorKind {
    fn as_code(self) -> &'static str {
        match self {
            Self::Auth => "auth",
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
    if lower.contains("auth") || lower.contains("oauth") || lower.contains("login") {
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
    DaemonStatus { status: DaemonStatus },
    DoctorReport { report: DoctorReport },
    Playback { playback: Playback },
    Devices { devices: Vec<Device> },
    SearchResults { items: Vec<MediaItem> },
    CacheStatus { status: CacheStatus },
    Reindex { stats: ReindexStats },
    Sync { summary: CacheSyncSummary },
    Image { bytes: Vec<u8> },
    Queue { queue: Queue },
    Playlists { playlists: Vec<Playlist> },
    MediaItems { items: Vec<MediaItem> },
    Logs { lines: Vec<String> },
    Mutation { receipt: CommandReceipt },
    PlaylistCreate { receipt: PlaylistCreateReceipt },
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
    pub media_items: u32,
    pub devices: u32,
    pub playback_snapshots: u32,
    pub playlists: u32,
    pub playlist_items: u32,
    pub recent_items: u32,
    pub library_items: u32,
    pub search_runs: u32,
    pub search_results: u32,
    pub sync_events: u32,
    pub index_documents: u64,
    pub last_sync_at_ms: Option<i64>,
    pub last_search_at_ms: Option<i64>,
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
    pub devices: u32,
    pub playlists: u32,
    pub playlist_items: u32,
    pub recent_items: u32,
    pub library_items: u32,
    pub media_items: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum DaemonEvent {
    ShutdownRequested,
    PlaybackChanged {
        action: String,
    },
    QueueChanged {
        action: String,
        uris: Vec<String>,
    },
    DevicesChanged {
        action: String,
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
}

/// Auth error categories. Mirrors `spotuify_spotify::error::AuthErrorKind`
/// so the daemon event stream stays typed without dragging the Spotify
/// crate into the protocol. Stable; remapping is a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthErrorKind {
    ExpiredRefresh,
    InvalidGrant,
    Forbidden,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum HealthClass {
    #[default]
    Healthy,
    Degraded,
}

impl HealthClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
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
    Spotifyd,
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
    pub spotifyd_config_path: Option<String>,
    pub spotifyd_autostart: Option<bool>,
    pub spotifyd_running: Option<bool>,
    pub client_id: Option<String>,
    pub client_secret_present: Option<bool>,
    pub redirect_uri: Option<String>,
    pub keychain_token: DoctorCheck,
    pub daemon: DaemonStatus,
    pub api_checks: Vec<DoctorCheck>,
    pub device_diagnostics: Option<DeviceDiagnostics>,
    pub recommended_next_steps: Vec<String>,
    pub findings: Vec<DoctorFinding>,
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
    use super::{IpcMessage, IpcPayload, Request};

    #[test]
    fn request_wire_shape_is_kebab_case_and_tagged() {
        let raw = serde_json::to_string(&IpcMessage {
            id: 7,
            payload: IpcPayload::Request(Request::GetDaemonStatus),
        })
        .unwrap();

        assert!(raw.contains("\"type\":\"Request\""));
        assert!(raw.contains("\"cmd\":\"get-daemon-status\""));
    }

    #[test]
    fn music_request_wire_shape_is_kebab_case_and_typed() {
        let raw = serde_json::to_string(&IpcMessage {
            id: 8,
            payload: IpcPayload::Request(Request::Search {
                query: "luther vandross".to_string(),
                scope: super::SearchScopeData::Track,
                source: super::SearchSourceData::Hybrid,
                limit: 10,
            }),
        })
        .unwrap();

        assert!(raw.contains("\"cmd\":\"search\""));
        assert!(raw.contains("\"query\":\"luther vandross\""));
        assert!(raw.contains("\"scope\":\"track\""));
        assert!(raw.contains("\"source\":\"hybrid\""));

        let raw = serde_json::to_string(&IpcMessage {
            id: 9,
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
            payload: IpcPayload::Request(Request::RecentlyPlayed),
        })
        .unwrap();

        assert!(raw.contains("\"cmd\":\"recently-played\""));

        let raw = serde_json::to_string(&IpcMessage {
            id: 11,
            payload: IpcPayload::Request(Request::Image {
                url: "https://example.invalid/cover.png".to_string(),
            }),
        })
        .unwrap();

        assert!(raw.contains("\"cmd\":\"image\""));
        assert!(raw.contains("\"url\":\"https://example.invalid/cover.png\""));
    }

    #[test]
    fn playlist_create_request_wire_shape_is_kebab_case_and_typed() {
        let raw = serde_json::to_string(&IpcMessage {
            id: 12,
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
}
