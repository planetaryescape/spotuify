//! Phase 12 — operation log + undo IPC types.
//!
//! Every mutating daemon request becomes an `Operation` row. The
//! `ReversalPlan` + `PreState` pair captures both how to undo and what
//! state existed pre-mutation (so conflict detection can compare
//! against the provider's current opaque version token).
//!
//! `OperationId` is a UUID v7 — sortable by insertion time so the ops
//! log is naturally chronological without an extra index.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationLabelParseError {
    kind: &'static str,
    value: String,
}

impl OperationLabelParseError {
    fn new(kind: &'static str, value: &str) -> Self {
        Self {
            kind,
            value: value.to_string(),
        }
    }
}

impl std::fmt::Display for OperationLabelParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown {} `{}`", self.kind, self.value)
    }
}

impl std::error::Error for OperationLabelParseError {}

/// Newtype around uuid v7 for time-orderable IDs. Distinct from
/// `ReceiptId` so the type system catches mix-ups: an operation row
/// has its own ID and points at a receipt (which may be `None` for
/// daemon-synthesised ops like `undo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OperationId(pub uuid::Uuid);

impl OperationId {
    pub fn new_v7() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}

impl std::fmt::Display for OperationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for OperationId {
    type Err = uuid::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        uuid::Uuid::parse_str(value).map(Self)
    }
}

/// Where an operation originated. CLI/TUI/MCP/agent tagging lets
/// `ops log --source mcp` answer "what did the agent do?". Daemon
/// internal covers maintenance ops (rebuild, prune, recovery undo).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum OperationSource {
    Cli,
    Tui,
    Mcp,
    Agent,
    #[serde(rename = "daemon-internal")]
    DaemonInternal,
}

impl OperationSource {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Tui => "tui",
            Self::Mcp => "mcp",
            Self::Agent => "agent",
            Self::DaemonInternal => "daemon-internal",
        }
    }

    pub fn from_label(value: &str) -> Option<Self> {
        value.parse().ok()
    }
}

impl std::fmt::Display for OperationSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl std::str::FromStr for OperationSource {
    type Err = OperationLabelParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "cli" => Ok(Self::Cli),
            "tui" => Ok(Self::Tui),
            "mcp" => Ok(Self::Mcp),
            "agent" => Ok(Self::Agent),
            "daemon-internal" => Ok(Self::DaemonInternal),
            _ => Err(OperationLabelParseError::new("operation source", value)),
        }
    }
}

/// Operation row status.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Pending,
    Succeeded,
    Failed,
    Undone,
    Redone,
}

impl OperationStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Undone => "undone",
            Self::Redone => "redone",
        }
    }
}

impl std::fmt::Display for OperationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl std::str::FromStr for OperationStatus {
    type Err = OperationLabelParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "pending" => Ok(Self::Pending),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "undone" => Ok(Self::Undone),
            "redone" => Ok(Self::Redone),
            _ => Err(OperationLabelParseError::new("operation status", value)),
        }
    }
}

/// Operation kinds — one per mutation surface plus `Undo`/`Redo`.
/// Transport kinds (`Play`, `Pause`, `Seek`, etc.) appear in the log
/// but are never `reversible`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    QueueAdd,
    PlaylistAdd,
    PlaylistRemove,
    PlaylistCreate,
    PlaylistUnfollow,
    PlaylistSetImage,
    PlaylistReorder,
    LibrarySave,
    LibraryUnsave,
    ArtistFollow,
    ArtistUnfollow,
    Transfer,
    Like,
    Unlike,
    Play,
    Pause,
    Resume,
    Toggle,
    Next,
    Previous,
    Seek,
    Volume,
    Shuffle,
    Repeat,
    Undo,
    Redo,
}

impl OperationKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::QueueAdd => "queue_add",
            Self::PlaylistAdd => "playlist_add",
            Self::PlaylistRemove => "playlist_remove",
            Self::PlaylistCreate => "playlist_create",
            Self::PlaylistUnfollow => "playlist_unfollow",
            Self::PlaylistSetImage => "playlist_set_image",
            Self::PlaylistReorder => "playlist_reorder",
            Self::LibrarySave => "library_save",
            Self::LibraryUnsave => "library_unsave",
            Self::ArtistFollow => "artist_follow",
            Self::ArtistUnfollow => "artist_unfollow",
            Self::Transfer => "transfer",
            Self::Like => "like",
            Self::Unlike => "unlike",
            Self::Play => "play",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Toggle => "toggle",
            Self::Next => "next",
            Self::Previous => "previous",
            Self::Seek => "seek",
            Self::Volume => "volume",
            Self::Shuffle => "shuffle",
            Self::Repeat => "repeat",
            Self::Undo => "undo",
            Self::Redo => "redo",
        }
    }

    /// Whether this kind has a meaningful inverse. Transport kinds
    /// (play/pause/etc.) are logged but never undone. QueueAdd is also
    /// non-reversible: neither the Spotify Web API nor librespot 0.8
    /// exposes queue-remove, so there is no executable inverse.
    pub fn is_reversible(&self) -> bool {
        matches!(
            self,
            Self::PlaylistAdd
                | Self::PlaylistRemove
                | Self::PlaylistCreate
                | Self::PlaylistReorder
                | Self::LibrarySave
                | Self::LibraryUnsave
                | Self::Transfer
                | Self::Like
                | Self::Unlike
                | Self::Undo
                | Self::Redo
        )
    }
}

