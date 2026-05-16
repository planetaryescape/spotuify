mod actions;
mod agent_playlists;
mod analytics;
mod app;
mod auth;
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
mod spotify;
mod spotifyd;
mod store;
mod sync;
mod tui_actions;
mod ui;

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

use crate::analytics::{AnalyticsSource, AnalyticsStore};
use crate::app::run_tui;
use crate::auth::{login, logout, token_status};
use crate::config::{
    config_path, get_config_value, init_config, set_config_value, Config, ConfigKey,
};
use crate::output::OutputFormat;
use crate::spotify::SpotifyClient;
use spotuify_cli::cli_args::{
    LibraryCommand, LyricsCommand, MprisCommand, PlaylistCommand, QueueCommand, VizCommand,
};

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
    /// Guided first-run setup: config, OAuth, and initial Spotify sync.
    Onboard,
    /// Authorize Spotify and store a refresh token in macOS Keychain.
    Login {
        /// Override the redirect URI registered in Spotify's Developer Dashboard.
        #[arg(long)]
        redirect_uri: Option<String>,
    },
    /// Remove the stored Spotify token from macOS Keychain.
    Logout,
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
    /// Search local cache and Spotify.
    Search {
        /// Search query.
        query: String,
        /// Media type to search.
        #[arg(long = "type", value_enum, default_value = "all")]
        kind: SearchKind,
        /// Search source. hybrid returns cached local results immediately and refreshes Spotify in the background.
        #[arg(long, value_enum, default_value = "hybrid")]
        source: SearchSource,
        /// Maximum results to return.
        #[arg(long, default_value_t = 10)]
        limit: u32,
        /// Play one result instead of printing results.
        #[arg(long)]
        play: bool,
        /// 1-based search result index for --play.
        #[arg(long, default_value_t = 1)]
        index: usize,
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Resolve playlist-plan track candidates.
    ResolveTracks {
        /// Playlist plan JSON file.
        #[arg(long = "from")]
        from: PathBuf,
        /// Output format.
        #[arg(long, value_enum, default_value = "jsonl")]
        format: OutputFormat,
    },
    /// Print the current Spotify queue.
    Queue {
        #[command(subcommand)]
        command: Option<QueueCommand>,
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// List the current user's playlists.
    Playlists {
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Search Spotify and play the first matching result.
    Play {
        /// Search query.
        query: String,
        /// Media type to search.
        #[arg(long = "type", value_enum, default_value = "track")]
        kind: SearchKind,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Play a Spotify URI directly.
    PlayUri {
        /// Spotify URI to play.
        uri: String,
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
    /// Synced lyrics operations.
    Lyrics {
        #[command(subcommand)]
        command: LyricsCommand,
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
    /// Save/like a Spotify URI or the current now-playing item.
    Like {
        /// Spotify URI or `current`.
        target: String,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Save a Spotify URI or the current now-playing item.
    Save {
        /// Spotify URI or `current`.
        target: String,
        /// Output format for the mutation receipt.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Show spotuify log file location or recent log lines.
    Logs {
        #[command(subcommand)]
        command: LogsCommand,
    },
    /// Read or write ~/.config/spotuify/spotuify.toml.
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
    /// Phase 13 (P13-I) — force the daemon to re-register its active
    /// player backend (after a VPN flap, network change, etc).
    Reconnect,
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
    /// Refresh local cache from Spotify.
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
        /// Output format.
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum SearchSource {
    Local,
    Spotify,
    Hybrid,
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

impl From<SearchSource> for protocol::SearchSourceData {
    fn from(source: SearchSource) -> Self {
        match source {
            SearchSource::Local => Self::Local,
            SearchSource::Spotify => Self::Spotify,
            SearchSource::Hybrid => Self::Hybrid,
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

fn repeat_arg_value(state: RepeatArg) -> &'static str {
    match state {
        RepeatArg::Off => "off",
        RepeatArg::Context => "context",
        RepeatArg::Track => "track",
    }
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Print the config path.
    Path,
    /// Create the config file if it does not exist.
    Init,
    /// Print a config value.
    Get { key: String },
    /// Set a config value.
    Set { key: String, value: String },
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
    /// Export qualified listens to ListenBrainz / Last.fm.
    Export {
        #[arg(long)]
        target: String,
        #[arg(long)]
        since: Option<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Import historical scrobbles.
    Import {
        #[arg(long)]
        target: String,
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

#[tokio::main]
async fn main() {
    // Phase 11 (F9): force the Windows console code page to UTF-8 before
    // any output so emoji + extended glyphs render correctly under cmd.exe
    // and PowerShell. No-op on macOS/Linux.
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::SetConsoleOutputCP;
        // Safety: ffi call with no side-effects beyond setting the
        // current process's output code page; CP_UTF8 = 65001.
        unsafe {
            SetConsoleOutputCP(65001);
        }
    }
    if let Err(err) = run().await {
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
    let _log_guard =
        logging::init_with_format(log_format).context("failed to initialize logging")?;
    logging::install_panic_hook();
    logging::surface_prior_panic_if_any();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "spotuify starting");

    match cli.command {
        Some(Command::Onboard) => onboard().await,
        Some(Command::Logs { command }) => handle_logs(command),
        Some(Command::Config { command }) => handle_config(command),
        Some(Command::Analytics { command }) => handle_analytics(command).await,
        Some(Command::Ops { command }) => handle_ops(command).await,
        Some(Command::Generate { command }) => handle_generate(command),
        Some(Command::Hooks { command }) => handle_hooks(command).await,
        Some(Command::Mpris { command }) => commands::ipc_mpris(command).await,
        Some(Command::Reload) => commands::ipc_reload().await,
        Some(Command::Reconnect) => commands::ipc_reconnect().await,
        Some(Command::BugReport { log_lines, output }) => bug_report(log_lines, output).await,
        Some(Command::Login { redirect_uri }) => {
            let mut config = Config::load().context("failed to load Spotify config")?;
            if let Some(redirect_uri) = redirect_uri {
                config.redirect_uri = redirect_uri;
            }
            Ok(login(&config).await?)
        }
        Some(Command::Logout) => Ok(logout()?),
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
        Some(Command::Search {
            query,
            kind,
            source,
            limit,
            play,
            index,
            format,
        }) => {
            commands::ipc_search(
                &query,
                kind.into(),
                source.into(),
                limit,
                play,
                index,
                format,
            )
            .await
        }
        Some(Command::ResolveTracks { from, format }) => {
            commands::ipc_resolve_tracks(&from, format).await
        }
        Some(Command::Queue { command, format }) => commands::ipc_queue(command, format).await,
        Some(Command::Playlists { format }) => commands::ipc_playlists(format).await,
        Some(Command::Play {
            query,
            kind,
            format,
        }) => commands::ipc_play_query(&query, kind.into(), format).await,
        Some(Command::PlayUri { uri, format }) => commands::ipc_play_uri(&uri, format).await,
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
                    state: repeat_arg_value(state).to_string(),
                },
                format,
            )
            .await
        }
        Some(Command::Transfer { device, format }) => commands::ipc_transfer(&device, format).await,
        Some(Command::Playlist { command }) => commands::ipc_playlist(command).await,
        Some(Command::Library { command }) => commands::ipc_library(command).await,
        Some(Command::Lyrics { command }) => commands::ipc_lyrics(command).await,
        Some(Command::Viz { command }) => commands::ipc_viz(command).await,
        Some(Command::Like { target, format }) => {
            commands::ipc_save_target("like", &target, format).await
        }
        Some(Command::Save { target, format }) => {
            commands::ipc_save_target("save", &target, format).await
        }
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
            format,
        }) => {
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
            format,
        }) => {
            if prune || older_than.is_some() {
                anyhow::bail!("--prune/--older-than are only valid with `sync search-cache`");
            }
            commands::ipc_sync(target.into(), format).await
        }
        None => {
            if needs_onboarding()? {
                onboard().await?;
            }
            run_tui().await
        }
    }
}

fn exit_code_for_error(err: &anyhow::Error) -> i32 {
    let message = err
        .chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();

    if message.contains("provide ")
        || message.contains("invalid ")
        || message.contains("expected ")
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
        .map(|status| status.socket_reachable)
        .unwrap_or(false)
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
    let (search, worker) =
        search::SearchServiceHandle::start(search::SearchIndex::open(store.index_path())?);
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
    let file_name = db_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "cache.sqlite3".to_string());
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
/// drops the bundled file from `install/` into the right home dir
/// and invokes the platform's `enable` command.
fn install_platform_service() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let agents = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("no home dir"))?
            .join("Library/LaunchAgents");
        std::fs::create_dir_all(&agents).context("could not create ~/Library/LaunchAgents")?;
        let dest = agents.join("dev.spotuify.daemon.plist");
        let src = find_install_file("launchd/dev.spotuify.daemon.plist")?;
        std::fs::copy(&src, &dest).with_context(|| format!("copy {src:?} → {dest:?} failed"))?;
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
                "warning: launchctl bootstrap returned {}; you may need to `launchctl bootout` first",
                status
            );
        }
        println!("Installed launchd agent: {dest:?}");
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        let units = dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("no config dir"))?
            .join("systemd/user");
        std::fs::create_dir_all(&units).context("could not create ~/.config/systemd/user")?;
        let dest = units.join("spotuify-daemon.service");
        let src = find_install_file("systemd/user/spotuify-daemon.service")?;
        std::fs::copy(&src, &dest).with_context(|| format!("copy {src:?} → {dest:?} failed"))?;
        std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status()
            .ok();
        let status = std::process::Command::new("systemctl")
            .args(["--user", "enable", "--now", "spotuify-daemon"])
            .status()?;
        if !status.success() {
            anyhow::bail!("`systemctl --user enable --now spotuify-daemon` failed");
        }
        println!("Installed systemd user unit: {dest:?}");
        println!("Tip: enable lingering with `sudo loginctl enable-linger $USER`");
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        let src = find_install_file("windows/spotuify-daemon-task.xml")?;
        let status = std::process::Command::new("schtasks")
            .args([
                "/Create",
                "/TN",
                "spotuify-daemon",
                "/XML",
                src.to_str().unwrap_or_default(),
                "/F",
            ])
            .status()?;
        if !status.success() {
            anyhow::bail!("`schtasks /Create` failed (status {status})");
        }
        println!("Installed Windows Task Scheduler entry: spotuify-daemon");
        return Ok(());
    }

    #[allow(unreachable_code)]
    {
        anyhow::bail!("daemon install-service is not implemented on this platform")
    }
}

fn uninstall_platform_service() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let uid = std::process::Command::new("id").arg("-u").output()?;
        let uid = String::from_utf8_lossy(&uid.stdout).trim().to_string();
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", &format!("gui/{uid}/dev.spotuify.daemon")])
            .status();
        let path = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("no home dir"))?
            .join("Library/LaunchAgents/dev.spotuify.daemon.plist");
        let _ = std::fs::remove_file(&path);
        println!("Removed launchd agent: {path:?}");
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "--now", "spotuify-daemon"])
            .status();
        let path = dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("no config dir"))?
            .join("systemd/user/spotuify-daemon.service");
        let _ = std::fs::remove_file(&path);
        println!("Removed systemd user unit: {path:?}");
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("schtasks")
            .args(["/Delete", "/TN", "spotuify-daemon", "/F"])
            .status();
        println!("Removed Windows Task Scheduler entry: spotuify-daemon");
        return Ok(());
    }
    #[allow(unreachable_code)]
    {
        anyhow::bail!("daemon uninstall-service is not implemented on this platform")
    }
}

