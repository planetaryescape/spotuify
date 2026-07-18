mod actions;
mod agent_playlists;
mod analytics;
mod app;
mod commands;
mod config;
mod daemon;
mod diagnostics;
mod logging;
mod output;
mod protocol;
mod reindex;
mod search;
mod selection;
mod store;
mod sync;
mod tui_actions;
mod ui;

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

use crate::analytics::AnalyticsStore;
use crate::app::{run_tui, TuiExit};
use crate::config::{
    config_path, get_config_value, get_effective_config_value, init_config, set_config_value,
    ConfigPath, EDITABLE_CONFIG_PATHS,
};
use crate::output::OutputFormat;
use spotuify_cli::cli_args::{
    AlbumCommand, ArtistCommand, LibraryCommand, LyricsCommand, MprisCommand, NotificationCommand,
    PlaylistCommand, QueueCommand, RadioCommand, ReminderCommand, ShowCommand, VizCommand,
};
use spotuify_cli::SearchSourceArg as SearchSource;
use spotuify_core::{MediaKind, ProviderId, ResourceUri, UriScheme};
use spotuify_protocol::{AuthCredentialKind, AuthMethodData, AuthStatusData, AuthStrategyData};

#[derive(Parser)]
#[command(name = "spotuify", version, about = "A keyboard-native Spotify TUI")]
struct Cli {
    /// Phase 13 (P13-A) — pick the daemon log format for this run.
    /// Also honoured via `SPOTUIFY_LOG_FORMAT`.
    #[arg(long, global = true, value_parser = ["text", "json"])]
    log_format: Option<String>,

    /// Phase 13 (P13-H) — if set, the CLI never auto-starts the daemon.
    /// Errors with a clear hint when the daemon socket is missing.
    #[arg(long, global = true)]
    no_daemon_start: bool,