impl std::fmt::Display for OperationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl std::str::FromStr for OperationKind {
    type Err = OperationLabelParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "queue_add" => Ok(Self::QueueAdd),
            "playlist_add" => Ok(Self::PlaylistAdd),
            "playlist_remove" => Ok(Self::PlaylistRemove),
            "playlist_create" => Ok(Self::PlaylistCreate),
            "playlist_unfollow" => Ok(Self::PlaylistUnfollow),
            "playlist_set_image" => Ok(Self::PlaylistSetImage),
            "playlist_reorder" => Ok(Self::PlaylistReorder),
            "library_save" => Ok(Self::LibrarySave),
            "library_unsave" => Ok(Self::LibraryUnsave),
            "artist_follow" => Ok(Self::ArtistFollow),
            "artist_unfollow" => Ok(Self::ArtistUnfollow),
            "transfer" => Ok(Self::Transfer),
            "like" => Ok(Self::Like),
            "unlike" => Ok(Self::Unlike),
            "play" => Ok(Self::Play),
            "pause" => Ok(Self::Pause),
            "resume" => Ok(Self::Resume),
            "toggle" => Ok(Self::Toggle),
            "next" => Ok(Self::Next),
            "previous" => Ok(Self::Previous),
            "seek" => Ok(Self::Seek),
            "volume" => Ok(Self::Volume),
            "shuffle" => Ok(Self::Shuffle),
            "repeat" => Ok(Self::Repeat),
            "undo" => Ok(Self::Undo),
            "redo" => Ok(Self::Redo),
            _ => Err(OperationLabelParseError::new("operation kind", value)),
        }
    }
}

/// Pre-mutation state captured at issue time. Both feeds the
/// `ReversalPlan` construction AND drives conflict detection (compare
/// the stored version token against the current provider value
/// before undoing — refuse with `--force` unless equal).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PreState {
    PlaylistAdd {
        playlist_id: String,
        #[serde(rename = "snapshot_id", alias = "version_token")]
        version_token: Option<String>,
        added_uris: Vec<String>,
    },
    PlaylistRemove {
        playlist_id: String,
        #[serde(rename = "snapshot_id", alias = "version_token")]
        version_token: Option<String>,
        /// `(uri, position)` so the inverse insert lands items back
        /// where they came from.
        removed_items: Vec<(String, u32)>,
    },
    PlaylistCreate {
        playlist_id: String,
    },
    PlaylistReorder {
        playlist_id: String,
        #[serde(rename = "snapshot_id", alias = "version_token")]
        version_token: Option<String>,
        range_start: u32,
        insert_before: u32,
        range_length: u32,
    },
    LibrarySave {
        uri: String,
        prior_was_saved: bool,
    },
    Transfer {
        prior_device_id: Option<String>,
    },
    QueueAdd {
        uri: String,
    },
    Like {
        uri: String,
        prior_was_liked: bool,
    },
    /// Transport kinds capture nothing.
    Transport,
}

/// How to undo this operation. One variant per reversible
/// `OperationKind`. `NotReversible` is recorded for transport so
/// `ops log` still shows them with a clear explanation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReversalPlan {
    QueueRemove {
        uri: String,
    },
    PlaylistRemoveTracks {
        playlist_id: String,
        uris: Vec<String>,
        #[serde(rename = "snapshot_id", alias = "version_token")]
        version_token: Option<String>,
    },
    PlaylistAddAtPositions {
        playlist_id: String,
        /// `(uri, position)` pairs to re-insert.
        items: Vec<(String, u32)>,
        #[serde(rename = "snapshot_id", alias = "version_token")]
        version_token: Option<String>,
    },
    PlaylistDelete {
        playlist_id: String,
    },
    PlaylistReorder {
        playlist_id: String,
        range_start: u32,
        insert_before: u32,
        range_length: u32,
        #[serde(rename = "snapshot_id", alias = "version_token")]
        version_token: Option<String>,
    },
    LibraryUnsave {
        uri: String,
    },
    LibrarySave {
        uri: String,
        prior_added_at_ms: Option<i64>,
    },
    TransferToPriorDevice {
        device_id: String,
        /// Provider that performed the original transfer. Absent on legacy
        /// persisted plans, which retain default-provider fallback on undo.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<spotuify_core::ProviderId>,
    },
    Like {
        uri: String,
    },
    Unlike {
        uri: String,
    },
    /// Redo of an undo: re-executes the original forward request.
    /// `target_op_id` identifies the op whose forward action to replay.
    Redo {
        target_op_id: OperationId,
    },
    NotReversible {
        reason: String,
    },
}

/// One row in `ResponseData::Operations`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Operation {
    pub operation_id: OperationId,
    pub kind: OperationKind,
    pub occurred_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<i64>,
    pub source: OperationSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester: Option<String>,
    pub subject_uris: Vec<String>,
    pub reversible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reversal_plan: Option<ReversalPlan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_state: Option<PreState>,
    pub status: OperationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<super::ReceiptId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_op_id: Option<OperationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undone_by_op_id: Option<OperationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redone_by_op_id: Option<OperationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}
