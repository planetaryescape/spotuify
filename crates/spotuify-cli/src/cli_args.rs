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