    /// Phase 13 (P13-H) — one-shot TOML override (e.g. `-o player.bitrate=160`).
    /// Repeatable. Applies for this invocation only; the config file
    /// on disk is unchanged.
    #[arg(
        short = 'o',
        long = "set",
        global = true,
        value_name = "key.path=value"
    )]
    set: Vec<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Guided BYO Spotify app setup: config, browser login, and initial Spotify sync.
    Onboard {
        /// Spotify provider ID to configure; defaults to the configured Spotify adapter.
        #[arg(long)]
        provider: Option<String>,
    },
    /// Log in to Spotify in your browser and store a refresh token in the local auth file.
    Login {
        /// Provider ID to authenticate; defaults to the configured default provider.
        #[arg(long)]
        provider: Option<String>,
        /// Override the redirect URI (only used with your own SPOTUIFY_CLIENT_ID app).
        #[arg(long)]
        redirect_uri: Option<String>,
        /// Force the dev-app (BYO client) login even if a first-party credential is
        /// stored (migrates off rate-limited first-party auth).
        #[arg(long)]
        dev_app: bool,
    },
    /// Remove the stored Spotify token from the local auth file.
    Logout {
        /// Provider ID whose credentials should be removed.
        #[arg(long)]
        provider: Option<String>,
    },
    /// Authentication-adjacent debug commands.
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Check config, auth, Spotify API access, and visible devices.
    Doctor {
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Manage the local spotuify daemon.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Run the MCP server transport.
    Mcp {
        /// Run JSON-RPC 2.0 over stdio. This is the default transport.
        #[arg(long, conflicts_with = "http")]
        stdio: bool,
        /// Run Streamable HTTP transport on loopback ADDR.
        #[arg(long, value_name = "ADDR")]
        http: Option<String>,
    },
    /// Print current playback state.
    Status {
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// List visible Spotify Connect devices.
    Devices {
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// List configured provider adapters and their capabilities.
    Providers {
        #[command(subcommand)]
        command: ProvidersCommand,
    },
    /// Search a provider catalog or the local cache.
    Search {
        /// Search query.
        query: String,
        /// Media type to search.
        #[arg(long = "type", value_enum, default_value = "all")]
        kind: SearchKind,
        /// Where to search. `remote` queries the selected provider catalog.
        /// `local` queries only the local Tantivy index (offline / library lookup).
        /// `hybrid` returns local cached hits and refreshes the provider in the background.
        #[arg(long, value_enum, default_value = "remote")]
        source: SearchSource,
        /// Provider adapter to search.
        #[arg(long)]
        provider: Option<String>,
        /// Maximum results to return (the selected provider may clamp this).
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Pages of 10 to request per media type. `1` = one-shot (current
        /// behavior, up to 60 items). `2`-`3` aggregate pages via
        /// `SearchStream` before printing; `3` matches the TUI fanout
        /// (up to 180 items) and is the maximum — higher values clamp.
        #[arg(long = "pages", default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=3))]
        pages: u8,
        /// Play one result instead of printing results.
        #[arg(long)]
        play: bool,
        /// 1-based search result index for --play.
        #[arg(long, default_value_t = 1)]
        index: usize,
        /// Sort results (relevance keeps provider order).
        #[arg(long, value_enum, default_value = "relevance")]
        sort: SearchSortArg,
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Fetch a single page (10 items) of search results at a specific
    /// offset. Mirrors the TUI's scroll-load-more flow — useful for
    /// scripts walking past the 180-item streaming horizon.
    SearchPage {
        /// Search query.
        query: String,
        /// Media kind to fetch.
        #[arg(long = "type", value_enum, default_value = "track")]
        kind: SearchKindSingle,
        /// Offset (multiple of 10; provider limits apply).
        #[arg(long, default_value_t = 0)]
        offset: u32,
        /// Provider adapter to search.
        #[arg(long)]
        provider: Option<String>,
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Resolve playlist-plan track candidates.
    ResolveTracks {
        /// Playlist plan JSON file.
        #[arg(long = "from")]
        from: PathBuf,
        /// Provider adapter to search for candidates.
        #[arg(long)]
        provider: Option<String>,
        /// Output format.
        #[arg(long, value_enum, default_value = "jsonl")]
        format: OutputFormat,
    },
    /// Print the current playback queue.
    Queue {
        #[command(subcommand)]
        command: Option<QueueCommand>,
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// List the current user's playlists.
    Playlists {
        #[arg(long)]
        provider: Option<String>,
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Listening history grouped into sessions. Use --flat for a chronological track list.
    History {
        /// Maximum number of sessions to return.
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Print a flat chronological track list instead of sessions.
        #[arg(long)]
        flat: bool,
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Search a provider and play the first matching result. Recognized
    /// resource references skip search and play directly.
    Play {
        /// Search query, provider URI, or recognized share link.
        query: String,
        /// Media type to search.
        #[arg(long = "type", value_enum, default_value = "track")]
        kind: SearchKind,
        #[arg(long)]
        provider: Option<String>,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Play a provider resource directly.
    PlayUri {
        /// Provider URI, share link, or bare ID.
        uri: String,
        #[arg(long)]
        provider: Option<String>,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Skip to the next track.
    Next {
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Skip to the previous track.
    Previous {
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Pause playback.
    Pause {
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Resume playback.
    Resume {
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Toggle play/pause.
    Toggle {
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Seek relative to current playback position or to an absolute time.
    Seek {
        /// Seek target, e.g. +15s, -30s, 90s, or 2m.
        offset: String,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Set playback volume percent.
    Volume {
        /// Volume percent, clamped to 0..100.
        percent: u8,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Set or toggle shuffle.
    Shuffle {
        /// Shuffle state.
        #[arg(value_enum)]
        state: ToggleArg,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Set repeat mode.
    Repeat {
        /// Repeat state.
        #[arg(value_enum)]
        state: RepeatArg,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Transfer playback to a visible device by ID or name.
    Transfer {
        /// Device ID or exact name.
        device: String,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Playlist operations.
    Playlist {
        #[command(subcommand)]
        command: PlaylistCommand,
    },
    /// Cached library operations.
    Library {
        #[command(subcommand)]
        command: LibraryCommand,
    },
    /// Podcast show operations.
    Show {
        #[command(subcommand)]
        command: ShowCommand,
    },
    /// Album operations.
    Album {
        #[command(subcommand)]
        command: AlbumCommand,
    },
    /// Artist operations.
    Artist {
        #[command(subcommand)]
        command: ArtistCommand,
    },
    /// Mercury-backed radio stations.
    Radio {
        #[command(subcommand)]
        command: RadioCommand,
    },
    /// Synced lyrics operations.
    Lyrics {
        #[command(subcommand)]
        command: LyricsCommand,
    },
    /// Schedule and manage listening reminders.
    Reminder {
        #[command(subcommand)]
        command: ReminderCommand,
    },
    /// View and act on reminder notifications (the inbox).
    Notifications {
        #[command(subcommand)]
        command: NotificationCommand,
    },
    /// Refresh current track cover art and lyrics.
    RefreshMedia {
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Configure the audio visualizer.
    Viz {
        #[command(subcommand)]
        command: VizCommand,
    },
    /// Test configured shell hooks.
    Hooks {
        #[command(subcommand)]
        command: HooksCommand,
    },
    /// Inspect OS media-control integration.
    Mpris {
        #[command(subcommand)]
        command: MprisCommand,
    },
    /// Save/like a resource or the current now-playing item.
    Like {
        /// Resource reference or `current`.
        target: String,
        /// Block until the daemon confirms the save with the provider
        /// (non-zero exit if it fails). Default is fire-and-forget.
        #[arg(long)]
        wait: bool,
        #[arg(long)]
        provider: Option<String>,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Remove (un-like) a resource from the library.
    Unlike {
        /// Provider URI, share link, or bare ID.
        target: String,
        /// Block until the daemon confirms with the provider (non-zero exit
        /// if it fails). Default is fire-and-forget.
        #[arg(long)]
        wait: bool,
        #[arg(long)]
        provider: Option<String>,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Save a resource or the current now-playing item.
    Save {
        /// Resource reference or `current`.
        target: String,
        /// Block until the daemon confirms the save with the provider
        /// (non-zero exit if it fails). Default is fire-and-forget.
        #[arg(long)]
        wait: bool,
        #[arg(long)]
        provider: Option<String>,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Show spotuify log file location or recent log lines.
    Logs {
        #[command(subcommand)]
        command: LogsCommand,
    },
    /// Read or write the current instance config file.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Inspect local analytics data.
    Analytics {
        #[command(subcommand)]
        command: AnalyticsCommand,
    },
    /// Inspect / undo / redo recorded operations (Phase 12).
    Ops {
        #[command(subcommand)]
        command: OpsCommand,
    },
    /// Phase 13 (P13-J) — emit shell completions or a man page.
    Generate {
        #[command(subcommand)]
        command: GenerateCommand,
    },
    /// Phase 13 (P13-I) — ask the running daemon to reload `config.toml`.
    Reload,
    /// Phase 13 (P13-I) — force the daemon to re-register its embedded
    /// player (after a VPN flap, network change, etc).
    Reconnect,
    /// List the local audio output devices the embedded player can render
    /// to (the system speakers/headphones spotuify-hume plays through).
    AudioOutputs {
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Choose which local audio output the embedded player renders to.
    /// Applies live: the daemon rebinds its sink in-process and resumes
    /// the interrupted track where it left off. Pass `default` (or
    /// empty) to follow the system default output again. Name must
    /// match one from `spotuify audio-outputs`.
    AudioOutput {
        /// Output device name, or `default` to clear.
        name: String,
    },
    /// Phase 13 (P13-D) — bundle a redacted diagnostic tarball for
    /// bug reports. Never auto-uploads; the user inspects + shares it.
    BugReport {
        /// Last N log lines to include (default 200).
        #[arg(long, visible_alias = "include-logs", default_value_t = 200)]
        log_lines: usize,
        /// Output path. Defaults to ./spotuify-bug-report-<ts>.tar.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Rebuild the local search index from SQLite cache.
    Reindex {
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Inspect local cache state.
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },
    /// Refresh the local cache from a provider.
    Sync {
        /// Cache domain to refresh.
        #[arg(value_enum, default_value = "all")]
        target: SyncTarget,
        /// Prune search-cache entries older than the retention window.
        #[arg(long)]
        prune: bool,
        /// Retention window for `sync search-cache --prune`, e.g. `7d`.
        #[arg(long)]
        older_than: Option<String>,
        /// Provider adapter to refresh.
        #[arg(long)]
        provider: Option<String>,
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Check whether a newer spotuify release is available and how to upgrade.
    Update {
        /// Force a fresh check now instead of returning the cached result.
        #[arg(long)]
        force: bool,
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// A flat, date-ordered episode feed across all the podcasts you follow.
    Episodes {
        /// How to order the feed.
        #[arg(long, value_enum, default_value = "newest")]
        sort: EpisodeSortArg,
        /// Maximum episodes to return.
        #[arg(long, default_value_t = 100)]
        limit: u32,
        /// Bypass the cached feed and re-fetch from the provider now.
        #[arg(long)]
        refresh: bool,
        #[arg(long)]
        provider: Option<String>,
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
}

#[derive(Subcommand)]
enum ProvidersCommand {
    /// List configured providers and semantic capabilities.
    List {
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum SearchKind {
    All,
    Track,
    Episode,
    Show,
    Album,
    Artist,
    Playlist,
}

/// Single-kind variant of `SearchKind` for `search-page` (the API
/// requires exactly one kind per offset-paginated call).
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum SearchKindSingle {
    Track,
    Episode,
    Show,
    Album,
    Artist,
    Playlist,
}

impl From<SearchKindSingle> for spotuify_core::MediaKind {
    fn from(kind: SearchKindSingle) -> Self {
        match kind {
            SearchKindSingle::Track => Self::Track,
            SearchKindSingle::Episode => Self::Episode,
            SearchKindSingle::Show => Self::Show,
            SearchKindSingle::Album => Self::Album,
            SearchKindSingle::Artist => Self::Artist,
            SearchKindSingle::Playlist => Self::Playlist,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum SyncTarget {
    All,
    Playback,
    Queue,
    Devices,
    Playlists,
    Recent,
    Library,
    SearchCache,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ToggleArg {
    On,
    Off,
    Toggle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum RepeatArg {
    Off,
    Context,
    Track,
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Show secret-free daemon-owned authentication state.
    Status {
        /// Provider ID to inspect; defaults to the configured default provider.
        #[arg(long)]
        provider: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Print the daemon's current Spotify Web API bearer token.
    ///
    /// The daemon owns live Web API bearers for modes that need daemon-side
    /// token minting; this command surfaces the current one so you can probe
    /// `api.spotify.com` directly. Treat the output as a secret; printing it
    /// requires `--reveal-secret`.
    Bearer {
        /// Force minting a fresh bearer even if the cached one is
        /// still valid. Use after a `logout` + `login` round-trip.
        #[arg(long)]
        force: bool,
        /// Output format. `table` prints just the token; `json` wraps
        /// it for piping into `jq`.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
        /// Actually print the live bearer token.
        #[arg(long)]
        reveal_secret: bool,
    },
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Start the daemon.
    Start {
        /// Run in the foreground for debugging or launchd/systemd.
        #[arg(long)]
        foreground: bool,
    },
    /// Stop the daemon.
    Stop,
    /// Restart the daemon with the current binary.
    Restart,
    /// Show daemon socket and lifecycle status.
    Status {
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Install the platform-appropriate auto-start service (launchd /
    /// systemd user / Windows Task Scheduler).
    InstallService,
    /// Remove the auto-start service registration.
    UninstallService,
}

#[derive(Subcommand)]
enum CacheCommand {
    /// Show local cache row counts and freshness.
    Status {
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Delete local SQLite cache and search index. Requires --confirm.
    Reset {
        /// Confirm destructive local cache deletion.
        #[arg(long)]
        confirm: bool,
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Replay cache migrations and rebuild the local search index.
    Repair {
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
}

/// CLI sort options for the cross-show episode feed (`spotuify episodes`).
#[derive(Copy, Clone, Debug, ValueEnum)]
enum EpisodeSortArg {
    Newest,
    Oldest,
    Duration,
    Title,
    Show,
}

impl From<EpisodeSortArg> for protocol::EpisodeSort {
    fn from(arg: EpisodeSortArg) -> Self {
        match arg {
            EpisodeSortArg::Newest => Self::Newest,
            EpisodeSortArg::Oldest => Self::Oldest,
            EpisodeSortArg::Duration => Self::Duration,
            EpisodeSortArg::Title => Self::Title,
            EpisodeSortArg::Show => Self::Show,
        }
    }
}

impl From<SearchKind> for protocol::SearchScopeData {
    fn from(kind: SearchKind) -> Self {
        match kind {
            SearchKind::All => Self::All,
            SearchKind::Track => Self::Track,
            SearchKind::Episode => Self::Episode,
            SearchKind::Show => Self::Show,
            SearchKind::Album => Self::Album,
            SearchKind::Artist => Self::Artist,
            SearchKind::Playlist => Self::Playlist,
        }
    }
}

/// CLI flag for `--sort` on `search`. `Relevance` (default) keeps Spotify's own
/// ordering; the daemon applies the others after fetch.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum SearchSortArg {
    Relevance,
    Name,
    Duration,
    Artist,
    /// Newest release first (episodes/shows).
    Date,
}

impl SearchSortArg {
    /// `None` for `Relevance` so the wire stays compact and the daemon skips
    /// the sort entirely.
    fn into_data(self) -> Option<protocol::SearchSortData> {
        match self {
            SearchSortArg::Relevance => None,
            SearchSortArg::Name => Some(protocol::SearchSortData::Name),
            SearchSortArg::Duration => Some(protocol::SearchSortData::Duration),
            SearchSortArg::Artist => Some(protocol::SearchSortData::Artist),
            SearchSortArg::Date => Some(protocol::SearchSortData::Date),
        }
    }
}

impl From<SyncTarget> for protocol::SyncTargetData {
    fn from(target: SyncTarget) -> Self {
        match target {
            SyncTarget::All => Self::All,
            SyncTarget::Playback => Self::Playback,
            SyncTarget::Queue => Self::Queue,
            SyncTarget::Devices => Self::Devices,
            SyncTarget::Playlists => Self::Playlists,
            SyncTarget::Recent => Self::Recent,
            SyncTarget::Library => Self::Library,
            SyncTarget::SearchCache => Self::All,
        }
    }
}

fn repeat_arg_value(state: RepeatArg) -> spotuify_core::RepeatMode {
    match state {
        RepeatArg::Off => spotuify_core::RepeatMode::Off,
        RepeatArg::Context => spotuify_core::RepeatMode::Context,
        RepeatArg::Track => spotuify_core::RepeatMode::Track,
    }
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Print the config path.
    Path,
    /// Create the config file if it does not exist.
    Init,
    /// Print a config value.
    Get {
        key: String,
        /// Read only the persisted file value, without defaults or overrides.
        #[arg(long)]
        raw: bool,
        /// Print sensitive values instead of `<redacted>`.
        #[arg(long)]
        reveal_secret: bool,
    },
    /// Set a config value.
    Set { key: String, value: String },
    /// Print every config key + value (the whole editable config). Drives the
    /// macOS Settings window's visual config editor.
    Show {
        /// Read only persisted file values, without defaults or overrides.
        #[arg(long)]
        raw: bool,
        /// Print sensitive values instead of `<redacted>`.
        #[arg(long)]
        reveal_secret: bool,
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
}

#[derive(Subcommand)]
enum LogsCommand {
    /// Print the log path.
    Path,
    /// Print recent log lines.
    Tail {
        /// Number of lines to print.
        #[arg(default_value_t = 80)]
        lines: usize,
        /// Phase 13 (P13-C) — keep printing as new lines arrive
        /// (poll the log file every 500ms; Ctrl-C to exit).
        #[arg(long)]
        follow: bool,
        /// Output format: text (default), json/jsonl (pass-through).
        #[arg(long, default_value = "text", value_parser = ["text", "json", "jsonl"])]
        format: String,
    },
}

#[derive(Subcommand)]
enum HooksCommand {
    /// Invoke the configured hook with a sample listen-qualified event.
    Test {
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
}

#[derive(Subcommand)]
enum AnalyticsCommand {
    /// Print recent analytics events.
    Events {
        /// Maximum events to print.
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Top-N most-played tracks / artists / albums / playlists.
    Top {
        /// `tracks` (default) | `artists` | `albums` | `playlists`.
        #[arg(long, default_value = "tracks")]
        kind: String,
        /// Time window: `7d`, `30d`, `90d`, `365d`, or `all`.
        #[arg(long, default_value = "30d")]
        since: String,
        #[arg(long, default_value_t = 25)]
        limit: u32,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Habit metrics bucketed by `day` / `week` / `month`.
    Habits {
        #[arg(long, default_value = "week")]
        window: String,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Search history (raw or normalized mode).
    Search {
        #[arg(long, default_value = "raw")]
        mode: String,
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Tracks worth re-discovering.
    Rediscovery {
        #[arg(long, default_value = "90d")]
        gap: String,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Recompute derived listen facts from analytics_events.
    Rebuild {
        #[arg(long)]
        since: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Apply retention prune (default: dry-run).
    Prune {
        #[arg(long)]
        apply: bool,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Export qualified listens. Not implemented yet; use live hooks.
    Export {
        /// Export target reserved for the future export bridge.
        #[arg(long)]
        target: String,
        #[arg(long)]
        since: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Import historical scrobbles.
    Import {
        /// Compatibility alias: `analytics import --target lastfm`.
        #[arg(long)]
        target: Option<String>,
        #[command(subcommand)]
        command: Option<AnalyticsImportCommand>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
}

#[derive(Subcommand)]
enum AnalyticsImportCommand {
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
    /// Undo promoted analytics effects while preserving raw scrobbles.
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

#[derive(Subcommand)]
enum OpsCommand {
    /// List recorded operations (newest first).
    Log {
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// `1h`, `24h`, or ISO date.
        #[arg(long)]
        since: Option<String>,
        /// Filter by `cli` / `tui` / `mcp` / `agent` / `daemon-internal`.
        #[arg(long)]
        source: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Inspect a single operation by id.
    Show {
        id: String,
        /// Render a human-readable diff of what undo would do.
        #[arg(long)]
        diff: bool,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Undo a recorded operation. Defaults to the last reversible op.
    Undo {
        id: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        force: bool,
        /// Bulk-undo every reversible op newer than this (e.g. `1h`).
        #[arg(long)]
        since: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Redo a previously-undone operation.
    Redo {
        id: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
}

#[derive(Subcommand)]
enum GenerateCommand {
    /// Emit shell completions for the given shell to stdout.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Emit a roff man page (section 1) to stdout.
    ManPage,
}

#[cfg(windows)]
const WINDOWS_MAIN_STACK_SIZE: usize = 8 * 1024 * 1024;

fn main() {
    #[cfg(windows)]
    {
        run_on_windows_stack();
    }

    #[cfg(not(windows))]
    {
        run_main();
    }
}

#[cfg(windows)]
fn run_on_windows_stack() {
    let handle = std::thread::Builder::new()
        .name("spotuify-main".to_string())
        .stack_size(WINDOWS_MAIN_STACK_SIZE)
        .spawn(run_main)
        .expect("failed to start spotuify main thread");
    if let Err(payload) = handle.join() {
        std::panic::resume_unwind(payload);
    }
}

fn run_main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to initialize tokio runtime");
    if let Err(err) = runtime.block_on(Box::pin(run())) {
        eprintln!("error: {err:#}");
        std::process::exit(exit_code_for_error(&err));
    }
}

async fn run() -> Result<()> {
    // Phase 13 (P13-A) — `--log-format json` overrides the env-default.
    // Parse the CLI once *before* we initialise tracing so the format
    // flag lands; the second parse below is a no-op cost-wise (clap
    // arg parsing is cheap) and keeps the rest of the code unchanged.
    let cli = Cli::parse();
    let log_format = match cli.log_format.as_deref() {
        Some("json") => logging::LogFormat::Json,
        Some("text") => logging::LogFormat::Text,
        _ => logging::LogFormat::from_env_or_default(),
    };
    if cli.no_daemon_start {
        // Threaded through to daemon-client via env var so existing
        // helper code that checks the daemon socket can pick it up
        // without a signature change.
        std::env::set_var("SPOTUIFY_NO_DAEMON_START", "1");
    }
    if !cli.set.is_empty() {
        // Phase 13 (P13-H) — accumulate `-o key.path=value` overrides
        // into an env-var the config loader picks up. The shell shape
        // is `key.path=value\nkey2.path=value\n…`.
        let payload = cli.set.join("\n");
        std::env::set_var("SPOTUIFY_CONFIG_OVERRIDES", payload);
    }
    spotuify_protocol::paths::secure_current_instance_dirs()
        .context("failed to secure spotuify state directories")?;
    let _log_guard =
        logging::init_with_format(log_format).context("failed to initialize logging")?;
    logging::install_panic_hook();
    logging::surface_prior_panic_if_any();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "spotuify starting");

    match cli.command {
        Some(Command::Onboard { provider }) => onboard(provider).await,
        Some(Command::Logs { command }) => handle_logs(command),
        Some(Command::Config { command }) => handle_config(command),
        Some(Command::Analytics { command }) => handle_analytics(command).await,
        Some(Command::Ops { command }) => handle_ops(command).await,
        Some(Command::Generate { command }) => handle_generate(command),
        Some(Command::Hooks { command }) => handle_hooks(command).await,
        Some(Command::Mpris { command }) => commands::ipc_mpris(command).await,
        Some(Command::Reload) => commands::ipc_reload().await,
        Some(Command::Reconnect) => commands::ipc_reconnect().await,
        Some(Command::AudioOutputs { format }) => commands::ipc_audio_outputs(format).await,
        Some(Command::AudioOutput { name }) => commands::ipc_set_audio_output(&name).await,
        Some(Command::BugReport { log_lines, output }) => bug_report(log_lines, output).await,
        Some(Command::Login {
            provider,
            redirect_uri,
            dev_app,
        }) => {
            let status = auth_status_for_login(provider.as_deref()).await?;
            let provider_id = status.provider;
            if let Some(redirect_uri) = redirect_uri {
                set_config_value(
                    &provider_config_path(&provider_id, "redirect_uri")?,
                    &redirect_uri,
                )?;
            }
            let method = dev_app.then(|| "dev_app".to_string());
            commands::ipc_login(Some(provider_id.to_string()), method).await
        }
        Some(Command::Logout { provider }) => {
            let result = commands::ipc_logout(provider).await?;
            println!("Logged out of {}.", result.provider);
            Ok(())
        }
        Some(Command::Auth { command }) => match command {
            AuthCommand::Status { provider, format } => auth_status(provider, format).await,
            AuthCommand::Bearer {
                force,
                format,
                reveal_secret,
            } => auth_bearer(force, format, reveal_secret).await,
        },
        Some(Command::Doctor { format }) => doctor(format).await,
        Some(Command::Daemon { command }) => handle_daemon(command).await,
        Some(Command::Mcp {
            http: Some(addr), ..
        }) => spotuify_mcp::http::serve(addr.parse().context("invalid MCP HTTP address")?).await,
        Some(Command::Mcp { .. }) => tokio::task::spawn_blocking(spotuify_mcp::stdio::run)
            .await
            .context("MCP stdio task failed")?,
        Some(Command::Status { format }) => commands::ipc_status(format).await,
        Some(Command::Devices { format }) => commands::ipc_devices(format).await,
        Some(Command::Providers { command }) => match command {
            ProvidersCommand::List { format } => commands::ipc_providers_list(format).await,
        },
        Some(Command::Search {
            query,
            kind,
            source,
            limit,
            pages,
            play,
            index,
            sort,
            provider,
            format,
        }) => {
            commands::ipc_search(
                &query,
                kind.into(),
                source,
                limit,
                pages,
                play,
                index,
                sort.into_data(),
                provider,
                format,
            )
            .await
        }
        Some(Command::History {
            limit,
            flat,
            format,
        }) => commands::ipc_history(limit, flat, format).await,
        Some(Command::Update { force, format }) => commands::ipc_update(force, format).await,
        Some(Command::Episodes {
            sort,
            limit,
            refresh,
            provider,
            format,
        }) => commands::ipc_episodes(limit, sort.into(), refresh, provider, format).await,
        Some(Command::SearchPage {
            query,
            kind,
            offset,
            provider,
            format,
        }) => commands::ipc_search_page(&query, kind.into(), offset, provider, format).await,
        Some(Command::ResolveTracks {
            from,
            provider,
            format,
        }) => commands::ipc_resolve_tracks(&from, provider, format).await,
        Some(Command::Queue { command, format }) => commands::ipc_queue(command, format).await,
        Some(Command::Playlists { provider, format }) => {
            commands::ipc_playlists(provider, format).await
        }
        Some(Command::Play {
            query,
            kind,
            provider,
            format,
        }) => commands::ipc_play_query(&query, kind.into(), provider, format).await,
        Some(Command::PlayUri {
            uri,
            provider,
            format,
        }) => commands::ipc_play_uri(&uri, provider, format).await,
        Some(Command::Next { format }) => {
            commands::ipc_playback_command(crate::protocol::PlaybackCommand::Next, format).await
        }
        Some(Command::Previous { format }) => {
            commands::ipc_playback_command(crate::protocol::PlaybackCommand::Previous, format).await
        }
        Some(Command::Pause { format }) => {
            commands::ipc_playback_command(crate::protocol::PlaybackCommand::Pause, format).await
        }
        Some(Command::Resume { format }) => {
            commands::ipc_playback_command(crate::protocol::PlaybackCommand::Resume, format).await
        }
        Some(Command::Toggle { format }) => {
            commands::ipc_playback_command(crate::protocol::PlaybackCommand::Toggle, format).await
        }
        Some(Command::Seek { offset, format }) => {
            // Phase 5 — typed parse client-side, daemon resolves
            // relative offsets against its `PlaybackClock`. Eliminates
            // the "relative seek lands somewhere surprising" bug caused
            // by the CLI reading a stale cached progress before sending
            // an absolute target.
            let cmd = match selection::parse_seek_input(&offset)? {
                selection::SeekInput::Absolute(position_ms) => {
                    crate::protocol::PlaybackCommand::Seek { position_ms }
                }
                selection::SeekInput::Relative(offset_ms) => {
                    crate::protocol::PlaybackCommand::SeekRelative { offset_ms }
                }
            };
            commands::ipc_playback_command(cmd, format).await
        }
        Some(Command::Volume { percent, format }) => {
            commands::ipc_playback_command(
                crate::protocol::PlaybackCommand::Volume {
                    volume_percent: percent,
                },
                format,
            )
            .await
        }
        Some(Command::Shuffle { state, format }) => {
            let state = match state {
                ToggleArg::On => true,
                ToggleArg::Off => false,
                ToggleArg::Toggle => {
                    let playback = commands::daemon_current_playback()
                        .await?
                        .unwrap_or_default();
                    !playback.shuffle
                }
            };
            commands::ipc_playback_command(
                crate::protocol::PlaybackCommand::Shuffle { state },
                format,
            )
            .await
        }
        Some(Command::Repeat { state, format }) => {
            commands::ipc_playback_command(
                crate::protocol::PlaybackCommand::Repeat {
                    state: repeat_arg_value(state),
                },
                format,
            )
            .await
        }
        Some(Command::Transfer { device, format }) => commands::ipc_transfer(&device, format).await,
        Some(Command::Playlist { command }) => commands::ipc_playlist(command).await,
        Some(Command::Library { command }) => commands::ipc_library(command).await,
        Some(Command::Show { command }) => commands::ipc_show(command).await,
        Some(Command::Album { command }) => commands::ipc_album(command).await,
        Some(Command::Artist { command }) => commands::ipc_artist(command).await,
        Some(Command::Radio { command }) => commands::ipc_radio(command).await,
        Some(Command::Lyrics { command }) => commands::ipc_lyrics(command).await,
        Some(Command::Reminder { command }) => commands::ipc_reminder(command).await,
        Some(Command::Notifications { command }) => commands::ipc_notifications(command).await,
        Some(Command::RefreshMedia { format }) => commands::ipc_refresh_media(format).await,
        Some(Command::Viz { command }) => commands::ipc_viz(command).await,
        Some(Command::Like {
            target,
            wait,
            provider,
            format,
        }) => commands::ipc_save_target("like", &target, wait, provider, format).await,
        Some(Command::Unlike {
            target,
            wait,
            provider,
            format,
        }) => commands::ipc_unsave_target(&target, wait, provider, format).await,
        Some(Command::Save {
            target,
            wait,
            provider,
            format,
        }) => commands::ipc_save_target("save", &target, wait, provider, format).await,
        Some(Command::Reindex { format }) => commands::ipc_reindex(format).await,
        Some(Command::Cache { command }) => match command {
            CacheCommand::Status { format } => commands::ipc_cache_status(format).await,
            CacheCommand::Reset { confirm, format } => cache_reset(confirm, format).await,
            CacheCommand::Repair { format } => cache_repair(format).await,
        },
        Some(Command::Sync {
            target: SyncTarget::SearchCache,
            prune,
            older_than,
            provider,
            format,
        }) => {
            if provider.is_some() {
                anyhow::bail!("--provider is not valid with `sync search-cache`");
            }
            if !prune {
                anyhow::bail!("sync search-cache requires --prune");
            }
            let older_than_ms = match older_than.as_deref() {
                Some(raw) => Some(parse_iso_or_relative(raw).with_context(|| {
                    format!("invalid --older-than `{raw}`; expected `7d`, `24h`, or unix-ms")
                })?),
                None => None,
            };
            send_and_render(
                spotuify_protocol::Request::SearchCachePrune { older_than_ms },
                format,
            )
            .await
        }
        Some(Command::Sync {
            target,
            prune,
            older_than,
            provider,
            format,
        }) => {
            if prune || older_than.is_some() {
                anyhow::bail!("--prune/--older-than are only valid with `sync search-cache`");
            }
            commands::ipc_sync(target.into(), provider, format).await
        }
        None => {
            if needs_onboarding().await? {
                onboard(None).await?;
            }
            run_tui_client_loop().await
        }
    }
}

async fn run_tui_client_loop() -> Result<()> {
    loop {
        daemon::server::ensure_daemon_running().await?;
        match run_tui().await? {
            TuiExit::Quit => return Ok(()),
            TuiExit::RestartDaemon => {
                daemon::server::restart_daemon().await?;
            }
        }
    }
}

fn exit_code_for_error(err: &anyhow::Error) -> i32 {
    // Structured kind first: the daemon told us exactly what failed.
    // Substring matching below is the FALLBACK for non-IPC errors only
    // — matched against prose that can embed user input ("no provider
    // result for `login to my heart`" is not an auth failure).
    if let Some(daemon_err) = err
        .chain()
        .find_map(|cause| cause.downcast_ref::<spotuify_cli::commands::DaemonRequestError>())
    {
        use spotuify_protocol::IpcErrorKind as K;
        return match daemon_err.kind {
            K::InvalidRequest => 2,
            K::Auth | K::AuthRevoked => 4,
            K::RateLimited => 6,
            K::Unsupported => 7,
            K::Network | K::Timeout | K::Provider | K::Internal => 1,
        };
    }
    let message = err
        .chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();

    if message.contains("provide ")
        || message.contains("invalid ")
        || message.contains("expected ")
        || message.contains("no result #")
        || message.contains("re-run with --confirm")
    {
        return 2;
    }
    if message.contains("cannot connect to daemon") || message.contains("daemon unavailable") {
        return 3;
    }
    if message.contains("auth") || message.contains("oauth") || message.contains("login") {
        return 4;
    }
    if message.contains("no active device") {
        return 5;
    }
    if message.contains("rate limited") || message.contains("rate limit") {
        return 6;
    }
    if message.contains("unsupported") || message.contains("not supported") {
        return 7;
    }
    if message.contains("partial") {
        return 8;
    }
    1
}

async fn handle_daemon(command: DaemonCommand) -> Result<()> {
    match command {
        DaemonCommand::Start { foreground } => {
            if let Some(status) = daemon::server::start_daemon(foreground).await? {
                daemon::status::print_status(&status, OutputFormat::Table)?;
            }
        }
        DaemonCommand::Stop => {
            daemon::server::stop_daemon().await?;
            println!("daemon stopped");
        }
        DaemonCommand::Restart => {
            if let Some(status) = daemon::server::restart_daemon().await? {
                daemon::status::print_status(&status, OutputFormat::Table)?;
            }
        }
        DaemonCommand::Status { format } => {
            let status = daemon::server::daemon_status().await?;
            daemon::status::print_status(&status, format)?;
        }
        DaemonCommand::InstallService => install_platform_service()?,
        DaemonCommand::UninstallService => uninstall_platform_service()?,
    }
    Ok(())
}

async fn cache_reset(confirm: bool, format: OutputFormat) -> Result<()> {
    if !confirm {
        anyhow::bail!("cache reset is destructive; re-run with --confirm");
    }

    if daemon::server::daemon_status()
        .await
        .is_ok_and(|status| status.socket_reachable)
    {
        daemon::server::stop_daemon()
            .await
            .context("failed to stop running daemon before cache reset")?;
    }

    let db_path = store::cache_db_path()?;
    let index_path = store::search_index_path()?;
    reset_cache_files(&db_path, &index_path)?;
    output::print_basic_receipt(
        "cache-reset",
        "Deleted local cache database and search index",
        format,
    )
}

async fn cache_repair(format: OutputFormat) -> Result<()> {
    let store = store::Store::open_default().await?;
    store.repair_schema().await?;
    let opened = search::SearchIndex::open(store.index_path())?;
    let (search, worker) = search::SearchServiceHandle::start(opened.index);
    let stats = reindex::reindex(&store, &search).await?;
    search.request_shutdown().await?;
    let _ = worker.await;
    output::print_reindex_stats(&stats, format)
}

fn reset_cache_files(db_path: &Path, index_path: &Path) -> Result<()> {
    remove_file_if_exists(db_path)?;
    remove_file_if_exists(&sqlite_sidecar_path(db_path, "-wal"))?;
    remove_file_if_exists(&sqlite_sidecar_path(db_path, "-shm"))?;
    if index_path.exists() {
        std::fs::remove_dir_all(index_path)
            .with_context(|| format!("failed to remove {}", index_path.display()))?;
    }
    Ok(())
}

fn sqlite_sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    let mut path = db_path.to_path_buf();
    let file_name = db_path.file_name().map_or_else(
        || "cache.sqlite3".to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    path.set_file_name(format!("{file_name}{suffix}"));
    path
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to remove {}", path.display())),
    }
}

/// Phase 11 (P11.4) — register the daemon as a platform-appropriate
/// auto-start service. macOS: launchd LaunchAgent. Linux: systemd
/// `--user` unit. Windows: Task Scheduler logon trigger. Each path
/// writes an instance-specific service definition into the right home
/// dir and invokes the platform's `enable` command.
fn install_platform_service() -> Result<()> {
    let instance = spotuify_protocol::paths::app_instance_name();
    let exe = std::env::current_exe()
        .context("failed to resolve current executable for service install")?;

    #[cfg(target_os = "macos")]
    {
        let agents = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("no home dir"))?
            .join("Library/LaunchAgents");
        std::fs::create_dir_all(&agents).context("could not create ~/Library/LaunchAgents")?;
        if instance != "spotuify" {
            remove_legacy_dev_launchd_agent(&agents)?;
        }
        let label = launchd_label(&instance);
        let dest = agents.join(format!("{label}.plist"));
        std::fs::write(&dest, launchd_plist(&label, &exe, &instance))
            .with_context(|| format!("write {dest:?} failed"))?;
        // launchctl bootstrap loads the agent into the current user's
        // GUI session; idempotent — re-running prints "already loaded".
        let uid = std::process::Command::new("id").arg("-u").output()?;
        let uid = String::from_utf8_lossy(&uid.stdout).trim().to_string();
        let status = std::process::Command::new("launchctl")
            .args([
                "bootstrap",
                &format!("gui/{uid}"),
                dest.to_str().unwrap_or_default(),
            ])
            .status()?;
        if !status.success() {
            eprintln!(
                "warning: launchctl bootstrap returned {status}; you may need to `launchctl bootout` first"
            );
        }
        println!("Installed launchd agent for {instance}: {dest:?}");
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        let units = dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("no config dir"))?
            .join("systemd/user");
        std::fs::create_dir_all(&units).context("could not create ~/.config/systemd/user")?;
        let unit_name = systemd_unit_name(&instance);
        let dest = units.join(format!("{unit_name}.service"));
        std::fs::write(&dest, systemd_unit(&exe, &instance))
            .with_context(|| format!("write {dest:?} failed"))?;
        std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status()
            .ok();
        let status = std::process::Command::new("systemctl")
            .args(["--user", "enable", "--now", &unit_name])
            .status()?;
        if !status.success() {
            anyhow::bail!("`systemctl --user enable --now {unit_name}` failed");
        }
        println!("Installed systemd user unit for {instance}: {dest:?}");
        println!("Tip: enable lingering with `sudo loginctl enable-linger $USER`");
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        let task_name = windows_task_name(&instance);
        let task_run = format!(
            "cmd /C set \"SPOTUIFY_INSTANCE={}\" && \"{}\" daemon start --foreground",
            instance,
            exe.display()
        );
        let status = std::process::Command::new("schtasks")
            .args([
                "/Create", "/TN", &task_name, "/SC", "ONLOGON", "/TR", &task_run, "/F",
            ])
            .status()?;
        if !status.success() {
            anyhow::bail!("`schtasks /Create` failed (status {status})");
        }
        println!("Installed Windows Task Scheduler entry for {instance}: {task_name}");
        return Ok(());
    }

    #[allow(unreachable_code)]
    {
        anyhow::bail!("daemon install-service is not implemented on this platform")
    }
}

fn uninstall_platform_service() -> Result<()> {
    let instance = spotuify_protocol::paths::app_instance_name();

    #[cfg(target_os = "macos")]
    {
        let label = launchd_label(&instance);
        let uid = std::process::Command::new("id").arg("-u").output()?;
        let uid = String::from_utf8_lossy(&uid.stdout).trim().to_string();
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", &format!("gui/{uid}/{label}")])
            .status();
        let path = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("no home dir"))?
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist"));
        let _ = std::fs::remove_file(&path);
        println!("Removed launchd agent for {instance}: {path:?}");
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        let unit_name = systemd_unit_name(&instance);
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "--now", &unit_name])
            .status();
        let path = dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("no config dir"))?
            .join("systemd/user")
            .join(format!("{unit_name}.service"));
        let _ = std::fs::remove_file(&path);
        println!("Removed systemd user unit for {instance}: {path:?}");
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        let task_name = windows_task_name(&instance);
        let _ = std::process::Command::new("schtasks")
            .args(["/Delete", "/TN", &task_name, "/F"])
            .status();
        println!("Removed Windows Task Scheduler entry for {instance}: {task_name}");
        return Ok(());
    }
    #[allow(unreachable_code)]
    {
        anyhow::bail!("daemon uninstall-service is not implemented on this platform")
    }
}

#[cfg(target_os = "macos")]
fn launchd_label(instance: &str) -> String {
    if instance == "spotuify" {
        "com.planetaryescape.spotuify.daemon".to_string()
    } else {
        format!("com.planetaryescape.{instance}.daemon")
    }
}

#[cfg(target_os = "macos")]
fn launchd_plist(label: &str, exe: &Path, instance: &str) -> String {
    let stdout = format!("/tmp/{instance}-daemon.log");
    let stderr = format!("/tmp/{instance}-daemon.err");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>daemon</string>
        <string>start</string>
        <string>--foreground</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{stdout}</string>
    <key>StandardErrorPath</key>
    <string>{stderr}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>SPOTUIFY_INSTANCE</key>
        <string>{instance}</string>
        <key>SPOTUIFY_LOG</key>
        <string>spotuify=info</string>{client_id_block}
    </dict>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#,
        label = xml_escape(label),
        exe = xml_escape(&exe.display().to_string()),
        stdout = xml_escape(&stdout),
        stderr = xml_escape(&stderr),
        instance = xml_escape(instance),
        client_id_block = opt_out_client_id().map_or_else(String::new, |id| format!(
            "\n        <key>SPOTUIFY_CLIENT_ID</key>\n        <string>{}</string>",
            xml_escape(&id)
        )),
    )
}

/// The `SPOTUIFY_CLIENT_ID` override, if the user has set it. Captured into
/// the installed service definition so a service-managed daemon (which
/// does not inherit an interactive shell's env) uses the same dev-app
/// credentials as the interactive login.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn opt_out_client_id() -> Option<String> {
    std::env::var("SPOTUIFY_CLIENT_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

#[cfg(target_os = "macos")]
fn remove_legacy_dev_launchd_agent(agents: &Path) -> Result<()> {
    let uid = std::process::Command::new("id").arg("-u").output()?;
    let uid = String::from_utf8_lossy(&uid.stdout).trim().to_string();
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &format!("gui/{uid}/dev.spotuify.daemon")])
        .status();
    let _ = std::fs::remove_file(agents.join("dev.spotuify.daemon.plist"));
    Ok(())
}

#[cfg(target_os = "macos")]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(target_os = "linux")]
fn systemd_unit_name(instance: &str) -> String {
    if instance == "spotuify" {
        "spotuify-daemon".to_string()
    } else {
        format!("{instance}-daemon")
    }
}

#[cfg(target_os = "linux")]
fn systemd_unit(exe: &Path, instance: &str) -> String {
    format!(
        "[Unit]\n\
         Description=spotuify daemon ({instance})\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={} daemon start --foreground\n\
         Restart=on-failure\n\
         RestartSec=5s\n\
         Environment=SPOTUIFY_INSTANCE={instance}\n\
         Environment=SPOTUIFY_LOG=spotuify=info\n\
         {client_id_line}\
         PrivateTmp=false\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        systemd_quote(&exe.display().to_string()),
        client_id_line = opt_out_client_id().map_or_else(String::new, |id| format!(
            "Environment=SPOTUIFY_CLIENT_ID={id}\n"
        )),
    )
}

#[cfg(target_os = "linux")]
fn systemd_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(target_os = "windows")]
fn windows_task_name(instance: &str) -> String {
    if instance == "spotuify" {
        "spotuify-daemon".to_string()
    } else {
        format!("{instance}-daemon")
    }
}

async fn onboard(requested_provider: Option<String>) -> Result<()> {
    println!("spotuify setup\n");
    println!("Config: {}\n", init_config()?.display());
    let status = auth_status_for_login(requested_provider.as_deref()).await?;
    let auth_method = preferred_auth_method(&status);
    let provider_id = status.provider;

    if auth_method == AuthMethodData::FirstParty {
        commands::ipc_login(Some(provider_id.to_string()), None).await?;
        println!("\nSetup complete. Run `spotuify` to open the player.");
        return Ok(());
    }

    // Dev-app onboarding: read the partial config template directly so
    // blank first-run credentials become prompts, not load errors.
    println!("Using BYO Spotify app OAuth.");
    let state = dev_app_onboarding_state(&provider_id)?;
    let needs_credentials = dev_app_onboarding_needs_credentials(&state);
    if needs_credentials {
        println!("Spotify Dashboard steps:");
        println!("1. Open https://developer.spotify.com/dashboard");
        println!("2. Create an app named spotuify");
        println!("3. Add this Redirect URI exactly: http://127.0.0.1:8888/callback");
        println!("4. Save settings, then copy Client ID from Basic Information\n");
        let _ = open::that_detached("https://developer.spotify.com/dashboard");
        wait_for_enter(
            "Press Enter when the Spotify app is created and the Redirect URI is saved...",
        )?;
    } else {
        println!("Using saved Spotify app client ID.");
    }

    if needs_credentials {
        let client_id = prompt_required_default("Client ID", state.client_id.as_deref())?;
        set_config_value(
            &provider_config_path(&provider_id, "client_id")?,
            &client_id,
        )?;

        let redirect_uri = prompt_default("Redirect URI", &state.redirect_uri)?;
        set_config_value(
            &provider_config_path(&provider_id, "redirect_uri")?,
            &redirect_uri,
        )?;
    }

    println!("\nCredentials saved. Starting Spotify OAuth...");
    commands::ipc_login(Some(provider_id.to_string()), None).await?;

    println!("\nOAuth complete. Syncing Spotify data...");
    initial_sync().await?;
    println!("\nSetup complete.");
    Ok(())
}

async fn auth_status_for_login(requested: Option<&str>) -> Result<AuthStatusData> {
    discover_auth_status(requested)
        .await?
        .context("no configured provider requires authentication")
}

async fn discover_auth_status(requested: Option<&str>) -> Result<Option<AuthStatusData>> {
    let primary = commands::ipc_auth_status(requested.map(str::to_owned)).await?;
    if requested.is_some() || primary.strategy != AuthStrategyData::None {
        return select_auth_status(requested.is_some(), primary, Vec::new());
    }

    let provider_ids = match commands::ipc_provider_catalog().await {
        Ok(catalog) => catalog
            .providers
            .into_iter()
            .map(|descriptor| descriptor.id)
            .collect::<Vec<_>>(),
        Err(catalog_error) => {
            // Phase-7 daemons expose AuthStatus but predate ProvidersList.
            // Local config supplies only neutral registry identities; the old
            // daemon remains authoritative for each identity's auth strategy.
            let loaded = spotuify_config::load().with_context(|| {
                format!(
                    "provider catalog unavailable ({catalog_error}); failed to load provider identities"
                )
            })?;
            let provider_ids = loaded
                .config
                .providers
                .iter()
                .map(|provider| provider.id.clone())
                .collect::<Vec<_>>();
            if provider_ids.is_empty() {
                return Err(catalog_error.context(
                    "provider catalog unavailable and local config has no provider identities",
                ));
            }
            provider_ids
        }
    };
    let mut alternatives = Vec::new();
    for provider_id in provider_ids
        .into_iter()
        .filter(|provider_id| provider_id != &primary.provider)
    {
        alternatives.push(
            commands::ipc_auth_status(Some(provider_id.to_string()))
                .await
                .with_context(|| format!("failed to query auth status for `{provider_id}`"))?,
        );
    }
    select_auth_status(false, primary, alternatives)
}

fn select_auth_status(
    explicit: bool,
    primary: AuthStatusData,
    alternatives: Vec<AuthStatusData>,
) -> Result<Option<AuthStatusData>> {
    if primary.strategy != AuthStrategyData::None {
        return Ok(Some(primary));
    }
    if explicit {
        anyhow::bail!(
            "provider `{}` does not require authentication",
            primary.provider
        );
    }

    let mut requiring_auth = alternatives
        .into_iter()
        .filter(|status| status.strategy != AuthStrategyData::None);
    let selected = requiring_auth.next();
    anyhow::ensure!(
        requiring_auth.next().is_none(),
        "multiple providers require authentication; pass --provider <id>"
    );
    Ok(selected)
}

fn preferred_auth_method(status: &AuthStatusData) -> AuthMethodData {
    status.preferred_method.unwrap_or_else(|| {
        let dev_app = status
            .credentials
            .iter()
            .any(|credential| credential.kind == AuthCredentialKind::DevApp && credential.present);
        let first_party = status.credentials.iter().any(|credential| {
            credential.kind == AuthCredentialKind::FirstParty && credential.present
        });
        if first_party && !dev_app {
            AuthMethodData::FirstParty
        } else {
            AuthMethodData::DevApp
        }
    })
}

fn provider_config_path(provider_id: &ProviderId, field: &str) -> Result<ConfigPath> {
    ConfigPath::parse(&format!("providers.{provider_id}.{field}")).map_err(anyhow::Error::from)
}

struct DevAppOnboardingState {
    client_id: Option<String>,
    redirect_uri: String,
}

fn dev_app_onboarding_state(provider_id: &ProviderId) -> Result<DevAppOnboardingState> {
    let configured_client_id =
        get_effective_config_value(&provider_config_path(provider_id, "client_id")?)?
            .map(|value| value.expose().to_string())
            .filter(|value| !value.trim().is_empty());
    let client_id = std::env::var("SPOTUIFY_CLIENT_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or(configured_client_id);
    let redirect_uri =
        get_effective_config_value(&provider_config_path(provider_id, "redirect_uri")?)?
            .map(|value| value.expose().to_string())
            .unwrap_or_else(|| "http://127.0.0.1:8888/callback".to_string());

    Ok(DevAppOnboardingState {
        client_id,
        redirect_uri,
    })
}

fn dev_app_onboarding_needs_credentials(state: &DevAppOnboardingState) -> bool {
    state.client_id.is_none()
}

async fn needs_onboarding() -> Result<bool> {
    let status = match discover_auth_status(None).await {
        Ok(status) => status,
        Err(err) => {
            eprintln!("warning: daemon auth status unavailable: {err}");
            None
        }
    };
    needs_onboarding_with_status(status.as_ref())
}

fn needs_onboarding_with_status(status: Option<&AuthStatusData>) -> Result<bool> {
    let Some(status) = status else {
        // A transient daemon problem must not force the interactive setup UI.
        return Ok(false);
    };
    if status.strategy == AuthStrategyData::None {
        return Ok(false);
    }
    let provider_id = status.provider.clone();
    if preferred_auth_method(status) == AuthMethodData::FirstParty {
        let creds_present = status.credentials.iter().any(|credential| {
            credential.kind == AuthCredentialKind::FirstParty && credential.present
        });
        return Ok(!creds_present);
    }

    // Default dev-app PKCE: needs a client_id and a stored token.
    let path = config_path()?;
    let client_id_present = std::env::var("SPOTUIFY_CLIENT_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .is_some()
        || (path.exists()
            && get_effective_config_value(&provider_config_path(&provider_id, "client_id")?)?
                .is_some_and(|value| !value.expose().trim().is_empty()));
    let token_present = status
        .credentials
        .iter()
        .any(|credential| credential.kind == AuthCredentialKind::DevApp && credential.present);
    Ok(!client_id_present || !token_present)
}

fn hook_test_uri() -> Result<String> {
    ResourceUri::new(
        UriScheme::new(env!("CARGO_PKG_NAME"))?,
        MediaKind::Track,
        "spotuify-hook-test",
    )
    .map(|uri| uri.as_uri())
    .context("failed to build hook test resource URI")
}

async fn initial_sync() -> Result<()> {
    use spotuify_protocol::{IpcClient, OperationSource, Request, Response, ResponseData};

    daemon::server::ensure_daemon_running().await?;
    let mut client = IpcClient::connect_with_source(OperationSource::Cli).await?;

    match client.request(Request::PlaybackGet).await {
        Ok(Response::Ok {
            data: ResponseData::Playback { playback },
        }) => {
            let now_playing = playback
                .item
                .as_ref()
                .map_or("nothing playing", |item| item.name.as_str());
            println!("playback: {now_playing}");
        }
        Ok(Response::Error { message, .. }) => {
            println!("playback: skipped ({message})")
        }
        Err(err) => println!("playback: skipped ({err})"),
        Ok(Response::Ok { .. }) => println!("playback: skipped (unexpected daemon response)"),
    }
    match client.request(Request::DevicesList).await {
        Ok(Response::Ok {
            data: ResponseData::Devices { devices },
        }) => println!("devices: {}", devices.len()),
        Ok(Response::Error { message, .. }) => {
            println!("devices: skipped ({message})")
        }
        Err(err) => println!("devices: skipped ({err})"),
        Ok(Response::Ok { .. }) => println!("devices: skipped (unexpected daemon response)"),
    }
    match client.request(Request::QueueGet).await {
        Ok(Response::Ok {
            data: ResponseData::Queue { queue },
        }) => println!("queue: {} upcoming", queue.items.len()),
        Ok(Response::Error { message, .. }) => {
            println!("queue: skipped ({message})")
        }
        Err(err) => println!("queue: skipped ({err})"),
        Ok(Response::Ok { .. }) => println!("queue: skipped (unexpected daemon response)"),
    }
    match client
        .request(Request::PlaylistsList { provider: None })
        .await
    {
        Ok(Response::Ok {
            data: ResponseData::Playlists { playlists },
        }) => println!("playlists: {}", playlists.len()),
        Ok(Response::Error { message, .. }) => {
            println!("playlists: skipped ({message})")
        }
        Err(err) => println!("playlists: skipped ({err})"),
        Ok(Response::Ok { .. }) => println!("playlists: skipped (unexpected daemon response)"),
    }
    Ok(())
}

fn prompt_required_default(label: &str, default: Option<&str>) -> Result<String> {
    loop {
        let value = if let Some(default) = default {
            prompt_default(label, default)?
        } else {
            prompt(label)?
        };
        if !value.trim().is_empty() {
            return Ok(value.trim().to_string());
        }
        println!("{label} is required.");
    }
}

fn prompt_default(label: &str, default: &str) -> Result<String> {
    print!("{label} [{default}]: ");
    io::stdout().flush()?;
    let mut value = String::new();
    if io::stdin().read_line(&mut value)? == 0 {
        anyhow::bail!("input closed while reading {label}");
    }
    let value = value.trim();
    if value.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(value.to_string())
    }
}

fn prompt(label: &str) -> Result<String> {
    print!("{label}: ");
    io::stdout().flush()?;
    let mut value = String::new();
    if io::stdin().read_line(&mut value)? == 0 {
        anyhow::bail!("input closed while reading {label}");
    }
    Ok(value)
}

fn wait_for_enter(message: &str) -> Result<()> {
    print!("{message}");
    io::stdout().flush()?;
    let mut value = String::new();
    if io::stdin().read_line(&mut value)? == 0 {
        anyhow::bail!("input closed while waiting for setup confirmation");
    }
    Ok(())
}

fn handle_logs(command: LogsCommand) -> Result<()> {
    match command {
        LogsCommand::Path => println!("{}", logging::active_log_path()?.display()),
        LogsCommand::Tail {
            lines,
            follow,
            format,
        } => {
            let json_mode = format == "json" || format == "jsonl";
            let logs = logging::read_tail(lines)?;
            if logs.is_empty() && !follow {
                if json_mode {
                    println!(
                        "{}",
                        serde_json::json!({
                            "warning": "no log file",
                            "path": logging::active_log_path()?.display().to_string()
                        })
                    );
                } else {
                    println!("no logs yet: {}", logging::active_log_path()?.display());
                }
            } else {
                for line in logs.lines() {
                    emit_log_line(line, json_mode);
                }
            }
            if follow {
                // Phase 13 (P13-C) — file-polling tail loop borrowed
                // from mxr (crates/daemon/src/commands/logs.rs:48-142).
                // Polls the log file every 500ms; exits on Ctrl-C.
                follow_log_file(json_mode)?;
            }
        }
    }
    Ok(())
}

fn emit_log_line(line: &str, json_mode: bool) {
    if json_mode {
        // Pass-through: file lines are already JSON when daemon ran in
        // JSON mode. If a line isn't valid JSON, wrap it for the
        // consumer.
        if serde_json::from_str::<serde_json::Value>(line).is_ok() {
            println!("{line}");
        } else {
            println!("{}", serde_json::json!({ "raw": line }));
        }
    } else {
        println!("{line}");
    }
}

fn follow_log_file(json_mode: bool) -> Result<()> {
    use std::io::{BufRead, BufReader, Seek, SeekFrom};
    let path = logging::active_log_path()?;
    let mut pos = std::fs::metadata(&path).map_or(0, |m| m.len());
    if !json_mode {
        println!("--- Following {} (Ctrl-C to stop) ---", path.display());
    }
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let current_len = match std::fs::metadata(&path) {
            Ok(m) => m.len(),
            Err(_) => continue,
        };
        if current_len > pos {
            let mut file = std::fs::File::open(&path)?;
            file.seek(SeekFrom::Start(pos))?;
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let line = line?;
                emit_log_line(&line, json_mode);
            }
            pos = current_len;
        } else if current_len < pos {
            // Log rotation truncated the file; rewind.
            pos = 0;
        }
    }
}

fn handle_config(command: ConfigCommand) -> Result<()> {
    match command {
        ConfigCommand::Path => println!("{}", config_path()?.display()),
        ConfigCommand::Init => println!("{}", init_config()?.display()),
        ConfigCommand::Get {
            key,
            raw,
            reveal_secret,
        } => {
            let key = ConfigPath::parse(&key)?;
            let value = if raw {
                get_config_value(&key)?
            } else {
                get_effective_config_value(&key)?
            };
            if let Some(value) = value {
                println!(
                    "{}",
                    if reveal_secret {
                        value.expose()
                    } else {
                        value.redacted()
                    }
                );
            }
        }
        ConfigCommand::Set { key, value } => {
            let key = ConfigPath::parse(&key)?;
            set_config_value(&key, &value)?;
            println!("updated {}", key.canonical());
        }
        ConfigCommand::Show {
            raw,
            reveal_secret,
            format,
        } => {
            // Build an ordered key -> value map over every settable key. The
            // macOS Settings window reads this (json) to populate its form.
            let mut entries: Vec<(String, String)> = Vec::new();
            for key_str in EDITABLE_CONFIG_PATHS {
                let key = ConfigPath::parse(key_str)?;
                let value = if raw {
                    get_config_value(&key)?
                } else {
                    get_effective_config_value(&key)?
                };
                let value = value.map_or_else(String::new, |value| {
                    if reveal_secret {
                        value.expose().to_string()
                    } else {
                        value.redacted().to_string()
                    }
                });
                entries.push(((*key_str).to_string(), value));
            }
            output::print_config_values(&entries, format)?;
        }
    }
    Ok(())
}

async fn handle_analytics(command: AnalyticsCommand) -> Result<()> {
    match command {
        AnalyticsCommand::Events { limit, format } => {
            let store = AnalyticsStore::open_default().await?;
            let events = store.recent_events(limit).await?;
            output::print_analytics_events(&events, format)
        }
        AnalyticsCommand::Top {
            kind,
            since,
            limit,
            format,
        } => {
            let request = spotuify_protocol::Request::AnalyticsTop {
                kind: parse_top_kind(&kind)?,
                since_window: parse_since_window(&since)?,
                limit,
            };
            send_and_render(request, format).await
        }
        AnalyticsCommand::Habits { window, format } => {
            let request = spotuify_protocol::Request::AnalyticsHabits {
                window: parse_habit_window(&window)?,
                since_ms: None,
            };
            send_and_render(request, format).await
        }
        AnalyticsCommand::Search {
            mode,
            limit,
            format,
        } => {
            let request = spotuify_protocol::Request::AnalyticsSearch {
                mode: parse_search_mode(&mode)?,
                limit,
            };
            send_and_render(request, format).await
        }
        AnalyticsCommand::Rediscovery { gap, format } => {
            let request = spotuify_protocol::Request::AnalyticsRediscovery {
                gap_days: parse_gap_days(&gap)?,
            };
            send_and_render(request, format).await
        }
        AnalyticsCommand::Rebuild { since, format } => {
            let request = spotuify_protocol::Request::AnalyticsRebuild {
                since_ms: since.as_deref().and_then(parse_iso_or_relative),
            };
            send_and_render(request, format).await
        }
        AnalyticsCommand::Prune { apply, format } => {
            let request = spotuify_protocol::Request::AnalyticsPrune { apply };
            send_and_render(request, format).await
        }
        AnalyticsCommand::Export {
            target,
            since,
            format,
        } => {
            let request = spotuify_protocol::Request::AnalyticsExport {
                target: parse_export_target(&target)?,
                since_ms: since.as_deref().and_then(parse_iso_or_relative),
            };
            send_and_render(request, format).await
        }
        AnalyticsCommand::Import {
            target,
            command,
            format,
        } => handle_analytics_import(target, command, format).await,
    }
}

async fn handle_analytics_import(
    target: Option<String>,
    command: Option<AnalyticsImportCommand>,
    format: OutputFormat,
) -> Result<()> {
    match command {
        Some(AnalyticsImportCommand::Lastfm {
            user,
            api_key,
            from,
            to,
            apply,
            format,
        }) => {
            let request = spotuify_protocol::Request::AnalyticsImport {
                target: spotuify_protocol::ExportTarget::LastFm,
                username: user,
                api_key,
                from_ms: from.as_deref().and_then(parse_iso_or_relative),
                to_ms: to.as_deref().and_then(parse_iso_or_relative),
                apply,
            };
            send_and_render(request, format).await
        }
        Some(AnalyticsImportCommand::Status { run_id, format }) => {
            send_and_render(
                spotuify_protocol::Request::AnalyticsImportStatus { run_id },
                format,
            )
            .await
        }
        Some(AnalyticsImportCommand::Unresolved { run_id, format }) => {
            send_and_render(
                spotuify_protocol::Request::AnalyticsImportUnresolved { run_id },
                format,
            )
            .await
        }
        Some(AnalyticsImportCommand::Undo {
            run_id,
            dry_run,
            yes,
            format,
        }) => {
            send_and_render(
                spotuify_protocol::Request::AnalyticsImportUndo {
                    run_id,
                    dry_run,
                    force: yes,
                },
                format,
            )
            .await
        }
        None => {
            let target = target.context(
                "missing import subcommand; use `analytics import lastfm` or compatibility `analytics import --target lastfm`",
            )?;
            let request = spotuify_protocol::Request::AnalyticsImport {
                target: parse_export_target(&target)?,
                username: None,
                api_key: None,
                from_ms: None,
                to_ms: None,
                apply: false,
            };
            send_and_render(request, format).await
        }
    }
}

async fn handle_ops(command: OpsCommand) -> Result<()> {
    match command {
        OpsCommand::Log {
            limit,
            since,
            source,
            format,
        } => {
            let request = spotuify_protocol::Request::OpsLog {
                limit,
                since_ms: since.as_deref().and_then(parse_iso_or_relative),
                source: source
                    .as_deref()
                    .and_then(spotuify_protocol::OperationSource::from_label),
            };
            send_and_render(request, format).await
        }
        OpsCommand::Show { id, diff, format } => {
            let operation_id: spotuify_protocol::OperationId = id
                .parse()
                .context("invalid operation id; expected uuid v7")?;
            let request = spotuify_protocol::Request::OpsShow {
                operation_id,
                with_diff: diff,
            };
            send_and_render(request, format).await
        }
        OpsCommand::Undo {
            id,
            dry_run,
            yes,
            force,
            since,
            format,
        } => {
            ensure_ops_undo_allowed(dry_run, yes)?;
            let operation_id = match id {
                Some(raw) => Some(
                    raw.parse::<spotuify_protocol::OperationId>()
                        .context("invalid operation id; expected uuid v7")?,
                ),
                None => None,
            };
            let request = spotuify_protocol::Request::OpsUndo {
                operation_id,
                dry_run,
                force,
                bulk_since_ms: since.as_deref().and_then(parse_iso_or_relative),
            };
            send_and_render(request, format).await
        }
        OpsCommand::Redo { id, format } => {
            let operation_id = match id {
                Some(raw) => Some(
                    raw.parse::<spotuify_protocol::OperationId>()
                        .context("invalid operation id; expected uuid v7")?,
                ),
                None => None,
            };
            let request = spotuify_protocol::Request::OpsRedo { operation_id };
            send_and_render(request, format).await
        }
    }
}

fn ensure_ops_undo_allowed(dry_run: bool, yes: bool) -> Result<()> {
    if dry_run || yes {
        return Ok(());
    }
    anyhow::bail!("ops undo requires --dry-run for preview or --yes to execute")
}

/// Phase 13 (P13-J) — clap-built-in completions + man-page generation.
fn handle_generate(command: GenerateCommand) -> Result<()> {
    use clap::CommandFactory;
    match command {
        GenerateCommand::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "spotuify", &mut std::io::stdout());
        }
        GenerateCommand::ManPage => {
            let cmd = Cli::command();
            let man = clap_mangen::Man::new(cmd);
            man.render(&mut std::io::stdout())
                .context("failed to render man page")?;
        }
    }
    Ok(())
}

async fn handle_hooks(command: HooksCommand) -> Result<()> {
    match command {
        HooksCommand::Test { format } => {
            let config = spotuify_config::load()
                .context("failed to load config")?
                .config;
            let hook_command = config
                .analytics
                .hook_command
                .clone()
                .context("no hook configured; set analytics.hook_command")?;
            let timeout_ms = config.analytics.hook_timeout_ms;
            let uri = hook_test_uri()?;
            let dispatcher = spotuify_system::HookDispatcher::new(spotuify_system::HookConfig {
                hook_command: hook_command.clone(),
                timeout_ms,
            });
            dispatcher
                .fire_checked(spotuify_system::HookEvent::ListenQualified {
                    uri,
                    duration_ms: 180_000,
                })
                .await
                .context("hook test failed")?;
            match format {
                OutputFormat::Json | OutputFormat::Jsonl => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "ok": true,
                            "action": "hooks-test",
                            "hook_command": hook_command,
                            "timeout_ms": timeout_ms
                        })
                    );
                }
                OutputFormat::Csv => {
                    println!("ok,action,hook_command,timeout_ms");
                    println!("true,hooks-test,{},{}", csv_cell(&hook_command), timeout_ms);
                }
                OutputFormat::Ids => println!("{hook_command}"),
                OutputFormat::Table => {
                    println!("hook test ok: {hook_command} ({timeout_ms}ms)");
                }
            }
        }
    }
    Ok(())
}

fn csv_cell(value: &str) -> String {
    if value.contains([',', '"', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// Phase 13 (P13-D) — assemble a redacted diagnostic tarball.
/// Includes: doctor JSON, redacted config, last N log lines, last 50
/// operations, version+platform. Never auto-uploads.
async fn bug_report(log_lines: usize, output: Option<PathBuf>) -> Result<()> {
    use std::io::Write;
    let target = output.unwrap_or_else(|| {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        PathBuf::from(format!("./spotuify-bug-report-{ts}.tar"))
    });

    let mut sections: Vec<(String, String)> = Vec::new();

    // 1. version + platform
    sections.push((
        "version.txt".to_string(),
        format!(
            "spotuify {} ({} {})\n",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
        ),
    ));

    // 2. doctor report (via daemon if reachable, else best-effort local probe).
    let doctor_json = match daemon::server::ensure_daemon_running().await {
        Ok(()) => {
            let mut client = spotuify_protocol::IpcClient::connect().await?;
            match client
                .request(spotuify_protocol::Request::GetDoctorReport)
                .await?
            {
                spotuify_protocol::Response::Ok {
                    data: spotuify_protocol::ResponseData::DoctorReport { report },
                } => serde_json::to_string_pretty(&report)?,
                _ => "daemon returned unexpected doctor response".to_string(),
            }
        }
        Err(err) => format!("doctor unavailable: {err}"),
    };
    sections.push(("doctor.json".to_string(), doctor_json));

    // 3. last 50 ops (best-effort).
    if let Ok(mut client) = spotuify_protocol::IpcClient::connect().await {
        if let Ok(spotuify_protocol::Response::Ok {
            data: spotuify_protocol::ResponseData::Operations { ops },
        }) = client
            .request(spotuify_protocol::Request::OpsLog {
                limit: 50,
                since_ms: None,
                source: None,
            })
            .await
        {
            sections.push((
                "operations.jsonl".to_string(),
                ops.iter()
                    .map(|op| serde_json::to_string(op).unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ));
        }
    }

    // 4. last-N log lines.
    if let Ok(tail) = logging::read_tail(log_lines) {
        sections.push(("spotuify.log".to_string(), tail));
    }

    // 5. redacted config.
    if let Ok(path) = config_path() {
        if let Ok(raw) = std::fs::read_to_string(&path) {
            sections.push(("config.redacted.toml".to_string(), redact_config(&raw)));
        }
    }

    // Plain tar keeps the dep surface tiny and makes manual inspection
    // possible with the system `tar` command.
    let tar_path = target;
    let file = std::fs::File::create(&tar_path)
        .with_context(|| format!("failed to create {}", tar_path.display()))?;
    let mut buf = std::io::BufWriter::new(file);
    for (name, body) in &sections {
        write_tar_entry(&mut buf, name, body.as_bytes())?;
    }
    write_tar_terminator(&mut buf)?;
    buf.flush()?;

    println!(
        "Wrote bug report ({} sections) to {}",
        sections.len(),
        tar_path.display()
    );
    println!("Manual review recommended: inspect the tarball before sharing.");
    Ok(())
}

/// Redact obvious-looking secrets from a TOML config string. Keeps
/// behaviour observable to bug-report reviewers without leaking
/// credentials. Matches client_secret / token / refresh_token /
/// password / api_key plus email-looking strings.
fn redact_config(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for line in raw.lines() {
        let lower = line.to_ascii_lowercase();
        let secret_field = [
            "client_secret",
            "token",
            "refresh_token",
            "password",
            "api_key",
        ]
        .iter()
        .any(|needle| lower.contains(needle));
        if secret_field && line.contains('=') {
            if let Some((key, _)) = line.split_once('=') {
                out.push_str(key.trim_end());
                out.push_str(" = \"<redacted>\"\n");
                continue;
            }
        }
        // Naive email scrub.
        let cleaned = line
            .split_whitespace()
            .map(|token| {
                if token.contains('@') && token.contains('.') {
                    "<redacted-email>".to_string()
                } else {
                    token.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        out.push_str(&cleaned);
        out.push('\n');
    }
    out
}

/// Minimal POSIX tar (ustar) entry writer — keeps the dep surface
/// small. Each `body` is treated as a regular file at the given
/// `name`. Padding to 512-byte block boundary is enforced.
fn write_tar_entry(buf: &mut impl std::io::Write, name: &str, body: &[u8]) -> Result<()> {
    let mut header = [0u8; 512];
    // Filename
    let name_bytes = name.as_bytes();
    let len = name_bytes.len().min(100);
    header[..len].copy_from_slice(&name_bytes[..len]);
    // Mode: 0644 (rust-style "100644")
    header[100..107].copy_from_slice(b"0000644");
    // owner uid / gid: 0
    header[108..115].copy_from_slice(b"0000000");
    header[116..123].copy_from_slice(b"0000000");
    // size in octal, 12 bytes incl. trailing null
    let size_oct = format!("{:011o}", body.len());
    header[124..124 + 11].copy_from_slice(size_oct.as_bytes());
    // mtime in octal, 12 bytes
    let mtime = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mtime_oct = format!("{mtime:011o}");
    header[136..136 + 11].copy_from_slice(mtime_oct.as_bytes());
    // Pre-checksum field: 8 spaces, per ustar.
    header[148..156].copy_from_slice(b"        ");
    // typeflag '0' = normal file
    header[156] = b'0';
    // ustar magic + version.
    header[257..262].copy_from_slice(b"ustar");
    header[263..265].copy_from_slice(b"00");
    // Compute checksum.
    let chksum: u32 = header.iter().map(|b| *b as u32).sum();
    let chk_oct = format!("{chksum:06o}\0 ");
    header[148..148 + 8].copy_from_slice(chk_oct.as_bytes());

    buf.write_all(&header)?;
    buf.write_all(body)?;
    // Pad body to 512-byte boundary.
    let pad = (512 - (body.len() % 512)) % 512;
    if pad > 0 {
        buf.write_all(&vec![0u8; pad])?;
    }
    Ok(())
}

fn write_tar_terminator(buf: &mut impl std::io::Write) -> Result<()> {
    // Two 512-byte zero blocks signal end-of-archive.
    buf.write_all(&[0u8; 1024])?;
    Ok(())
}

/// Shared dispatch: connect to the daemon, send a Request, render the
/// resulting ResponseData via the existing output module. Used by the
/// Phase 10 analytics and Phase 12 ops subcommands.
async fn send_and_render(request: spotuify_protocol::Request, format: OutputFormat) -> Result<()> {
    use spotuify_protocol::{IpcClient, OperationSource};
    daemon::server::ensure_daemon_running().await?;
    let mut client = IpcClient::connect_with_source(OperationSource::Cli).await?;
    let response = client.request(request).await?;
    match response {
        spotuify_protocol::Response::Ok { data } => output::print_response_data(&data, format),
        spotuify_protocol::Response::Error { message, .. } => {
            anyhow::bail!("daemon error: {message}")
        }
    }
}

fn parse_top_kind(raw: &str) -> Result<spotuify_protocol::TopKind> {
    use spotuify_protocol::TopKind as K;
    match raw {
        "tracks" => Ok(K::Tracks),
        "artists" => Ok(K::Artists),
        "albums" => Ok(K::Albums),
        "playlists" => Ok(K::Playlists),
        other => {
            anyhow::bail!("invalid --kind `{other}`; expected tracks|artists|albums|playlists")
        }
    }
}

fn parse_habit_window(raw: &str) -> Result<spotuify_protocol::HabitWindow> {
    use spotuify_protocol::HabitWindow as W;
    match raw {
        "day" => Ok(W::Day),
        "week" => Ok(W::Week),
        "month" => Ok(W::Month),
        other => anyhow::bail!("invalid --window `{other}`; expected day|week|month"),
    }
}

fn parse_search_mode(raw: &str) -> Result<spotuify_protocol::SearchMode> {
    use spotuify_protocol::SearchMode as M;
    match raw {
        "raw" => Ok(M::Raw),
        "normalized" => Ok(M::Normalized),
        other => anyhow::bail!("invalid --mode `{other}`; expected raw|normalized"),
    }
}

fn parse_export_target(raw: &str) -> Result<spotuify_protocol::ExportTarget> {
    use spotuify_protocol::ExportTarget as T;
    match raw {
        "listenbrainz" | "listen_brainz" => Ok(T::ListenBrainz),
        "lastfm" | "last_fm" => Ok(T::LastFm),
        other => anyhow::bail!("invalid --target `{other}`; expected listenbrainz|lastfm"),
    }
}

fn parse_since_window(raw: &str) -> Result<spotuify_protocol::SinceWindow> {
    use spotuify_protocol::SinceWindow as S;
    if raw == "all" {
        return Ok(S::All);
    }
    let stripped = raw.strip_suffix('d').unwrap_or(raw);
    let days: u32 = stripped
        .parse()
        .with_context(|| format!("invalid --since `{raw}`; expected `7d`, `30d`, … or `all`"))?;
    Ok(S::Days(days))
}

fn parse_gap_days(raw: &str) -> Result<u32> {
    let stripped = raw.strip_suffix('d').unwrap_or(raw);
    stripped
        .parse()
        .with_context(|| format!("invalid --gap `{raw}`; expected `30d` / `90d` / `365d`"))
}

/// Parse `1h`, `24h`, `7d` into an absolute unix-ms timestamp.
/// Returns `None` on unparseable input (callers treat that as "no
/// filter"). Absolute ISO timestamps are accepted via the standalone
/// numeric form (callers can pre-convert).
fn parse_iso_or_relative(raw: &str) -> Option<i64> {
    if let Some(hours) = raw.strip_suffix('h').and_then(|s| s.parse::<i64>().ok()) {
        return Some(spotuify_core::now_ms().saturating_sub(hours * 3_600_000));
    }
    if let Some(days) = raw.strip_suffix('d').and_then(|s| s.parse::<i64>().ok()) {
        return Some(spotuify_core::now_ms().saturating_sub(days * 86_400_000));
    }
    // Plain unix-ms integer is the unambiguous escape hatch.
    raw.parse::<i64>().ok()
}

/// Print the daemon's current Web API bearer to stdout.
///
/// Useful for direct `api.spotify.com` probing from scripts and
/// agents in modes that mint a daemon-side bearer.
async fn auth_status(provider: Option<String>, format: OutputFormat) -> Result<()> {
    let status = commands::ipc_auth_status(provider).await?;
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&status)?);
            return Ok(());
        }
        OutputFormat::Jsonl => {
            println!("{}", serde_json::to_string(&status)?);
            return Ok(());
        }
        OutputFormat::Table | OutputFormat::Csv | OutputFormat::Ids => {}
    }

    println!("provider: {}", status.provider);
    println!("strategy: {:?}", status.strategy);
    println!("auth required: {}", status.auth_required);
    println!("auth revoked: {}", status.auth_revoked);
    for credential in status.credentials {
        println!(
            "{:?}: {}",
            credential.kind,
            if credential.present {
                "present"
            } else {
                "absent"
            }
        );
    }
    Ok(())
}

async fn auth_bearer(force: bool, format: OutputFormat, reveal_secret: bool) -> Result<()> {
    use spotuify_protocol::{IpcClient, OperationSource, Request, Response, ResponseData};
    if !reveal_secret {
        anyhow::bail!("refusing to print a live Spotify bearer token without --reveal-secret");
    }
    daemon::server::ensure_daemon_running().await?;
    let mut client = IpcClient::connect_with_source(OperationSource::Cli).await?;
    let token = match client.request(Request::WebApiToken { force }).await? {
        Response::Ok {
            data: ResponseData::WebApiToken { token: Some(t) },
        } => t,
        Response::Ok {
            data: ResponseData::WebApiToken { token: None },
        } => anyhow::bail!(
            "daemon has no Web API bearer; not logged in or token minting unavailable"
        ),
        Response::Ok { .. } => anyhow::bail!("unexpected daemon response"),
        Response::Error { message, .. } => anyhow::bail!(message),
    };
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::json!({ "token": token }));
        }
        _ => {
            println!("{token}");
        }
    }
    Ok(())
}

async fn doctor(format: OutputFormat) -> Result<()> {
    // The daemon carries live embedded-player audio-flow health on its
    // `DaemonStatus` (the proven `GetDaemonStatus` path), so the local report
    // below surfaces it via `diagnostics::build_findings` — no need for the
    // larger `GetDoctorReport` round-trip.
    let daemon = daemon::server::daemon_status().await?;
    let report = diagnostics::collect_report(daemon).await?;
    diagnostics::print_report(&report, format)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic, clippy::unwrap_used)]

    use clap::Parser;

    use super::{
        bug_report, dev_app_onboarding_needs_credentials, dev_app_onboarding_state,
        ensure_ops_undo_allowed, handle_hooks, hook_test_uri, needs_onboarding_with_status,
        preferred_auth_method, provider_config_path, reset_cache_files, select_auth_status,
        AnalyticsCommand, AuthCommand, CacheCommand, Cli, Command, DaemonCommand,
        DevAppOnboardingState, HooksCommand, LyricsCommand, MprisCommand, PlaylistCommand,
        ProvidersCommand, QueueCommand, RepeatArg, SearchKind, SearchSource, SyncTarget, ToggleArg,
        VizCommand,
    };
    use crate::output::OutputFormat;
    use spotuify_cli::cli_args::LyricsFollowFormat;
    use spotuify_core::ProviderId;
    use spotuify_protocol::{
        AuthCredentialKind, AuthCredentialStatus, AuthMethodData, AuthStatusData, AuthStrategyData,
    };

    static TEST_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn restore_env(key: &str, value: Option<std::ffi::OsString>) {
        if let Some(value) = value {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
    }

    fn auth_status(provider: &str, strategy: AuthStrategyData) -> AuthStatusData {
        AuthStatusData {
            provider: ProviderId::new(provider).unwrap(),
            strategy,
            preferred_method: None,
            auth_required: strategy != AuthStrategyData::None,
            auth_revoked: false,
            credentials: Vec::new(),
        }
    }

    #[test]
    fn onboarding_state_accepts_blank_first_run_config() {
        let _guard = TEST_ENV_LOCK.blocking_lock();
        let temp = tempfile::TempDir::new().unwrap();
        let config_path = temp.path().join("spotuify.toml");
        std::fs::write(
            &config_path,
            r#"
client_id = ""
redirect_uri = "http://127.0.0.1:8888/callback"

[player]
bitrate = 320
"#,
        )
        .unwrap();

        let old_config = std::env::var_os("SPOTUIFY_CONFIG");
        let old_client_id = std::env::var_os("SPOTUIFY_CLIENT_ID");
        let old_first_party = std::env::var_os("SPOTUIFY_USE_FIRST_PARTY");
        std::env::set_var("SPOTUIFY_CONFIG", &config_path);
        std::env::remove_var("SPOTUIFY_CLIENT_ID");
        std::env::remove_var("SPOTUIFY_USE_FIRST_PARTY");

        let provider = spotuify_core::ProviderId::new("spotify").unwrap();
        let state = dev_app_onboarding_state(&provider).unwrap();
        assert_eq!(state.client_id, None);
        assert_eq!(state.redirect_uri, "http://127.0.0.1:8888/callback");
        let mut status = auth_status("archive", AuthStrategyData::SpotifyOauth);
        status.preferred_method = Some(AuthMethodData::DevApp);
        assert!(needs_onboarding_with_status(Some(&status)).unwrap());

        restore_env("SPOTUIFY_CONFIG", old_config);
        restore_env("SPOTUIFY_CLIENT_ID", old_client_id);
        restore_env("SPOTUIFY_USE_FIRST_PARTY", old_first_party);
    }

    #[test]
    fn auth_selection_with_no_auth_default_uses_the_only_auth_capable_secondary() {
        let default = auth_status("default", AuthStrategyData::None);
        let selected = select_auth_status(
            false,
            default,
            vec![auth_status("archive", AuthStrategyData::SpotifyOauth)],
        )
        .unwrap()
        .expect("the sole auth-capable provider should be selected");

        assert_eq!(selected.provider, ProviderId::new("archive").unwrap());
    }

    #[test]
    fn auth_selection_rejects_an_explicit_no_auth_provider() {
        let explicit = select_auth_status(
            true,
            auth_status("local", AuthStrategyData::None),
            Vec::new(),
        )
        .unwrap_err();
        assert!(explicit
            .to_string()
            .contains("provider `local` does not require authentication"));
    }

    #[test]
    fn auth_selection_with_no_auth_default_rejects_ambiguous_secondaries() {
        let ambiguous = select_auth_status(
            false,
            auth_status("default", AuthStrategyData::None),
            vec![
                auth_status("archive", AuthStrategyData::SpotifyOauth),
                auth_status("radio", AuthStrategyData::SpotifyOauth),
            ],
        )
        .unwrap_err();
        assert!(ambiguous.to_string().contains("pass --provider <id>"));
    }

    #[test]
    fn legacy_auth_method_inference_prefers_first_party_only_when_unambiguous() {
        let credential = |kind| AuthCredentialStatus {
            kind,
            present: true,
            expires_at_ms: None,
            scopes: Vec::new(),
            missing_scopes: Vec::new(),
        };
        let mut status = auth_status("archive", AuthStrategyData::SpotifyOauth);
        status.credentials = vec![credential(AuthCredentialKind::FirstParty)];
        assert_eq!(preferred_auth_method(&status), AuthMethodData::FirstParty);

        status
            .credentials
            .push(credential(AuthCredentialKind::DevApp));
        assert_eq!(preferred_auth_method(&status), AuthMethodData::DevApp);

        status.preferred_method = Some(AuthMethodData::FirstParty);
        assert_eq!(preferred_auth_method(&status), AuthMethodData::FirstParty);
    }

    #[test]
    fn hook_test_uri_uses_the_application_namespace() {
        assert_eq!(
            hook_test_uri().unwrap(),
            "spotuify:track:spotuify-hook-test"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hooks_test_runs_without_a_daemon() {
        let _guard = TEST_ENV_LOCK.lock().await;
        let temp = tempfile::TempDir::new().unwrap();
        let config_path = temp.path().join("spotuify.toml");
        std::fs::write(
            &config_path,
            r#"
client_id = "public-client"

[analytics]
hook_command = "/usr/bin/true"
hook_timeout_ms = 1000
"#,
        )
        .unwrap();

        let old_no_daemon = std::env::var_os("SPOTUIFY_NO_DAEMON_START");
        let old_config = std::env::var_os("SPOTUIFY_CONFIG");
        let old_runtime = std::env::var_os("SPOTUIFY_RUNTIME_DIR");
        std::env::set_var("SPOTUIFY_NO_DAEMON_START", "1");
        std::env::set_var("SPOTUIFY_CONFIG", &config_path);
        std::env::set_var("SPOTUIFY_RUNTIME_DIR", temp.path().join("missing-runtime"));

        handle_hooks(HooksCommand::Test {
            format: OutputFormat::Json,
        })
        .await
        .expect("hook testing must not require daemon discovery");

        restore_env("SPOTUIFY_NO_DAEMON_START", old_no_daemon);
        restore_env("SPOTUIFY_CONFIG", old_config);
        restore_env("SPOTUIFY_RUNTIME_DIR", old_runtime);
    }

    #[test]
    fn dev_app_onboarding_treats_client_secret_as_optional() {
        let state = DevAppOnboardingState {
            client_id: Some("public-client".to_string()),
            redirect_uri: "http://127.0.0.1:8888/callback".to_string(),
        };

        assert!(
            !dev_app_onboarding_needs_credentials(&state),
            "PKCE onboarding should not force users to copy a client secret"
        );
    }

    #[test]
    fn status_command_accepts_machine_output_format() {
        let cli = Cli::try_parse_from(["spotuify", "status", "--format", "json"]).unwrap();

        match cli.command {
            Some(Command::Status { format }) => assert_eq!(format, OutputFormat::Json),
            _ => panic!("expected status command"),
        }
    }

    #[test]
    fn sync_search_cache_prune_accepts_older_than_and_json_output() {
        let cli = Cli::try_parse_from([
            "spotuify",
            "sync",
            "search-cache",
            "--prune",
            "--older-than",
            "7d",
            "--format",
            "json",
        ])
        .unwrap();

        match cli.command {
            Some(Command::Sync {
                target,
                prune,
                older_than,
                format,
                ..
            }) => {
                assert_eq!(target, SyncTarget::SearchCache);
                assert!(prune);
                assert_eq!(older_than.as_deref(), Some("7d"));
                assert_eq!(format, OutputFormat::Json);
            }
            _ => panic!("expected sync command"),
        }
    }

    #[test]
    fn bug_report_accepts_include_logs_alias() {
        let cli = Cli::try_parse_from([
            "spotuify",
            "bug-report",
            "--include-logs",
            "37",
            "--output",
            "report.tar",
        ])
        .unwrap();

        match cli.command {
            Some(Command::BugReport { log_lines, output }) => {
                assert_eq!(log_lines, 37);
                assert_eq!(output.as_deref(), Some(std::path::Path::new("report.tar")));
            }
            _ => panic!("expected bug-report command"),
        }
    }

    #[tokio::test]
    async fn bug_report_writes_requested_tar_and_redacts_config() {
        let _guard = TEST_ENV_LOCK.lock().await;
        let temp = tempfile::TempDir::new().unwrap();
        let config_path = temp.path().join("spotuify.toml");
        std::fs::write(
            &config_path,
            r#"
client_id = "public-client"
client_secret = "do-not-leak"
refresh_token = "refresh-secret"
support_email = "user@example.com"
"#,
        )
        .unwrap();
        let out = temp.path().join("report.bundle");

        let old_no_daemon = std::env::var_os("SPOTUIFY_NO_DAEMON_START");
        let old_config = std::env::var_os("SPOTUIFY_CONFIG");
        let old_runtime = std::env::var_os("SPOTUIFY_RUNTIME_DIR");
        std::env::set_var("SPOTUIFY_NO_DAEMON_START", "1");
        std::env::set_var("SPOTUIFY_CONFIG", &config_path);
        std::env::set_var("SPOTUIFY_RUNTIME_DIR", temp.path().join("runtime"));

        bug_report(0, Some(out.clone())).await.unwrap();

        restore_env("SPOTUIFY_NO_DAEMON_START", old_no_daemon);
        restore_env("SPOTUIFY_CONFIG", old_config);
        restore_env("SPOTUIFY_RUNTIME_DIR", old_runtime);

        let bytes = std::fs::read(&out).expect("bug report should use requested output path");
        let archive = String::from_utf8_lossy(&bytes);
        assert!(archive.contains("config.redacted.toml"));
        assert!(archive.contains("client_secret = \"<redacted>\""));
        assert!(archive.contains("refresh_token = \"<redacted>\""));
        assert!(archive.contains("<redacted-email>"));
        assert!(!archive.contains("do-not-leak"));
        assert!(!archive.contains("refresh-secret"));
        assert!(!archive.contains("user@example.com"));
    }

    #[test]
    fn ops_undo_requires_preview_or_explicit_yes() {
        let err =
            ensure_ops_undo_allowed(false, false).expect_err("plain undo should require --yes");
        assert!(err.to_string().contains("--dry-run"));
        assert!(err.to_string().contains("--yes"));

        ensure_ops_undo_allowed(true, false).expect("--dry-run should be allowed");
        ensure_ops_undo_allowed(false, true).expect("--yes should be allowed");
    }

    #[test]
    fn devices_command_accepts_machine_output_format() {
        let cli = Cli::try_parse_from(["spotuify", "devices", "--format", "json"]).unwrap();

        match cli.command {
            Some(Command::Devices { format }) => assert_eq!(format, OutputFormat::Json),
            _ => panic!("expected devices command"),
        }
    }

    #[test]
    fn providers_list_and_provider_routing_flags_parse() {
        let cli =
            Cli::try_parse_from(["spotuify", "providers", "list", "--format", "json"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Providers {
                command: ProvidersCommand::List {
                    format: OutputFormat::Json
                }
            })
        ));

        let cli = Cli::try_parse_from([
            "spotuify",
            "search",
            "needle",
            "--source",
            "remote",
            "--provider",
            "fake",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Search {
                source, provider, ..
            }) => {
                assert_eq!(source, SearchSource::Remote);
                assert_eq!(provider.as_deref(), Some("fake"));
            }
            _ => panic!("expected provider-routed search"),
        }
    }

    #[test]
    fn search_command_accepts_type_and_pipeable_output_format() {
        let cli = Cli::try_parse_from([
            "spotuify",
            "search",
            "luther vandross",
            "--type",
            "track",
            "--format",
            "ids",
        ])
        .unwrap();

        match cli.command {
            Some(Command::Search {
                query,
                kind,
                play,
                index,
                format,
                ..
            }) => {
                assert_eq!(query, "luther vandross");
                assert_eq!(kind, SearchKind::Track);
                assert!(!play);
                assert_eq!(index, 1);
                assert_eq!(format, OutputFormat::Ids);
            }
            _ => panic!("expected search command"),
        }
    }

    #[test]
    fn local_store_search_commands_parse_from_phase_three_spec() {
        let cli = Cli::try_parse_from([
            "spotuify",
            "search",
            "luther vandross",
            "--type",
            "track",
            "--source",
            "local",
            "--limit",
            "25",
            "--format",
            "jsonl",
        ])
        .unwrap();

        match cli.command {
            Some(Command::Search {
                query,
                kind,
                source,
                limit,
                format,
                ..
            }) => {
                assert_eq!(query, "luther vandross");
                assert_eq!(kind, SearchKind::Track);
                assert_eq!(source, SearchSource::Local);
                assert_eq!(limit, 25);
                assert_eq!(format, OutputFormat::Jsonl);
            }
            _ => panic!("expected search command"),
        }

        let cli = Cli::try_parse_from(["spotuify", "reindex", "--format", "json"]).unwrap();
        match cli.command {
            Some(Command::Reindex { format }) => assert_eq!(format, OutputFormat::Json),
            _ => panic!("expected reindex command"),
        }

        let cli = Cli::try_parse_from(["spotuify", "cache", "status", "--format", "json"]).unwrap();
        match cli.command {
            Some(Command::Cache {
                command: CacheCommand::Status { format },
            }) => assert_eq!(format, OutputFormat::Json),
            _ => panic!("expected cache status command"),
        }

        let cli = Cli::try_parse_from(["spotuify", "cache", "reset", "--confirm"]).unwrap();
        match cli.command {
            Some(Command::Cache {
                command: CacheCommand::Reset { confirm, .. },
            }) => assert!(confirm),
            _ => panic!("expected cache reset command"),
        }

        let cli = Cli::try_parse_from(["spotuify", "cache", "repair", "--format", "json"]).unwrap();
        match cli.command {
            Some(Command::Cache {
                command: CacheCommand::Repair { format },
            }) => assert_eq!(format, OutputFormat::Json),
            _ => panic!("expected cache repair command"),
        }
    }

    #[test]
    fn mcp_command_accepts_stdio_default_and_http_addr() {
        let cli = Cli::try_parse_from(["spotuify", "mcp"]).unwrap();
        match cli.command {
            Some(Command::Mcp { stdio, http }) => {
                assert!(!stdio);
                assert_eq!(http, None);
            }
            _ => panic!("expected mcp command"),
        }

        let cli = Cli::try_parse_from(["spotuify", "mcp", "--stdio"]).unwrap();
        match cli.command {
            Some(Command::Mcp { stdio, http }) => {
                assert!(stdio);
                assert_eq!(http, None);
            }
            _ => panic!("expected mcp stdio command"),
        }

        let cli = Cli::try_parse_from(["spotuify", "mcp", "--http", "127.0.0.1:8787"]).unwrap();
        match cli.command {
            Some(Command::Mcp { stdio, http }) => {
                assert!(!stdio);
                assert_eq!(http.as_deref(), Some("127.0.0.1:8787"));
            }
            _ => panic!("expected mcp http command"),
        }
    }

    #[test]
    fn cache_reset_removes_database_siblings_and_index_directory() {
        let temp = tempfile::TempDir::new().unwrap();
        let db_path = temp.path().join("cache.sqlite3");
        let wal_path = temp.path().join("cache.sqlite3-wal");
        let shm_path = temp.path().join("cache.sqlite3-shm");
        let index_path = temp.path().join("index");
        std::fs::write(&db_path, "db").unwrap();
        std::fs::write(&wal_path, "wal").unwrap();
        std::fs::write(&shm_path, "shm").unwrap();
        std::fs::create_dir_all(&index_path).unwrap();
        std::fs::write(index_path.join("segment"), "idx").unwrap();

        reset_cache_files(&db_path, &index_path).unwrap();

        assert!(!db_path.exists());
        assert!(!wal_path.exists());
        assert!(!shm_path.exists());
        assert!(!index_path.exists());
    }

    #[test]
    fn doctor_command_accepts_machine_output_format() {
        let cli = Cli::try_parse_from(["spotuify", "doctor", "--format", "json"]).unwrap();

        match cli.command {
            Some(Command::Doctor { format }) => assert_eq!(format, OutputFormat::Json),
            _ => panic!("expected doctor command"),
        }
    }

    #[test]
    fn daemon_commands_follow_blueprint_shape() {
        let cli = Cli::try_parse_from(["spotuify", "daemon", "start", "--foreground"]).unwrap();
        match cli.command {
            Some(Command::Daemon {
                command: DaemonCommand::Start { foreground },
            }) => assert!(foreground),
            _ => panic!("expected daemon start command"),
        }

        let cli =
            Cli::try_parse_from(["spotuify", "daemon", "status", "--format", "json"]).unwrap();
        match cli.command {
            Some(Command::Daemon {
                command: DaemonCommand::Status { format },
            }) => assert_eq!(format, OutputFormat::Json),
            _ => panic!("expected daemon status command"),
        }
    }

    #[test]
    fn queue_command_accepts_jsonl_output_format() {
        let cli = Cli::try_parse_from(["spotuify", "queue", "--format", "jsonl"]).unwrap();

        match cli.command {
            Some(Command::Queue { command, format }) => {
                assert!(command.is_none());
                assert_eq!(format, OutputFormat::Jsonl);
            }
            _ => panic!("expected queue command"),
        }
    }

    #[test]
    fn playlists_command_accepts_ids_output_format() {
        let cli = Cli::try_parse_from(["spotuify", "playlists", "--format", "ids"]).unwrap();

        match cli.command {
            Some(Command::Playlists { format, .. }) => assert_eq!(format, OutputFormat::Ids),
            _ => panic!("expected playlists command"),
        }
    }

    #[test]
    fn lyrics_commands_parse_track_fetch_and_offset() {
        let cli = Cli::try_parse_from([
            "spotuify",
            "lyrics",
            "show",
            "--track",
            "spotify:track:abc",
            "--format",
            "json",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Lyrics {
                command: LyricsCommand::Show { track, format, .. },
            }) => {
                assert_eq!(track.as_deref(), Some("spotify:track:abc"));
                assert_eq!(format, OutputFormat::Json);
            }
            _ => panic!("expected lyrics show command"),
        }

        let cli =
            Cli::try_parse_from(["spotuify", "lyrics", "fetch", "spotify:track:abc"]).unwrap();
        match cli.command {
            Some(Command::Lyrics {
                command: LyricsCommand::Fetch { track_uri, .. },
            }) => assert_eq!(track_uri, "spotify:track:abc"),
            _ => panic!("expected lyrics fetch command"),
        }

        let cli = Cli::try_parse_from([
            "spotuify", "lyrics", "follow", "--lines", "5", "--lead", "+250ms", "--format", "jsonl",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Lyrics {
                command:
                    LyricsCommand::Follow {
                        lines,
                        lead,
                        format,
                    },
            }) => {
                assert_eq!(lines, 5);
                assert_eq!(lead.as_deref(), Some("+250ms"));
                assert_eq!(format, LyricsFollowFormat::Jsonl);
            }
            _ => panic!("expected lyrics follow command"),
        }
        let err = match Cli::try_parse_from(["spotuify", "lyrics", "follow", "--format", "json"]) {
            Ok(_) => panic!("expected lyrics follow json format to be rejected"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("possible values: table, jsonl"),
            "unexpected error: {err}"
        );

        let cli = Cli::try_parse_from([
            "spotuify",
            "lyrics",
            "export",
            "spotify:track:abc",
            "--output",
            "/tmp/track.lrc",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Lyrics {
                command:
                    LyricsCommand::Export {
                        track_uri, output, ..
                    },
            }) => {
                assert_eq!(track_uri, "spotify:track:abc");
                assert_eq!(
                    output.as_deref(),
                    Some(std::path::Path::new("/tmp/track.lrc"))
                );
            }
            _ => panic!("expected lyrics export command"),
        }

        let cli =
            Cli::try_parse_from(["spotuify", "lyrics", "offset", "spotify:track:abc", "+50ms"])
                .unwrap();
        match cli.command {
            Some(Command::Lyrics {
                command: LyricsCommand::Offset { offset, .. },
            }) => assert_eq!(offset, "+50ms"),
            _ => panic!("expected lyrics offset command"),
        }
    }

    #[test]
    fn refresh_media_command_parses_output_format() {
        let cli = Cli::try_parse_from(["spotuify", "refresh-media", "--format", "json"]).unwrap();

        match cli.command {
            Some(Command::RefreshMedia { format }) => assert_eq!(format, OutputFormat::Json),
            _ => panic!("expected refresh-media command"),
        }
    }

    #[test]
    fn viz_commands_parse_enable_source_and_status() {
        let cli = Cli::try_parse_from(["spotuify", "viz", "enable"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Viz {
                command: VizCommand::Enable
            })
        ));

        let cli = Cli::try_parse_from(["spotuify", "viz", "source", "loopback"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Viz {
                command: VizCommand::Source { .. }
            })
        ));

        let cli = Cli::try_parse_from(["spotuify", "viz", "status", "--format", "json"]).unwrap();
        match cli.command {
            Some(Command::Viz {
                command: VizCommand::Status { format },
            }) => assert_eq!(format, OutputFormat::Json),
            _ => panic!("expected viz status command"),
        }
    }

    #[test]
    fn hooks_test_command_accepts_machine_output_format() {
        let cli = Cli::try_parse_from(["spotuify", "hooks", "test", "--format", "json"]).unwrap();

        match cli.command {
            Some(Command::Hooks {
                command: HooksCommand::Test { format },
            }) => assert_eq!(format, OutputFormat::Json),
            _ => panic!("expected hooks test command"),
        }
    }

    #[test]
    fn mpris_status_command_accepts_machine_output_format() {
        let cli = Cli::try_parse_from(["spotuify", "mpris", "status", "--format", "json"]).unwrap();

        match cli.command {
            Some(Command::Mpris {
                command: MprisCommand::Status { format },
            }) => assert_eq!(format, OutputFormat::Json),
            _ => panic!("expected mpris status command"),
        }
    }

    #[test]
    fn agent_playlist_workflow_commands_parse_from_phase_five_spec() {
        let cli = Cli::try_parse_from([
            "spotuify",
            "playlist",
            "plan",
            "exile and returning home",
            "--format",
            "json",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Playlist {
                command: PlaylistCommand::Plan { brief, format },
            }) => {
                assert_eq!(brief, "exile and returning home");
                assert_eq!(format, OutputFormat::Json);
            }
            _ => panic!("expected playlist plan command"),
        }

        let cli = Cli::try_parse_from([
            "spotuify",
            "resolve-tracks",
            "--from",
            "plan.json",
            "--format",
            "jsonl",
        ])
        .unwrap();
        match cli.command {
            Some(Command::ResolveTracks { from, format, .. }) => {
                assert_eq!(from, std::path::PathBuf::from("plan.json"));
                assert_eq!(format, OutputFormat::Jsonl);
            }
            _ => panic!("expected resolve-tracks command"),
        }

        let cli = Cli::try_parse_from([
            "spotuify",
            "playlist",
            "create",
            "Exile and Return",
            "--from",
            "candidates.jsonl",
            "--dry-run",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Playlist {
                command:
                    PlaylistCommand::Create {
                        name,
                        from,
                        dry_run,
                        yes,
                        ..
                    },
            }) => {
                assert_eq!(name, "Exile and Return");
                assert_eq!(from, std::path::PathBuf::from("candidates.jsonl"));
                assert!(dry_run);
                assert!(!yes);
            }
            _ => panic!("expected playlist create dry-run command"),
        }

        let cli = Cli::try_parse_from([
            "spotuify",
            "playlist",
            "create",
            "Exile and Return",
            "--from",
            "candidates.jsonl",
            "--yes",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Playlist {
                command:
                    PlaylistCommand::Create {
                        name,
                        dry_run,
                        yes,
                        format,
                        ..
                    },
            }) => {
                assert_eq!(name, "Exile and Return");
                assert!(!dry_run);
                assert!(yes);
                assert_eq!(format, OutputFormat::Table);
            }
            _ => panic!("expected playlist create commit command"),
        }
    }

    #[test]
    fn next_command_accepts_json_receipt_format() {
        let cli = Cli::try_parse_from(["spotuify", "next", "--format", "json"]).unwrap();

        match cli.command {
            Some(Command::Next { format }) => assert_eq!(format, OutputFormat::Json),
            _ => panic!("expected next command"),
        }
    }

    #[test]
    fn zero_arg_playback_commands_accept_json_receipts() {
        for command in ["previous", "pause", "resume", "toggle"] {
            let cli = Cli::try_parse_from(["spotuify", command, "--format", "json"]).unwrap();
            let parsed = match cli.command {
                Some(Command::Previous { format }) => ("previous", format),
                Some(Command::Pause { format }) => ("pause", format),
                Some(Command::Resume { format }) => ("resume", format),
                Some(Command::Toggle { format }) => ("toggle", format),
                _ => panic!("expected playback control command"),
            };

            assert_eq!(parsed, (command, OutputFormat::Json));
        }
    }

    #[test]
    fn play_commands_accept_query_uri_type_and_receipt_format() {
        let cli = Cli::try_parse_from([
            "spotuify",
            "play",
            "imagine dragons",
            "--type",
            "track",
            "--format",
            "json",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Play {
                query,
                kind,
                format,
                ..
            }) => {
                assert_eq!(query, "imagine dragons");
                assert_eq!(kind, SearchKind::Track);
                assert_eq!(format, OutputFormat::Json);
            }
            _ => panic!("expected play command"),
        }

        let cli = Cli::try_parse_from([
            "spotuify",
            "play-uri",
            "spotify:track:abc",
            "--format",
            "json",
        ])
        .unwrap();
        match cli.command {
            Some(Command::PlayUri { uri, format, .. }) => {
                assert_eq!(uri, "spotify:track:abc");
                assert_eq!(format, OutputFormat::Json);
            }
            _ => panic!("expected play-uri command"),
        }
    }

    #[test]
    fn analytics_events_command_accepts_limit_and_jsonl_format() {
        let cli = Cli::try_parse_from([
            "spotuify",
            "analytics",
            "events",
            "--limit",
            "5",
            "--format",
            "jsonl",
        ])
        .unwrap();

        match cli.command {
            Some(Command::Analytics {
                command: AnalyticsCommand::Events { limit, format },
            }) => {
                assert_eq!(limit, 5);
                assert_eq!(format, OutputFormat::Jsonl);
            }
            _ => panic!("expected analytics events command"),
        }
    }

    #[test]
    fn playback_parity_commands_parse_from_phase_one_spec() {
        let cli = Cli::try_parse_from(["spotuify", "seek", "+15s", "--format", "json"]).unwrap();
        match cli.command {
            Some(Command::Seek { offset, format }) => {
                assert_eq!(offset, "+15s");
                assert_eq!(format, OutputFormat::Json);
            }
            _ => panic!("expected seek command"),
        }

        let cli = Cli::try_parse_from(["spotuify", "volume", "70", "--format", "json"]).unwrap();
        match cli.command {
            Some(Command::Volume { percent, format }) => {
                assert_eq!(percent, 70);
                assert_eq!(format, OutputFormat::Json);
            }
            _ => panic!("expected volume command"),
        }

        let cli =
            Cli::try_parse_from(["spotuify", "shuffle", "toggle", "--format", "json"]).unwrap();
        match cli.command {
            Some(Command::Shuffle { state, format }) => {
                assert_eq!(state, ToggleArg::Toggle);
                assert_eq!(format, OutputFormat::Json);
            }
            _ => panic!("expected shuffle command"),
        }

        let cli =
            Cli::try_parse_from(["spotuify", "repeat", "context", "--format", "json"]).unwrap();
        match cli.command {
            Some(Command::Repeat { state, format }) => {
                assert_eq!(state, RepeatArg::Context);
                assert_eq!(format, OutputFormat::Json);
            }
            _ => panic!("expected repeat command"),
        }
    }

    #[test]
    fn search_parity_accepts_artist_play_and_index() {
        let cli = Cli::try_parse_from([
            "spotuify",
            "search",
            "erykah badu",
            "--type",
            "artist",
            "--play",
            "--index",
            "2",
            "--format",
            "json",
        ])
        .unwrap();

        match cli.command {
            Some(Command::Search {
                query,
                kind,
                play,
                index,
                format,
                ..
            }) => {
                assert_eq!(query, "erykah badu");
                assert_eq!(kind, SearchKind::Artist);
                assert!(play);
                assert_eq!(index, 2);
                assert_eq!(format, OutputFormat::Json);
            }
            _ => panic!("expected search command"),
        }
    }

    #[test]
    fn device_queue_playlist_and_library_parity_commands_parse() {
        let cli = Cli::try_parse_from(["spotuify", "transfer", "spotuify-hume"]).unwrap();
        match cli.command {
            Some(Command::Transfer { device, format }) => {
                assert_eq!(device, "spotuify-hume");
                assert_eq!(format, OutputFormat::Table);
            }
            _ => panic!("expected transfer command"),
        }

        let cli = Cli::try_parse_from([
            "spotuify",
            "queue",
            "add",
            "--search",
            "luther vandross",
            "--format",
            "json",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Queue {
                command:
                    Some(QueueCommand::Add {
                        uris,
                        ids,
                        search,
                        many: _,
                        wait: _,
                        format,
                        ..
                    }),
                ..
            }) => {
                assert!(uris.is_empty());
                assert_eq!(ids, None);
                assert_eq!(search.as_deref(), Some("luther vandross"));
                assert_eq!(format, OutputFormat::Json);
            }
            _ => panic!("expected queue add command"),
        }

        let cli = Cli::try_parse_from([
            "spotuify",
            "queue",
            "add",
            "--ids",
            "tracks.txt",
            "--format",
            "json",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Queue {
                command:
                    Some(QueueCommand::Add {
                        uris,
                        ids,
                        search,
                        many: _,
                        wait: _,
                        format,
                        ..
                    }),
                ..
            }) => {
                assert!(uris.is_empty());
                assert_eq!(ids, Some(std::path::PathBuf::from("tracks.txt")));
                assert_eq!(search, None);
                assert_eq!(format, OutputFormat::Json);
            }
            _ => panic!("expected queue add ids command"),
        }

        let cli = Cli::try_parse_from([
            "spotuify",
            "playlist",
            "add",
            "quiet-storm",
            "--ids",
            "tracks.txt",
            "--dry-run",
            "--format",
            "json",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Playlist {
                command:
                    PlaylistCommand::Add {
                        playlist,
                        uris,
                        ids,
                        dry_run,
                        yes,
                        format,
                        ..
                    },
            }) => {
                assert_eq!(playlist, "quiet-storm");
                assert!(uris.is_empty());
                assert_eq!(ids, Some(std::path::PathBuf::from("tracks.txt")));
                assert!(dry_run);
                assert!(!yes);
                assert_eq!(format, OutputFormat::Json);
            }
            _ => panic!("expected playlist add ids dry-run command"),
        }

        let cli = Cli::try_parse_from([
            "spotuify",
            "playlist",
            "remove",
            "Fake Favorites",
            "fake-b:track:track-1",
            "--dry-run",
            "--yes",
            "--provider",
            "fake-b",
            "--format",
            "json",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Playlist {
                command:
                    PlaylistCommand::Remove {
                        playlist,
                        uris,
                        ids,
                        dry_run,
                        yes,
                        provider,
                        format,
                    },
            }) => {
                assert_eq!(playlist, "Fake Favorites");
                assert_eq!(uris, ["fake-b:track:track-1"]);
                assert_eq!(ids, None);
                assert!(dry_run);
                assert!(yes);
                assert_eq!(provider.as_deref(), Some("fake-b"));
                assert_eq!(format, OutputFormat::Json);
            }
            _ => panic!("expected playlist remove provider-scoped dry-run command"),
        }

        let cli = Cli::try_parse_from([
            "spotuify",
            "playlist",
            "add-current",
            "workout",
            "--format",
            "json",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Playlist {
                command:
                    PlaylistCommand::AddCurrent {
                        playlist, format, ..
                    },
            }) => {
                assert_eq!(playlist, "workout");
                assert_eq!(format, OutputFormat::Json);
            }
            _ => panic!("expected playlist add-current command"),
        }

        let cli = Cli::try_parse_from(["spotuify", "like", "current", "--format", "json"]).unwrap();
        match cli.command {
            Some(Command::Like {
                target,
                wait,
                format,
                ..
            }) => {
                assert_eq!(target, "current");
                assert!(!wait);
                assert_eq!(format, OutputFormat::Json);
            }
            _ => panic!("expected like current command"),
        }

        let cli = Cli::try_parse_from([
            "spotuify",
            "like",
            "spotify:track:track-1",
            "--format",
            "json",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Like { target, format, .. }) => {
                assert_eq!(target, "spotify:track:track-1");
                assert_eq!(format, OutputFormat::Json);
            }
            _ => panic!("expected like URI command"),
        }

        let cli = Cli::try_parse_from(["spotuify", "save", "current", "--format", "json"]).unwrap();
        match cli.command {
            Some(Command::Save { target, format, .. }) => {
                assert_eq!(target, "current");
                assert_eq!(format, OutputFormat::Json);
            }
            _ => panic!("expected save current command"),
        }
    }

    #[test]
    fn auth_commands_accept_custom_provider_ids() {
        let onboard =
            Cli::try_parse_from(["spotuify", "onboard", "--provider", "spotify-work"]).unwrap();
        assert!(matches!(
            onboard.command,
            Some(Command::Onboard { provider })
                if provider.as_deref() == Some("spotify-work")
        ));

        let cli = Cli::try_parse_from([
            "spotuify",
            "login",
            "--provider",
            "spotify-work",
            "--redirect-uri",
            "http://127.0.0.1:9999/callback",
            "--dev-app",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Login {
                provider,
                redirect_uri,
                dev_app,
            }) => {
                assert_eq!(provider.as_deref(), Some("spotify-work"));
                assert_eq!(
                    redirect_uri.as_deref(),
                    Some("http://127.0.0.1:9999/callback")
                );
                assert!(dev_app);
            }
            _ => panic!("expected login command"),
        }

        let logout =
            Cli::try_parse_from(["spotuify", "logout", "--provider", "spotify-work"]).unwrap();
        assert!(matches!(
            logout.command,
            Some(Command::Logout { provider })
                if provider.as_deref() == Some("spotify-work")
        ));

        let status =
            Cli::try_parse_from(["spotuify", "auth", "status", "--provider", "spotify-work"])
                .unwrap();
        assert!(matches!(
            status.command,
            Some(Command::Auth {
                command: AuthCommand::Status { provider, .. }
            }) if provider.as_deref() == Some("spotify-work")
        ));
    }

    #[test]
    fn custom_provider_config_paths_do_not_fall_back_to_spotify_id() {
        let provider = spotuify_core::ProviderId::new("spotify-work").unwrap();
        assert_eq!(
            provider_config_path(&provider, "redirect_uri")
                .unwrap()
                .canonical(),
            "providers.spotify-work.redirect_uri"
        );
    }

    #[test]
    fn exit_code_mapping_follows_cli_blueprint() {
        assert_eq!(exit_code_for_message("provide a URI or --search QUERY"), 2);
        assert_eq!(exit_code_for_message("Cannot connect to daemon"), 3);
        assert_eq!(exit_code_for_message("OAuth login required"), 4);
        assert_eq!(exit_code_for_message("no active device"), 5);
        assert_eq!(exit_code_for_message("Spotify API was rate limited"), 6);
        assert_eq!(exit_code_for_message("unsupported capability"), 7);
        assert_eq!(exit_code_for_message("partial mutation failure"), 8);
        assert_eq!(
            exit_code_for_message("cache reset is destructive; re-run with --confirm"),
            2
        );
    }

    #[test]
    fn provider_routing_errors_have_stable_nonzero_exit_codes() {
        let unknown = anyhow::Error::new(spotuify_cli::commands::DaemonRequestError {
            kind: spotuify_protocol::IpcErrorKind::InvalidRequest,
            message: "unknown provider `nope`".to_string(),
            retryable: false,
            provider: None,
            detail: None,
            retry_after_secs: None,
        });
        let unsupported = anyhow::Error::new(spotuify_cli::commands::DaemonRequestError {
            kind: spotuify_protocol::IpcErrorKind::Unsupported,
            message: "provider `fake` does not support playback".to_string(),
            retryable: false,
            provider: None,
            detail: None,
            retry_after_secs: None,
        });
        assert_eq!(super::exit_code_for_error(&unknown), 2);
        assert_eq!(super::exit_code_for_error(&unsupported), 7);
    }

    fn exit_code_for_message(message: &str) -> i32 {
        super::exit_code_for_error(&anyhow::anyhow!("{message}"))
    }
}
