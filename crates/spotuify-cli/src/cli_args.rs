//! Clap subcommand enums consumed by `spotuify-cli::commands`.
//!
//! Kept here (rather than in the binary's `src/main.rs`) so the
//! command handler functions and their argument types live together.
//! The binary re-exports these from its `Cli` definition.

use std::path::PathBuf;

use clap::Subcommand;

use crate::output::OutputFormat;

#[derive(Subcommand)]
pub enum QueueCommand {
    /// Add an item to the current queue.
    Add {
        /// Spotify URI(s) to queue.
        uris: Vec<String>,
        /// Read Spotify URI(s) from a file, or `-` for stdin.
        #[arg(long, value_name = "FILE")]
        ids: Option<PathBuf>,
        /// Search for a track and queue the first result.
        #[arg(long)]
        search: Option<String>,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
}

#[derive(Subcommand)]
pub enum PlaylistCommand {
    /// Generate a playlist plan JSON scaffold from a brief.
    Plan {
        /// Plain-language playlist brief.
        brief: String,
        /// Output format.
        #[arg(long, value_enum, default_value = "json")]
        format: OutputFormat,
    },
    /// Create a playlist from resolved candidates.
    Create {
        /// New playlist name.
        name: String,
        /// Resolved candidates JSONL file.
        #[arg(long = "from")]
        from: PathBuf,
        /// Show the exact mutation without creating the playlist.
        #[arg(long)]
        dry_run: bool,
        /// Commit the playlist creation without an interactive prompt.
        #[arg(long)]
        yes: bool,
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Print playlist tracks.
    Tracks {
        /// Playlist ID, URI, or exact name.
        playlist: String,
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Play a playlist.
    Play {
        /// Playlist ID, URI, or exact name.
        playlist: String,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Add a Spotify URI to a playlist.
    Add {
        /// Playlist ID, URI, or exact name.
        playlist: String,
        /// Track or episode URI(s).
        uris: Vec<String>,
        /// Read Spotify URI(s) from a file, or `-` for stdin.
        #[arg(long, value_name = "FILE")]
        ids: Option<PathBuf>,
        /// Show the exact mutation without adding to the playlist.
        #[arg(long)]
        dry_run: bool,
        /// Commit a multi-item playlist add without an interactive prompt.
        #[arg(long)]
        yes: bool,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Add the current track or episode to a playlist.
    AddCurrent {
        /// Playlist ID, URI, or exact name.
        playlist: String,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
}

#[derive(Subcommand)]
pub enum LibraryCommand {
    /// Print cached saved tracks and albums.
    Tracks {
        /// Maximum cached library rows to print.
        #[arg(long, default_value_t = 100)]
        limit: u32,
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
}

// --- Phase 10: analytics derivations ---

/// Window specifier for `analytics top --since`.
/// Accepts `7d`, `30d`, `90d`, `365d`, or `all`.
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum AnalyticsSinceWindow {
    #[clap(name = "7d")]
    SevenDays,
    #[clap(name = "30d")]
    ThirtyDays,
    #[clap(name = "90d")]
    NinetyDays,
    #[clap(name = "365d")]
    YearDays,
    All,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum AnalyticsTopKind {
    Tracks,
    Artists,
    Albums,
    Playlists,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum AnalyticsHabitWindow {
    Day,
    Week,
    Month,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum AnalyticsSearchMode {
    Raw,
    Normalized,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum AnalyticsRediscoveryGap {
    #[clap(name = "30d")]
    ThirtyDays,
    #[clap(name = "90d")]
    NinetyDays,
    #[clap(name = "365d")]
    YearDays,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum AnalyticsExportTarget {
    Listenbrainz,
    Lastfm,
}

#[derive(Subcommand)]
pub enum AnalyticsCommand {
    /// Recompute derived listen facts from raw analytics_events.
    Rebuild {
        /// ISO timestamp to rebuild from; omit for full rebuild.
        #[arg(long)]
        since: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Top-N most-played tracks / artists / albums / playlists.
    Top {
        #[arg(long, value_enum, default_value = "tracks")]
        kind: AnalyticsTopKind,
        #[arg(long, value_enum, default_value = "30d")]
        since: AnalyticsSinceWindow,
        #[arg(long, default_value_t = 25)]
        limit: u32,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Habit metrics bucketed by day / week / month.
    Habits {
        #[arg(long, value_enum, default_value = "week")]
        window: AnalyticsHabitWindow,
        #[arg(long)]
        since: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Search history with optional redaction.
    Search {
        #[arg(long, value_enum, default_value = "raw")]
        mode: AnalyticsSearchMode,
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Tracks worth re-discovering (last listened > gap days ago).
    Rediscovery {
        #[arg(long, value_enum, default_value = "90d")]
        gap: AnalyticsRediscoveryGap,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Export qualified listens to ListenBrainz / Last.fm.
    Export {
        #[arg(long, value_enum)]
        target: AnalyticsExportTarget,
        #[arg(long)]
        since: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Import historical scrobbles from ListenBrainz / Last.fm.
    Import {
        #[arg(long, value_enum)]
        target: AnalyticsExportTarget,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Apply retention prune to raw events + progress samples.
    Prune {
        /// Default is dry-run; pass `--apply` to actually delete.
        #[arg(long)]
        apply: bool,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
}

// --- Phase 12: operation log + undo ---

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum OpsSource {
    Cli,
    Tui,
    Mcp,
    Agent,
    #[clap(name = "daemon-internal")]
    DaemonInternal,
}

#[derive(Subcommand)]
pub enum OpsCommand {
    /// List recorded operations (newest first).
    Log {
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// ISO timestamp or relative (`1h`, `24h`, `2026-05-13`).
        #[arg(long)]
        since: Option<String>,
        #[arg(long, value_enum)]
        source: Option<OpsSource>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Inspect a single operation by id, with optional human diff.
    Show {
        /// Operation id (uuid v7).
        id: String,
        /// Render a human-readable diff of what undo would do.
        #[arg(long)]
        diff: bool,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Undo a recorded operation. Defaults to the last reversible op.
    Undo {
        /// Operation id; omit to undo the last reversible op.
        id: Option<String>,
        /// Predict the reversal without executing.
        #[arg(long)]
        dry_run: bool,
        /// Skip confirmation prompts.
        #[arg(long)]
        yes: bool,
        /// Override snapshot-id conflict detection.
        #[arg(long)]
        force: bool,
        /// Bulk-undo every reversible op newer than this (`1h`, `24h`).
        #[arg(long)]
        since: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Redo a previously-undone operation.
    Redo {
        /// Operation id; omit to redo the last undone op.
        id: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
}

// --- Phase 11: completions + man-page generators ---

#[derive(Subcommand)]
pub enum GenerateCommand {
    /// Emit shell completion script to stdout.
    Completions { shell: clap_complete::Shell },
    /// Emit man-page source (groff) to stdout.
    ManPage,
}
