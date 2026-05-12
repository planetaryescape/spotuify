mod actions;
mod analytics;
mod app;
mod auth;
mod config;
mod daemon;
mod diagnostics;
mod logging;
mod output;
mod protocol;
mod selection;
mod spotify;
mod spotifyd;
mod ui;

use std::io::{self, Write};
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
    /// Search Spotify.
    Search {
        /// Search query.
        query: String,
        /// Media type to search.
        #[arg(long = "type", value_enum, default_value = "all")]
        kind: SearchKind,
        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },
    /// Print the current Spotify queue.
    Queue {
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum SearchKind {
    All,
    Track,
    Episode,
    Album,
    Playlist,
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

impl From<SearchKind> for actions::SearchScope {
    fn from(kind: SearchKind) -> Self {
        match kind {
            SearchKind::All => Self::All,
            SearchKind::Track => Self::Track,
            SearchKind::Episode => Self::Episode,
            SearchKind::Album => Self::Album,
            SearchKind::Playlist => Self::Playlist,
        }
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
async fn main() -> Result<()> {
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
        Some(Command::Status { format }) => {
            let config = Config::load().context("failed to load Spotify config")?;
            let mut client = spotify_client(config, AnalyticsSource::Cli).await?;
            let playback = actions::status(&mut client).await?;
            output::print_playback(&playback, format)
        }
        Some(Command::Devices { format }) => {
            let config = Config::load().context("failed to load Spotify config")?;
            let mut client = spotify_client(config, AnalyticsSource::Cli).await?;
            let devices = actions::devices(&mut client).await?;
            output::print_devices(&devices, format)
        }
        Some(Command::Search {
            query,
            kind,
            format,
        }) => {
            let config = Config::load().context("failed to load Spotify config")?;
            let mut client = spotify_client(config, AnalyticsSource::Cli).await?;
            let items = actions::search(&mut client, &query, kind.into()).await?;
            output::print_media_items(&items, format)
        }
        Some(Command::Queue { format }) => {
            let config = Config::load().context("failed to load Spotify config")?;
            let mut client = spotify_client(config, AnalyticsSource::Cli).await?;
            let queue = actions::queue(&mut client).await?;
            output::print_queue(&queue, format)
        }
        Some(Command::Playlists { format }) => {
            let config = Config::load().context("failed to load Spotify config")?;
            let mut client = spotify_client(config, AnalyticsSource::Cli).await?;
            let playlists = actions::playlists(&mut client).await?;
            output::print_playlists(&playlists, format)
        }
        Some(Command::Play {
            query,
            kind,
            format,
        }) => {
            let config = Config::load().context("failed to load Spotify config")?;
            let mut client = spotify_client(config, AnalyticsSource::Cli).await?;
            let item = actions::play_query(&mut client, &query, kind.into()).await?;
            output::print_item_receipt("play", &item, format)
        }
        Some(Command::PlayUri { uri, format }) => {
            let config = Config::load().context("failed to load Spotify config")?;
            let mut client = spotify_client(config, AnalyticsSource::Cli).await?;
            actions::play_uri(&mut client, &uri).await?;
            output::print_basic_receipt("play-uri", &format!("Playing {uri}"), format)
        }
        Some(Command::Next { format }) => {
            let config = Config::load().context("failed to load Spotify config")?;
            let mut client = spotify_client(config, AnalyticsSource::Cli).await?;
            actions::next(&mut client).await?;
            output::print_basic_receipt("next", "Skipped", format)
        }
        Some(Command::Previous { format }) => {
            let config = Config::load().context("failed to load Spotify config")?;
            let mut client = spotify_client(config, AnalyticsSource::Cli).await?;
            actions::previous(&mut client).await?;
            output::print_basic_receipt("previous", "Previous track", format)
        }
        Some(Command::Pause { format }) => {
            let config = Config::load().context("failed to load Spotify config")?;
            let mut client = spotify_client(config, AnalyticsSource::Cli).await?;
            actions::pause(&mut client).await?;
            output::print_basic_receipt("pause", "Paused", format)
        }
        Some(Command::Resume { format }) => {
            let config = Config::load().context("failed to load Spotify config")?;
            let mut client = spotify_client(config, AnalyticsSource::Cli).await?;
            actions::resume(&mut client).await?;
            output::print_basic_receipt("resume", "Playing", format)
        }
        Some(Command::Toggle { format }) => {
            let config = Config::load().context("failed to load Spotify config")?;
            let mut client = spotify_client(config, AnalyticsSource::Cli).await?;
            let is_playing = actions::toggle_playback(&mut client).await?;
            let message = if is_playing { "Playing" } else { "Paused" };
            output::print_basic_receipt("toggle", message, format)
        }
        None => {
            if needs_onboarding()? {
                onboard().await?;
            }
            let config = Config::load().context("failed to load Spotify config")?;
            let client = spotify_client(config, AnalyticsSource::Tui).await?;
            run_tui(client).await
        }
    }
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
        Ok(store) => Ok(client.with_analytics(store, source)),
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

    use super::{AnalyticsCommand, Cli, Command, DaemonCommand, SearchKind};
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
                format,
            }) => {
                assert_eq!(query, "luther vandross");
                assert_eq!(kind, SearchKind::Track);
                assert_eq!(format, OutputFormat::Ids);
            }
            _ => panic!("expected search command"),
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
            Some(Command::Queue { format }) => assert_eq!(format, OutputFormat::Jsonl),
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
}
