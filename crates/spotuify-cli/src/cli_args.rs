//! Clap subcommand enums consumed by `spotuify-cli::commands`.
//!
//! Kept here (rather than in the binary's `src/main.rs`) so the
//! command handler functions and their argument types live together.
//! The binary re-exports these from its `Cli` definition.

use std::path::PathBuf;

use clap::Subcommand;

use crate::output::OutputFormat;

#[derive(clap::ValueEnum, Clone, Copy, Debug, Eq, PartialEq)]
pub enum LyricsFollowFormat {
    Table,
    Jsonl,
}

impl From<LyricsFollowFormat> for OutputFormat {
    fn from(value: LyricsFollowFormat) -> Self {
        match value {
            LyricsFollowFormat::Table => Self::Table,
            LyricsFollowFormat::Jsonl => Self::Jsonl,
        }
    }
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, Eq, PartialEq)]
pub enum VizSourceKindArg {
    Auto,
    Sink,
    Loopback,
    None,
}

impl From<VizSourceKindArg> for spotuify_protocol::VizSourceKindData {
    fn from(value: VizSourceKindArg) -> Self {
        match value {
            VizSourceKindArg::Auto => Self::Auto,
            VizSourceKindArg::Sink => Self::Sink,
            VizSourceKindArg::Loopback => Self::Loopback,
            VizSourceKindArg::None => Self::None,
        }
    }
}