/// Locate a bundled service file in `install/` relative to the
/// running binary. Walks up from `current_exe` so the same lookup
/// works for `cargo run`, an installed `/usr/local/bin/spotuify`,
/// and a Homebrew Cellar layout.
fn find_install_file(relative: &str) -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    for base in exe.ancestors() {
        let candidate = base.join("install").join(relative);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    anyhow::bail!(
        "could not find install/{relative} relative to the running binary; \
         reinstall spotuify from a release tarball that ships install/"
    )
}

async fn spotify_client(config: Config, source: AnalyticsSource) -> Result<SpotifyClient> {
    let client = SpotifyClient::new(config)?;
    match AnalyticsStore::open_default().await {
        Ok(store) => Ok(client.with_analytics(std::sync::Arc::new(store), source)),
        Err(err) => {
            tracing::warn!(error = %err, "analytics store unavailable");
            Ok(client)
        }
    }
}

async fn onboard() -> Result<()> {
    println!("spotuify setup\n");
    println!("This will create your config, save Spotify app credentials, open OAuth, then sync Spotify data.");
    println!("Config: {}\n", init_config()?.display());

    let existing_client_id = std::env::var("SPOTUIFY_CLIENT_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or(get_config_value(ConfigKey::ClientId)?);
    let existing_client_secret = std::env::var("SPOTUIFY_CLIENT_SECRET")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or(get_config_value(ConfigKey::ClientSecret)?);
    let needs_credentials = existing_client_id.is_none() || existing_client_secret.is_none();
    if needs_credentials {
        println!("Spotify Dashboard steps:");
        println!("1. Open https://developer.spotify.com/dashboard");
        println!("2. Create an app named spotuify");
        println!("3. Add this Redirect URI exactly: http://127.0.0.1:8888/callback");
        println!(
            "4. Save settings, then copy Client ID and Client Secret from Basic Information\n"
        );
        let _ = open::that_detached("https://developer.spotify.com/dashboard");
        wait_for_enter(
            "Press Enter when the Spotify app is created and the Redirect URI is saved...",
        )?;
    } else {
        println!("Using saved Spotify app credentials.");
    }

    if needs_credentials {
        let client_id = prompt_required_default("Client ID", existing_client_id.as_deref())?;
        set_config_value(ConfigKey::ClientId, &client_id)?;

        let client_secret =
            prompt_secret_required_default("Client Secret", existing_client_secret.is_some())?
                .or(existing_client_secret);
        if let Some(client_secret) = client_secret {
            set_config_value(ConfigKey::ClientSecret, &client_secret)?;
        }

        let redirect_uri = prompt_default(
            "Redirect URI",
            &get_config_value(ConfigKey::RedirectUri)?
                .unwrap_or_else(|| "http://127.0.0.1:8888/callback".to_string()),
        )?;
        set_config_value(ConfigKey::RedirectUri, &redirect_uri)?;
    }

    println!("\nCredentials saved. Starting Spotify OAuth...");
    let config = Config::load().context("failed to load saved config")?;
    login(&config).await?;

    println!("\nOAuth complete. Syncing Spotify data...");
    initial_sync(config).await?;
    println!("\nSetup complete.");
    Ok(())
}

fn needs_onboarding() -> Result<bool> {
    let path = config_path()?;
    let client_id_present = std::env::var("SPOTUIFY_CLIENT_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .is_some()
        || (path.exists() && get_config_value(ConfigKey::ClientId)?.is_some());
    let token_present = match token_status_bounded(Duration::from_secs(3)) {
        Ok(status) => status.is_some(),
        Err(err) => {
            eprintln!("warning: keychain token status unavailable: {err}");
            true
        }
    };
    Ok(!client_id_present || !token_present)
}

async fn initial_sync(config: Config) -> Result<()> {
    let mut client = spotify_client(config, AnalyticsSource::Cli).await?;
    match client.playback().await {
        Ok(playback) => {
            let now_playing = playback
                .item
                .as_ref()
                .map(|item| item.name.as_str())
                .unwrap_or("nothing playing");
            println!("playback: {now_playing}");
        }
        Err(err) => println!("playback: skipped ({err})"),
    }
    match client.devices().await {
        Ok(devices) => println!("devices: {}", devices.len()),
        Err(err) => println!("devices: skipped ({err})"),
    }
    match client.queue().await {
        Ok(queue) => println!("queue: {} upcoming", queue.items.len()),
        Err(err) => println!("queue: skipped ({err})"),
    }
    match client.playlists().await {
        Ok(playlists) => println!("playlists: {}", playlists.len()),
        Err(err) => println!("playlists: skipped ({err})"),
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

fn prompt_secret_required_default(label: &str, has_default: bool) -> Result<Option<String>> {
    loop {
        let prompt = if has_default {
            format!("{label} [press Enter to keep saved]: ")
        } else {
            format!("{label}: ")
        };
        let value = rpassword::prompt_password(prompt)?;
        if !value.trim().is_empty() {
            return Ok(Some(value.trim().to_string()));
        }
        if has_default {
            return Ok(None);
        }
        println!("{label} is required.");
    }
}

fn prompt_default(label: &str, default: &str) -> Result<String> {
    print!("{label} [{default}]: ");
    io::stdout().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
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
    io::stdin().read_line(&mut value)?;
    Ok(value)
}

fn wait_for_enter(message: &str) -> Result<()> {
    print!("{message}");
    io::stdout().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    Ok(())
}

fn handle_logs(command: LogsCommand) -> Result<()> {
    match command {
        LogsCommand::Path => println!("{}", logging::log_path()?.display()),
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
                            "path": logging::log_path()?.display().to_string()
                        })
                    );
                } else {
                    println!("no logs yet: {}", logging::log_path()?.display());
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
    let path = logging::log_path()?;
    let mut pos = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
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
        ConfigCommand::Get { key } => {
            if let Some(value) = get_config_value(ConfigKey::parse(&key)?)? {
                println!("{value}");
            }
        }
        ConfigCommand::Set { key, value } => {
            let path = set_config_value(ConfigKey::parse(&key)?, &value)?;
            println!("updated {}", path.display());
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
        AnalyticsCommand::Import { target, format } => {
            let request = spotuify_protocol::Request::AnalyticsImport {
                target: parse_export_target(&target)?,
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
            let config = Config::load().context("failed to load config")?;
            let hook_command = config
                .analytics
                .hook_command
                .clone()
                .or_else(|| config.player.event_hook.clone())
                .context("no hook configured; set analytics.hook_command")?;
            let timeout_ms = config.analytics.hook_timeout_ms;
            let dispatcher = spotuify_system::HookDispatcher::new(spotuify_system::HookConfig {
                hook_command: hook_command.clone(),
                timeout_ms,
            });
            dispatcher
                .fire_checked(spotuify_system::HookEvent::ListenQualified {
                    uri: "spotify:track:spotuify-hook-test".to_string(),
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
            .map(|d| d.as_secs())
            .unwrap_or(0);
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
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mtime_oct = format!("{:011o}", mtime);
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
    let chk_oct = format!("{:06o}\0 ", chksum);
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

async fn doctor(format: OutputFormat) -> Result<()> {
    let daemon = daemon::server::daemon_status().await?;
    let report = diagnostics::collect_report(daemon).await?;
    diagnostics::print_report(&report, format)
}

fn token_status_bounded(timeout: Duration) -> Result<Option<String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(token_status());
    });

    match rx.recv_timeout(timeout) {
        Ok(result) => Ok(result?),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err(anyhow::anyhow!("timed out after {}s", timeout.as_secs()))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(anyhow::anyhow!("keychain status worker exited"))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic, clippy::unwrap_used)]

    use clap::Parser;

    use super::{
        bug_report, ensure_ops_undo_allowed, reset_cache_files, AnalyticsCommand, CacheCommand,
        Cli, Command, DaemonCommand, HooksCommand, LyricsCommand, MprisCommand, PlaylistCommand,
        QueueCommand, RepeatArg, SearchKind, SearchSource, SyncTarget, ToggleArg, VizCommand,
    };
    use crate::output::OutputFormat;

    static BUG_REPORT_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn restore_env(key: &str, value: Option<std::ffi::OsString>) {
        if let Some(value) = value {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
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
        let _guard = BUG_REPORT_ENV_LOCK.lock().await;
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
            Some(Command::Playlists { format }) => assert_eq!(format, OutputFormat::Ids),
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
                command: LyricsCommand::Show { track, format },
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
                command: LyricsCommand::Export { track_uri, output },
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
            Some(Command::ResolveTracks { from, format }) => {
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
            Some(Command::PlayUri { uri, format }) => {
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
                        format,
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
                        format,
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
            "add-current",
            "workout",
            "--format",
            "json",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Playlist {
                command: PlaylistCommand::AddCurrent { playlist, format },
            }) => {
                assert_eq!(playlist, "workout");
                assert_eq!(format, OutputFormat::Json);
            }
            _ => panic!("expected playlist add-current command"),
        }

        let cli = Cli::try_parse_from(["spotuify", "like", "current", "--format", "json"]).unwrap();
        match cli.command {
            Some(Command::Like { target, format }) => {
                assert_eq!(target, "current");
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
            Some(Command::Like { target, format }) => {
                assert_eq!(target, "spotify:track:track-1");
                assert_eq!(format, OutputFormat::Json);
            }
            _ => panic!("expected like URI command"),
        }

        let cli = Cli::try_parse_from(["spotuify", "save", "current", "--format", "json"]).unwrap();
        match cli.command {
            Some(Command::Save { target, format }) => {
                assert_eq!(target, "current");
                assert_eq!(format, OutputFormat::Json);
            }
            _ => panic!("expected save current command"),
        }
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

    fn exit_code_for_message(message: &str) -> i32 {
        super::exit_code_for_error(&anyhow::anyhow!("{}", message))
    }
}
