use bytes::BytesMut;
use serde::{Deserialize, Serialize};
use tokio_util::codec::{Decoder, Encoder, LengthDelimitedCodec};

use crate::spotify::{Device, MediaItem, Playback, Playlist, Queue};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    Image { bytes: Vec<u8> },
    Queue { queue: Queue },
    Playlists { playlists: Vec<Playlist> },
    MediaItems { items: Vec<MediaItem> },
    Mutation { receipt: CommandReceipt },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandReceipt {
    pub ok: bool,
    pub action: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum DaemonEvent {
    ShutdownRequested,
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
            }),
        })
        .unwrap();

        assert!(raw.contains("\"cmd\":\"search\""));
        assert!(raw.contains("\"query\":\"luther vandross\""));
        assert!(raw.contains("\"scope\":\"track\""));

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
}
