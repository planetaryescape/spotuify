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
use std::path::PathBuf;
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
use spotuify_cli::cli_args::{LibraryCommand, PlaylistCommand, QueueCommand};

#[derive(Parser)]
#[command(name = "spotuify", version, about = "A keyboard-native Spotify TUI")]
struct Cli {
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
    Devices,
    Playlists,
    Recent,
    Library,
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
}

#[derive(Subcommand)]
enum CacheCommand {
    /// Show local cache row counts and freshness.
    Status {
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
            SyncTarget::Devices => Self::Devices,
            SyncTarget::Playlists => Self::Playlists,
            SyncTarget::Recent => Self::Recent,
            SyncTarget::Library => Self::Library,
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
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("error: {err:#}");
        std::process::exit(exit_code_for_error(&err));
    }
}

async fn run() -> Result<()> {
    let _log_guard = logging::init().context("failed to initialize logging")?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "spotuify starting");
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Onboard) => onboard().await,
        Some(Command::Logs { command }) => handle_logs(command),
        Some(Command::Config { command }) => handle_config(command),
        Some(Command::Analytics { command }) => handle_analytics(command).await,
        Some(Command::Login { redirect_uri }) => {
            let mut config = Config::load().context("failed to load Spotify config")?;
            if let Some(redirect_uri) = redirect_uri {
                config.redirect_uri = redirect_uri;
            }
            login(&config).await
        }
        Some(Command::Logout) => logout(),
        Some(Command::Doctor { format }) => doctor(format).await,
        Some(Command::Daemon { command }) => handle_daemon(command).await,
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
            let current = match commands::daemon_current_playback().await? {
                Some(playback) => playback.progress_ms,
                None => 0,
            };
            let position_ms = selection::parse_seek_target(&offset, current)?;
            commands::ipc_playback_command(
                crate::protocol::PlaybackCommand::Seek { position_ms },
                format,
            )
            .await
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
        Some(Command::Like { target, format }) => {
            commands::ipc_save_target("like", &target, format).await
        }
        Some(Command::Save { target, format }) => {
            commands::ipc_save_target("save", &target, format).await
        }
        Some(Command::Reindex { format }) => commands::ipc_reindex(format).await,
        Some(Command::Cache { command }) => match command {
            CacheCommand::Status { format } => commands::ipc_cache_status(format).await,
        },
        Some(Command::Sync { target, format }) => commands::ipc_sync(target.into(), format).await,
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

    if message.contains("provide ") || message.contains("invalid ") || message.contains("expected ")
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
    }
    Ok(())
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
        LogsCommand::Tail { lines } => {
            let logs = logging::read_tail(lines)?;
            if logs.is_empty() {
                println!("no logs yet: {}", logging::log_path()?.display());
            } else {
                println!("{logs}");
            }
        }
    }
    Ok(())
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
    }
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
        Ok(result) => result,
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
    use clap::Parser;

    use super::{
        AnalyticsCommand, CacheCommand, Cli, Command, DaemonCommand, PlaylistCommand, QueueCommand,
        RepeatArg, SearchKind, SearchSource, ToggleArg,
    };
    use crate::output::OutputFormat;

    #[test]
    fn status_command_accepts_machine_output_format() {
        let cli = Cli::try_parse_from(["spotuify", "status", "--format", "json"]).unwrap();

        match cli.command {
            Some(Command::Status { format }) => assert_eq!(format, OutputFormat::Json),
            _ => panic!("expected status command"),
        }
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
    }

    fn exit_code_for_message(message: &str) -> i32 {
        super::exit_code_for_error(&anyhow::anyhow!("{}", message))
    }
}
