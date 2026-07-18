//! Phase 10 — analytics derivation IPC types.
//!
//! Request/Response shapes for the `spotuify analytics …` subcommands
//! and the matching MCP tools. Wire-stable: snake_case payloads with
//! `kind`-tagged enums so JSON clients don't need to know the Rust
//! variant order.

use serde::{Deserialize, Serialize};

pub use spotuify_core::HabitWindow;

/// Which leaderboard to compute.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TopKind {
    Tracks,
    Artists,
    Albums,
    Playlists,
}

impl TopKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Tracks => "tracks",
            Self::Artists => "artists",
            Self::Albums => "albums",
            Self::Playlists => "playlists",
        }
    }
}

/// Time window for `analytics top` queries. JSON renders as either
/// `{ "days": 30 }` or `"all"`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SinceWindow {
    Days(u32),
    All,
}

/// `analytics search` mode: raw stored queries vs hashed normalised.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Raw,
    Normalized,
}

/// External scrobbler target for export/import.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExportTarget {
    ListenBrainz,
    LastFm,
}

/// Summary returned by Last.fm historical import dry-run/apply.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnalyticsImportSummary {
    pub run_id: String,
    pub provider: String,
    pub username: String,
    pub dry_run: bool,
    pub fetched: u64,
    pub stored: u64,
    pub duplicates: u64,
    pub resolved: u64,
    pub promoted: u64,
    pub unresolved: u64,
    pub started_at_ms: i64,
    pub finished_at_ms: Option<i64>,
}

/// Durable import run status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnalyticsImportRunStatus {
    pub run_id: String,
    pub provider: String,
    pub username: String,
    pub state: String,
    pub dry_run: bool,
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    pub fetched: u64,
    pub stored: u64,
    pub duplicates: u64,
    pub resolved: u64,
    pub promoted: u64,
    pub unresolved: u64,
    pub cursor: Option<String>,
    pub started_at_ms: i64,
    pub finished_at_ms: Option<i64>,
}

/// One unresolved external scrobble.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UnresolvedScrobble {
    pub id: i64,
    pub scrobbled_at_ms: i64,
    pub artist: String,
    pub track: String,
    pub album: Option<String>,
    pub url: Option<String>,
    pub resolution_status: String,
    pub confidence: Option<f64>,
}

/// Summary returned by import undo.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnalyticsImportUndoSummary {
    pub run_id: String,
    pub dry_run: bool,
    pub listen_facts_removed: u64,
    pub raw_scrobbles_preserved: u64,
}

/// One row in `ResponseData::AnalyticsTop`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TopEntry {
    pub uri: String,
    pub name: String,
    pub subtitle: String,
    pub qualified_count: u32,
    pub skip_count: u32,
    pub total_audible_ms: i64,
    pub last_listened_at_ms: Option<i64>,
}

/// One row in `ResponseData::AnalyticsSearch`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchHistoryEntry {
    pub query: Option<String>,
    pub normalized: String,
    pub query_hash: String,
    pub occurred_at_ms: i64,
    pub result_count: u32,
    pub led_to_listen: bool,
}

/// One row in `ResponseData::AnalyticsRediscovery`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RediscoveryCandidate {
    pub track_uri: String,
    pub name: String,
    pub subtitle: String,
    pub qualified_count: u32,
    pub last_listened_at_ms: i64,
    pub days_since_last_listen: u32,
}

/// Summary returned by `analytics rebuild`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RebuildReport {
    pub events_processed: u64,
    pub listen_facts_emitted: u64,
    pub qualified_listens: u64,
    pub elapsed_ms: u64,
}
