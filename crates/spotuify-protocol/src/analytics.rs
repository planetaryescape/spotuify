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
    pub elapsed_ms: u128,
}