#[derive(Subcommand)]
pub enum VizCommand {
    /// Enable the TUI spectrum visualizer.
    Enable,
    /// Disable the TUI spectrum visualizer.
    Disable,
    /// Select the audio source used by the visualizer.
    Source {
        /// Source kind: auto, sink, loopback, or none.
        #[arg(value_enum)]
        kind: VizSourceKindArg,
    },
    /// Show visualizer status and diagnostics.
    Status {
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
}

#[derive(Subcommand)]
pub enum MprisCommand {
    /// Print media-control registration status.
    Status {
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
}

#[derive(Subcommand)]
pub enum QueueCommand {
    /// Add an item to the current queue.
    Add {
        /// Resource reference(s) to queue.
        uris: Vec<String>,
        /// Read resource references from a file, or `-` for stdin.
        #[arg(long, value_name = "FILE")]
        ids: Option<PathBuf>,
        /// Search for a track and queue the first result.
        #[arg(long)]
        search: Option<String>,
        /// Append all URIs in one batch request (single receipt). Use for
        /// "queue all". Without it, each URI is queued individually.
        #[arg(long)]
        many: bool,
        /// Block until the daemon confirms the mutation with the provider
        /// (non-zero exit if it fails). Default is fire-and-forget.
        #[arg(long)]
        wait: bool,
        /// Provider adapter to route search and resource references through.
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
}

#[derive(Subcommand)]
pub enum ShowCommand {
    /// Print a podcast show's episodes (with listened state).
    Episodes {
        /// Show ID or URI.
        show: String,
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[arg(long, default_value_t = 0)]
        offset: u32,
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
}

#[derive(Subcommand)]
pub enum AlbumCommand {
    /// Print an album's tracks.
    Tracks {
        /// Album ID or URI.
        album: String,
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
}

#[derive(Subcommand)]
pub enum ArtistCommand {
    /// Print an artist's discography (albums, singles, compilations, appears-on).
    Albums {
        /// Artist ID or URI.
        artist: String,
        /// Only albums already in your library (saved albums).
        #[arg(long)]
        library_only: bool,
        /// Restrict to one or more album groups (repeatable). Default: all.
        #[arg(long = "group", value_enum)]
        groups: Vec<AlbumGroup>,
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// List the artists you follow.
    Followed {
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Follow an artist.
    Follow {
        /// Artist ID or URI.
        artist: String,
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Unfollow an artist.
    Unfollow {
        /// Artist ID or URI.
        artist: String,
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Artists related to the given one (Mercury-backed; needs the daemon's
    /// librespot session, since the Web API endpoint was deprecated).
    Related {
        /// Artist ID or URI.
        artist: String,
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
}

/// Radio stations seeded by any supported resource URI.
#[derive(clap::Subcommand)]
pub enum RadioCommand {
    /// Start a station seeded by a track/artist/album/playlist URI. By
    /// default it queues the resolved tracks onto the active device.
    Start {
        /// Provider resource URI or share link.
        seed: String,
        /// Resolve and print the station without queueing anything.
        #[arg(long)]
        dry_run: bool,
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
}

/// Spotify's per-artist album grouping (`album_group`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum AlbumGroup {
    Album,
    Single,
    Compilation,
    #[value(name = "appears-on")]
    AppearsOn,
}

impl AlbumGroup {
    /// The string Spotify reports in `album_group`.
    pub fn as_api_str(self) -> &'static str {
        match self {
            Self::Album => "album",
            Self::Single => "single",
            Self::Compilation => "compilation",
            Self::AppearsOn => "appears_on",
        }
    }
}

#[derive(Subcommand)]
pub enum ReminderCommand {
    /// Schedule a listening reminder for any media URI (track/album/playlist/
    /// artist/show/episode).
    Create {
        /// Provider resource URI to be reminded about.
        uri: String,
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        /// When to fire: an offset (`+2h`, `+30m`, `+3d`, `+1w`), `tomorrow`,
        /// or an ISO-8601 datetime (`2026-07-01T09:00:00Z`).
        #[arg(long)]
        at: String,
        /// Repeat cadence.
        #[arg(long, default_value = "none")]
        repeat: String,
        /// Optional note shown with the reminder.
        #[arg(long)]
        message: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// List reminder schedules (active only unless `--all`).
    List {
        #[arg(long)]
        all: bool,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Cancel a reminder schedule by id.
    Cancel {
        id: String,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
}

#[derive(Subcommand)]
pub enum NotificationCommand {
    /// List inbox notifications (fired reminders). `--all` includes archived.
    List {
        #[arg(long)]
        all: bool,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Play the media for a notification (marks it done).
    Play {
        id: String,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Queue the media for a notification (marks it done).
    Queue {
        id: String,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Snooze a notification; re-fires after `--for` (default 1h).
    Snooze {
        id: String,
        /// Snooze duration: `15m`, `1h`, `4h`, `1d`.
        #[arg(long = "for")]
        snooze_for: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Dismiss a notification without playing.
    Dismiss {
        id: String,
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
        /// Provider adapter that should own the new playlist.
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Print playlist tracks.
    Tracks {
        /// Playlist ID, URI, or exact name.
        playlist: String,
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Play a playlist.
    Play {
        /// Playlist ID, URI, or exact name.
        playlist: String,
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Add a track or episode to a playlist.
    Add {
        /// Playlist ID, URI, or exact name.
        playlist: String,
        /// Track or episode URI(s).
        uris: Vec<String>,
        /// Read resource references from a file, or `-` for stdin.
        #[arg(long, value_name = "FILE")]
        ids: Option<PathBuf>,
        /// Show the exact mutation without adding to the playlist.
        #[arg(long)]
        dry_run: bool,
        /// Commit a multi-item playlist add without an interactive prompt.
        #[arg(long)]
        yes: bool,
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Remove track or episode occurrences from a playlist.
    Remove {
        /// Playlist ID, URI, or exact name.
        playlist: String,
        /// Track or episode URI(s).
        uris: Vec<String>,
        /// Read resource references from a file, or `-` for stdin.
        #[arg(long, value_name = "FILE")]
        ids: Option<PathBuf>,
        /// Show the exact mutation without removing from the playlist.
        #[arg(long)]
        dry_run: bool,
        /// Commit a multi-item playlist removal without an interactive prompt.
        #[arg(long)]
        yes: bool,
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Add the current track or episode to a playlist.
    AddCurrent {
        /// Playlist ID, URI, or exact name.
        playlist: String,
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Unfollow (effectively delete) a playlist you own.
    ///
    /// Some providers model deletion as the owner unfollowing a playlist.
    /// This is not reversible — the
    /// playlist and its track list are gone from your library.
    Unfollow {
        /// Playlist ID, URI, or exact name.
        playlist: String,
        /// Commit the unfollow without an interactive prompt.
        #[arg(long)]
        yes: bool,
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Replace a playlist's cover art with a custom JPEG.
    ///
    /// The built-in adapter accepts only JPEG and caps the base64 body at
    /// 256 KB. It requires the `ugc-image-upload` OAuth scope — if your
    /// stored token predates spotuify 0.1.23, run `spotuify login`
    /// first.
    SetImage {
        /// Playlist ID, URI, or exact name.
        playlist: String,
        /// Path to a JPEG file (or `-` to read JPEG bytes from stdin).
        #[arg(long, value_name = "FILE")]
        file: PathBuf,
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
}

#[derive(Subcommand)]
pub enum LibraryCommand {
    /// Print cached saved tracks, albums, and shows.
    Tracks {
        /// Maximum cached library rows to print.
        #[arg(long, default_value_t = 100)]
        limit: u32,
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Print liked songs (live `/me/tracks`, with date added).
    SavedTracks {
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[arg(long, default_value_t = 0)]
        offset: u32,
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Print subscribed podcasts (saved shows).
    Shows {
        #[arg(long, default_value_t = 200)]
        limit: u32,
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchSourceArg {
    Local,
    /// Remote provider catalog. `spotify` is accepted as a legacy alias.
    #[value(alias = "spotify")]
    Remote,
    Hybrid,
}

#[derive(Subcommand)]
pub enum LyricsCommand {
    /// Print lyrics for the current or specified track.
    Show {
        /// Provider track URI. Defaults to the current now-playing track.
        #[arg(long)]
        track: Option<String>,
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Follow synced lyrics for the current track.
    Follow {
        /// Number of lyric lines to show in human mode.
        #[arg(long, default_value_t = 3)]
        lines: usize,
        /// Display timing adjustment, e.g. +250ms or -100ms.
        #[arg(long)]
        lead: Option<String>,
        /// Output format. Supports table and jsonl.
        #[arg(long, value_enum, default_value = "table")]
        format: LyricsFollowFormat,
    },
    /// Force-refresh cached lyrics for a provider track URI.
    Fetch {
        /// Provider track URI.
        track_uri: String,
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Export lyrics as an LRC file.
    Export {
        /// Provider track URI.
        track_uri: String,
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        /// Write to a file instead of stdout.
        #[arg(long, value_name = "FILE")]
        output: Option<PathBuf>,
    },
    /// Save a per-track lyrics timing offset, e.g. +50ms or -200ms.
    Offset {
        /// Provider track URI.
        track_uri: String,
        /// Provider to target (defaults to the daemon's default provider).
        #[arg(long)]
        provider: Option<String>,
        /// Offset in milliseconds, with optional `ms` suffix.
        offset: String,
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
    /// Export qualified listens. Not implemented yet; use live hooks.
    Export {
        /// Export target reserved for the future export bridge.
        #[arg(long, value_enum)]
        target: AnalyticsExportTarget,
        #[arg(long)]
        since: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Import historical scrobbles.
    Import {
        /// Compatibility import target; only lastfm is implemented.
        #[arg(long, value_enum)]
        target: Option<AnalyticsExportTarget>,
        #[command(subcommand)]
        command: Option<AnalyticsImportCommand>,
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

#[derive(Subcommand)]
pub enum AnalyticsImportCommand {
    /// Preview/apply Last.fm historical scrobble import.
    Lastfm {
        #[arg(long = "user")]
        user: Option<String>,
        #[arg(long = "api-key")]
        api_key: Option<String>,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        apply: bool,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Show import run status.
    Status {
        run_id: String,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// List unresolved scrobbles for a run.
    Unresolved {
        run_id: String,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Undo promoted analytics facts for a run.
    Undo {
        run_id: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
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
        /// Required to execute a non-dry-run undo.
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
