use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

use spotuify_core::{MediaItem, MediaKind, Playback, Playlist};
use spotuify_protocol::{
    DaemonEvent, IpcClient, OperationSource, PlaybackCommand, Request, Response, ResponseData,
    SearchScopeData, SearchSourceData, SyncTargetData,
};

use crate::output::{self, OutputFormat};
use crate::selection;

pub async fn ipc_status(format: OutputFormat) -> Result<()> {
    match daemon_request(Request::PlaybackGet).await? {
        ResponseData::Playback { playback } => output::print_playback(&playback, format),
        _ => unexpected_response(),
    }
}

pub async fn ipc_devices(format: OutputFormat) -> Result<()> {
    match daemon_request(Request::DevicesList).await? {
        ResponseData::Devices { devices } => output::print_devices(&devices, format),
        _ => unexpected_response(),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn ipc_search(
    query: &str,
    scope: SearchScopeData,
    source: SearchSourceData,
    limit: u32,
    pages: u8,
    play: bool,
    index: usize,
    format: OutputFormat,
) -> Result<()> {
    // pages > 1 uses the same streaming path as the TUI (Request::SearchStream
    // → 18 parallel daemon-spawned tasks → DaemonEvent::SearchPage events →
    // SearchComplete). Aggregate events synchronously before printing so the
    // CLI experience stays one-shot.
    let items = if pages > 1 {
        stream_search_aggregate(query, scope, source).await?
    } else {
        match daemon_request(Request::Search {
            query: query.to_string(),
            scope,
            source,
            limit,
        })
        .await?
        {
            ResponseData::SearchResults { items } => items,
            _ => return unexpected_response(),
        }
    };

    if play {
        let item = selection::media_item_at_index(items, query, index)?;
        daemon_request(Request::PlaybackCommand {
            command: PlaybackCommand::PlayUri {
                uri: item.uri.clone(),
            },
        })
        .await?;
        return output::print_item_receipt("play", &item, format);
    }

    output::print_media_items(&items, format)
}

/// CLI equivalent of TUI scroll-load-more: fetch a single page of
/// results for one media kind at a given offset.
pub async fn ipc_search_page(
    query: &str,
    kind: MediaKind,
    offset: u32,
    format: OutputFormat,
) -> Result<()> {
    let version = 1u64;
    let mut client = IpcClient::connect_with_source(OperationSource::Cli).await?;
    let ack = client
        .request(Request::SearchPage {
            query: query.to_string(),
            kind: kind.clone(),
            offset,
            version,
        })
        .await?;
    match ack {
        Response::Ok {
            data: ResponseData::SearchStarted { .. },
        } => {}
        Response::Error { message, .. } => {
            anyhow::bail!("search-page request failed: {message}");
        }
        other => anyhow::bail!("unexpected ack: {other:?}"),
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    loop {
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for SearchPage event");
        }
        let ev = tokio::time::timeout(Duration::from_millis(500), client.next_event()).await;
        match ev {
            Ok(Ok(DaemonEvent::SearchPage {
                kind: ev_kind,
                offset: ev_offset,
                version: ev_version,
                items,
                ..
            })) if ev_kind == kind && ev_offset == offset && ev_version == version => {
                return output::print_media_items(&items, format);
            }
            Ok(Ok(DaemonEvent::SearchFailed {
                kind: Some(ev_kind),
                offset: Some(ev_offset),
                version: ev_version,
                message,
                ..
            })) if ev_kind == kind && ev_offset == offset && ev_version == version => {
                anyhow::bail!("{message}");
            }
            Ok(Ok(_)) => continue,
            Ok(Err(e)) => return Err(e),
            Err(_) => continue,
        }
    }
}

/// Connect, subscribe to events, fire `Request::SearchStream`, drain
/// pages until `SearchComplete`. Used by `spotuify search --pages 3`
/// to give CLI users the same 180-result capability as the TUI.
async fn stream_search_aggregate(
    query: &str,
    scope: SearchScopeData,
    source: SearchSourceData,
) -> Result<Vec<MediaItem>> {
    let version = 1u64;
    let mut client = IpcClient::connect_with_source(OperationSource::Cli).await?;
    let ack = client
        .request(Request::SearchStream {
            query: query.to_string(),
            scope,
            source,
            version,
        })
        .await?;
    match ack {
        Response::Ok {
            data: ResponseData::SearchStarted { .. },
        } => {}
        Response::Error { message, .. } => {
            anyhow::bail!("search-stream request failed: {message}");
        }
        other => anyhow::bail!("unexpected ack: {other:?}"),
    }

    let mut items: Vec<MediaItem> = Vec::new();
    let mut seen_uris: std::collections::HashSet<String> = std::collections::HashSet::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if std::time::Instant::now() >= deadline {
            // Partial results are better than a hard error; just return
            // what we collected so far. Mirrors TUI behavior when an
            // event leg lags.
            break;
        }
        let ev = tokio::time::timeout(Duration::from_millis(500), client.next_event()).await;
        match ev {
            Ok(Ok(DaemonEvent::SearchPage {
                version: ev_version,
                items: page_items,
                ..
            })) if ev_version == version => {
                for item in page_items {
                    if seen_uris.insert(item.uri.clone()) {
                        items.push(item);
                    }
                }
            }
            Ok(Ok(DaemonEvent::SearchComplete {
                version: ev_version,
                ..
            })) if ev_version == version => break,
            Ok(Ok(DaemonEvent::SearchFailed {
                version: ev_version,
                message,
                ..
            })) if ev_version == version => anyhow::bail!("{message}"),
            Ok(Ok(_)) => continue,
            Ok(Err(e)) => return Err(e),
            Err(_) => continue,
        }
    }
    Ok(items)
}

pub async fn ipc_queue(command: Option<crate::QueueCommand>, format: OutputFormat) -> Result<()> {
    match command {
        Some(crate::QueueCommand::Add {
            uris,
            ids,
            search,
            format,
        }) => ipc_queue_add(uris, ids, search, format).await,
        None => match daemon_request(Request::QueueGet).await? {
            ResponseData::Queue { queue } => output::print_queue(&queue, format),
            _ => unexpected_response(),
        },
    }
}

pub async fn ipc_playlists(format: OutputFormat) -> Result<()> {
    match daemon_request(Request::PlaylistsList).await? {
        ResponseData::Playlists { playlists } => output::print_playlists(&playlists, format),
        _ => unexpected_response(),
    }
}

pub async fn ipc_resolve_tracks(from: &Path, format: OutputFormat) -> Result<()> {
    let raw = read_input(from)?;
    let plan = crate::agent_playlists::parse_plan(&raw)?;
    let mut results = Vec::with_capacity(plan.candidate_searches.len());
    for query in &plan.candidate_searches {
        let items = match daemon_request(Request::Search {
            query: query.clone(),
            scope: SearchScopeData::Track,
            // Plan resolution = catalog discovery, not library lookup.
            source: SearchSourceData::Spotify,
            limit: 50,
        })
        .await?
        {
            ResponseData::SearchResults { items } => items,
            _ => return unexpected_response(),
        };
        results.push(items);
    }
    let candidates = crate::agent_playlists::resolve_plan_candidates(&plan, results);
    output::print_resolved_track_candidates(&candidates, format)
}

pub async fn ipc_play_query(
    query: &str,
    scope: SearchScopeData,
    format: OutputFormat,
) -> Result<()> {
    // `spotuify play <query>` is a "find anywhere and play" command
    // — catalog discovery, not library lookup. Limit=10 keeps the
    // search slim since we only consume the top result.
    ipc_search(
        query,
        scope,
        SearchSourceData::Spotify,
        10,
        1,
        true,
        1,
        format,
    )
    .await
}

pub async fn ipc_reindex(format: OutputFormat) -> Result<()> {
    match daemon_request(Request::Reindex).await? {
        ResponseData::Reindex { stats } => output::print_reindex_stats(&stats, format),
        _ => unexpected_response(),
    }
}

pub async fn ipc_cache_status(format: OutputFormat) -> Result<()> {
    match daemon_request(Request::CacheStatus).await? {
        ResponseData::CacheStatus { status } => output::print_cache_status(&status, format),
        _ => unexpected_response(),
    }
}

pub async fn ipc_lyrics(command: crate::LyricsCommand) -> Result<()> {
    match command {
        crate::LyricsCommand::Show { track, format } => {
            let data = daemon_request(Request::LyricsGet {
                track_uri: track,
                force_refresh: false,
            })
            .await?;
            output::print_response_data(&data, format)
        }
        crate::LyricsCommand::Fetch { track_uri, format } => {
            let data = daemon_request(Request::LyricsGet {
                track_uri: Some(track_uri),
                force_refresh: true,
            })
            .await?;
            output::print_response_data(&data, format)
        }
        crate::LyricsCommand::Export { track_uri, output } => {
            let data = daemon_request(Request::LyricsGet {
                track_uri: Some(track_uri),
                force_refresh: false,
            })
            .await?;
            output::export_lyrics_lrc(&data, output.as_deref())
        }
        crate::LyricsCommand::Offset {
            track_uri,
            offset,
            format,
        } => {
            let offset_ms = parse_lyrics_offset(&offset)?;
            let data = daemon_request(Request::LyricsOffsetSet {
                track_uri,
                offset_ms,
            })
            .await?;
            output::print_response_data(&data, format)
        }
    }
}

pub async fn ipc_sync(target: SyncTargetData, format: OutputFormat) -> Result<()> {
    match daemon_request(Request::Sync { target }).await? {
        ResponseData::Sync { summary } => output::print_sync_summary(&summary, format),
        _ => unexpected_response(),
    }
}

pub async fn ipc_viz(command: crate::VizCommand) -> Result<()> {
    match command {
        crate::VizCommand::Enable => print_ack(Request::SetVizEnabled { enabled: true }).await,
        crate::VizCommand::Disable => print_ack(Request::SetVizEnabled { enabled: false }).await,
        crate::VizCommand::Source { kind } => {
            print_ack(Request::SetVizSource { kind: kind.into() }).await
        }
        crate::VizCommand::Status { format } => {
            match daemon_request(Request::GetVizStatus).await? {
                data @ ResponseData::VizStatus { .. } => output::print_response_data(&data, format),
                _ => unexpected_response(),
            }
        }
    }
}

pub async fn ipc_mpris(command: crate::MprisCommand) -> Result<()> {
    match command {
        crate::MprisCommand::Status { format } => {
            match daemon_request(Request::GetDoctorReport).await? {
                ResponseData::DoctorReport { report } => {
                    let diagnostics = report
                        .system
                        .context("daemon did not return media-control diagnostics")?;
                    output::print_system_diagnostics(&diagnostics, format)
                }
                _ => unexpected_response(),
            }
        }
    }
}

pub async fn ipc_play_uri(uri: &str, format: OutputFormat) -> Result<()> {
    print_mutation(
        daemon_request(Request::PlaybackCommand {
            command: PlaybackCommand::PlayUri {
                uri: uri.to_string(),
            },
        })
        .await?,
        format,
    )
}

async fn print_ack(request: Request) -> Result<()> {
    match daemon_request(request).await? {
        ResponseData::Ack { message } => {
            println!("{message}");
            Ok(())
        }
        _ => unexpected_response(),
    }
}

pub async fn ipc_playback_command(action: PlaybackCommand, format: OutputFormat) -> Result<()> {
    print_mutation(
        daemon_request(Request::PlaybackCommand { command: action }).await?,
        format,
    )
}

pub async fn daemon_current_playback() -> Result<Option<Playback>> {
    match daemon_request(Request::PlaybackGet).await? {
        ResponseData::Playback { playback } => Ok(Some(playback)),
        _ => unexpected_response(),
    }
}

pub async fn ipc_transfer(device: &str, format: OutputFormat) -> Result<()> {
    print_mutation(
        daemon_request(Request::DeviceTransfer {
            device: device.to_string(),
        })
        .await?,
        format,
    )
}

pub async fn ipc_playlist(command: crate::PlaylistCommand) -> Result<()> {
    match command {
        crate::PlaylistCommand::Plan { brief, format } => {
            let plan = crate::agent_playlists::build_playlist_plan(&brief)?;
            output::print_playlist_plan(&plan, format)
        }
        crate::PlaylistCommand::Create {
            name,
            from,
            dry_run,
            yes,
            format,
        } => ipc_playlist_create(&name, &from, dry_run, yes, format).await,
        crate::PlaylistCommand::Tracks { playlist, format } => {
            match daemon_request(Request::PlaylistTracks {
                playlist,
                wait: true,
            })
            .await?
            {
                ResponseData::MediaItems { items } => output::print_media_items(&items, format),
                _ => unexpected_response(),
            }
        }
        crate::PlaylistCommand::Play { playlist, format } => {
            let playlists = match daemon_request(Request::PlaylistsList).await? {
                ResponseData::Playlists { playlists } => playlists,
                _ => return unexpected_response(),
            };
            let playlist = selection::resolve_playlist(&playlists, &playlist)?;
            ipc_play_uri(&selection::playlist_uri(&playlist.id), format).await
        }
        crate::PlaylistCommand::Add {
            playlist,
            uris,
            ids,
            dry_run,
            yes,
            format,
        } => ipc_playlist_add(&playlist, uris, ids, dry_run, yes, format).await,
        crate::PlaylistCommand::AddCurrent { playlist, format } => {
            let item = match daemon_request(Request::PlaybackGet).await? {
                ResponseData::Playback { playback } => {
                    playback.item.context("nothing is playing")?
                }
                _ => return unexpected_response(),
            };
            print_mutation(
                daemon_request(Request::PlaylistAddItems {
                    playlist,
                    uris: vec![item.uri],
                })
                .await?,
                format,
            )
        }
    }
}

async fn ipc_playlist_create(
    name: &str,
    from: &Path,
    dry_run: bool,
    yes: bool,
    format: OutputFormat,
) -> Result<()> {
    crate::agent_playlists::ensure_playlist_create_allowed(dry_run, yes)?;
    let raw = read_input(from)?;
    let candidates = crate::agent_playlists::parse_candidates_jsonl(&raw)?;
    let preview = crate::agent_playlists::build_playlist_preview(name, &candidates);
    if dry_run {
        return output::print_playlist_preview(&preview, format);
    }
    let uris = crate::agent_playlists::selected_track_uris(&candidates);
    if uris.is_empty() {
        anyhow::bail!("no resolved track URIs to add");
    }
    match daemon_request(Request::PlaylistCreate {
        name: name.to_string(),
        description: None,
        uris,
    })
    .await?
    {
        ResponseData::PlaylistCreate { receipt } => {
            output::print_playlist_create_receipt(&receipt, format)
        }
        _ => unexpected_response(),
    }
}

pub async fn ipc_library(command: crate::LibraryCommand) -> Result<()> {
    match command {
        crate::LibraryCommand::Tracks { limit, format } => {
            match daemon_request(Request::LibraryList { limit }).await? {
                ResponseData::MediaItems { items } => output::print_media_items(&items, format),
                _ => unexpected_response(),
            }
        }
    }
}

pub async fn ipc_save_target(action: &str, target: &str, format: OutputFormat) -> Result<()> {
    let current = target.eq_ignore_ascii_case("current");
    let data = daemon_request(Request::LibrarySave {
        uri: (!current).then(|| target.to_string()),
        current,
    })
    .await?;
    match data {
        ResponseData::Mutation { mut receipt } => {
            receipt.action = action.to_string();
            output::print_basic_receipt(&receipt.action, &receipt.message, format)
        }
        _ => unexpected_response(),
    }
}

fn print_mutation(data: ResponseData, format: OutputFormat) -> Result<()> {
    match data {
        ResponseData::Mutation { receipt } => {
            output::print_basic_receipt(&receipt.action, &receipt.message, format)
        }
        _ => unexpected_response(),
    }
}

async fn ipc_queue_add(
    uris: Vec<String>,
    ids: Option<PathBuf>,
    search: Option<String>,
    format: OutputFormat,
) -> Result<()> {
    match search {
        Some(query) => {
            if !uris.is_empty() || ids.is_some() {
                anyhow::bail!("provide URI(s), --ids, or --search, not more than one");
            }
            let items = match daemon_request(Request::Search {
                query: query.clone(),
                scope: SearchScopeData::Track,
                source: SearchSourceData::Spotify,
                limit: 50,
            })
            .await?
            {
                ResponseData::SearchResults { items } => items,
                _ => return unexpected_response(),
            };
            let item = selection::media_item_at_index(items, &query, 1)?;
            daemon_request(Request::QueueAdd {
                uri: item.uri.clone(),
            })
            .await?;
            output::print_item_receipt("queue", &item, format)
        }
        None => {
            let selection = selection::resolve_uri_selection(
                uris,
                ids.as_deref(),
                "provide a URI or --search QUERY",
            )?;
            let mut errors = Vec::new();
            let mut succeeded = 0;
            for uri in &selection.uris {
                match daemon_request(Request::QueueAdd { uri: uri.clone() }).await {
                    Ok(ResponseData::Mutation { .. }) => succeeded += 1,
                    Ok(_) => errors.push(output::MutationOutputError {
                        uri: uri.clone(),
                        error: "unexpected response from daemon".to_string(),
                    }),
                    Err(err) => errors.push(output::MutationOutputError {
                        uri: uri.clone(),
                        error: err.to_string(),
                    }),
                }
            }
            let failed = errors.len();
            let receipt = output::MutationOutput {
                ok: failed == 0,
                action: "queue".to_string(),
                dry_run: Some(false),
                playlist: None,
                playlist_name: None,
                requested: selection.uris.len(),
                succeeded,
                failed,
                uris: selection.uris,
                errors,
                message: format!("Queued {succeeded} item(s)"),
            };
            output::print_mutation_output(&receipt, format)?;
            if receipt.failed > 0 {
                anyhow::bail!(
                    "partial mutation failure: queued {}, failed {}",
                    receipt.succeeded,
                    receipt.failed
                );
            }
            Ok(())
        }
    }
}

async fn ipc_playlist_add(
    playlist: &str,
    uris: Vec<String>,
    ids: Option<PathBuf>,
    dry_run: bool,
    yes: bool,
    format: OutputFormat,
) -> Result<()> {
    let selection = selection::resolve_uri_selection(
        uris,
        ids.as_deref(),
        "provide playlist URI(s), --ids FILE, or pipe IDs on stdin",
    )?;
    selection::ensure_track_or_episode_uris(&selection.uris)?;
    let playlist = daemon_playlist(playlist).await?;

    if dry_run {
        return output::print_mutation_output(
            &playlist_add_receipt(&playlist, &selection.uris, true, 0, Vec::new()),
            format,
        );
    }

    if selection.requires_confirmation() && !yes {
        confirm_playlist_add(&playlist, &selection.uris)?;
    }

    match daemon_request(Request::PlaylistAddItems {
        playlist: playlist.id.clone(),
        uris: selection.uris.clone(),
    })
    .await?
    {
        ResponseData::Mutation { .. } => output::print_mutation_output(
            &playlist_add_receipt(
                &playlist,
                &selection.uris,
                false,
                selection.uris.len(),
                Vec::new(),
            ),
            format,
        ),
        _ => unexpected_response(),
    }
}

async fn daemon_playlist(value: &str) -> Result<Playlist> {
    let playlists = match daemon_request(Request::PlaylistsList).await? {
        ResponseData::Playlists { playlists } => playlists,
        _ => return unexpected_response(),
    };
    Ok(selection::resolve_playlist(&playlists, value)?)
}

fn playlist_add_receipt(
    playlist: &Playlist,
    uris: &[String],
    dry_run: bool,
    succeeded: usize,
    errors: Vec<output::MutationOutputError>,
) -> output::MutationOutput {
    let failed = errors.len();
    let message = if dry_run {
        format!("Would add {} item(s) to {}", uris.len(), playlist.name)
    } else {
        format!("Added {succeeded} item(s) to {}", playlist.name)
    };
    output::MutationOutput {
        ok: failed == 0,
        action: "playlist-add".to_string(),
        dry_run: Some(dry_run),
        playlist: Some(playlist.id.clone()),
        playlist_name: Some(playlist.name.clone()),
        requested: uris.len(),
        succeeded,
        failed,
        uris: uris.to_vec(),
        errors,
        message,
    }
}

fn confirm_playlist_add(playlist: &Playlist, uris: &[String]) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        anyhow::bail!(
            "Confirmation required for `playlist add`. Re-run with --yes or inspect with --dry-run."
        );
    }
    println!("Would add {} item(s) to {}", uris.len(), playlist.name);
    for uri in uris.iter().take(8) {
        println!("- {uri}");
    }
    if uris.len() > 8 {
        println!("... and {} more", uris.len() - 8);
    }
    print!("\nContinue? [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        return Ok(());
    }
    anyhow::bail!("Aborted")
}

async fn daemon_request(request: Request) -> Result<ResponseData> {
    spotuify_daemon::server::ensure_daemon_running().await?;
    let mut client = IpcClient::connect_with_source(OperationSource::Cli).await?;
    let response = client.request(request.clone()).await?;
    match response {
        Response::Ok { data } => Ok(data),
        Response::Error {
            kind: spotuify_protocol::IpcErrorKind::AuthRevoked,
            message,
            ..
        } => handle_auth_revoked_then_retry(request, &message).await,
        Response::Error { message, .. } => anyhow::bail!(message),
    }
}

/// Interactive auto-recovery on `IpcErrorKind::AuthRevoked`. Prompts
/// Format OAuth progress as the same human-readable lines the CLI
/// has always emitted. Used by both `spotuify login` and the
/// auth-revoked retry path so the user sees identical output.
fn cli_login_progress(event: spotuify_spotify::auth::LoginProgress) {
    use spotuify_spotify::auth::LoginProgress;
    match event {
        LoginProgress::OpeningBrowser {
            auth_url,
            redirect_uri,
        } => {
            eprintln!("Opening Spotify authorization in your browser...");
            eprintln!("Spotify Dashboard Redirect URI should be one of:");
            eprintln!("  {redirect_uri}");
            eprintln!("  http://127.0.0.1/callback  (loopback dynamic-port allowlist)");
            eprintln!("Do not use the Website field, localhost, or a trailing slash.\n");
            eprintln!("If it does not open, visit:\n{auth_url}\n");
        }
        LoginProgress::BrowserLaunchFailed {
            auth_url,
            redirect_uri,
            error,
        } => {
            eprintln!(
                "Could not launch a browser automatically ({error}).\nOpen this URL in any browser:\n  {auth_url}\n(Waiting for the OAuth callback on {redirect_uri})"
            );
        }
        LoginProgress::WaitingForCallback => {}
        LoginProgress::Saved => {
            eprintln!("Spotify auth saved in macOS Keychain.");
        }
    }
}

/// the user on stdin; on Y, runs the same OAuth flow as `spotuify
/// login`, asks the daemon to drop its stale token cache, then
/// retries the original request exactly once.
///
/// Non-TTY callers (scripts, pipes) skip the prompt and exit with
/// the actionable error message — they have no way to answer "Y".
async fn handle_auth_revoked_then_retry(
    request: Request,
    original_message: &str,
) -> Result<ResponseData> {
    use std::io::{BufRead, IsTerminal, Write};

    eprintln!("Spotify session expired ({original_message}).");

    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "Spotify session expired and stdin is not a TTY; run `spotuify login` to recover"
        );
    }

    eprint!("Re-authenticate now? [Y/n] ");
    std::io::stderr().flush().ok();
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .context("failed to read stdin")?;
    let answer = answer.trim();
    let consent = answer.is_empty() || matches!(answer, "y" | "Y" | "yes" | "Yes" | "YES");
    if !consent {
        anyhow::bail!("Aborted. Run `spotuify login` when you're ready to re-authenticate.");
    }

    eprintln!("Re-authenticating…");
    let config =
        spotuify_spotify::config::Config::load().context("failed to load Spotify config")?;
    spotuify_spotify::auth::login(&config, cli_login_progress)
        .await
        .context("OAuth flow failed")?;

    // Tell the daemon to drop its cached broken token + clear the
    // auth-revoked latch so the retry doesn't immediately fail again
    // with the same error.
    let mut client = IpcClient::connect_with_source(OperationSource::Cli).await?;
    let _ = client.request(Request::ReloadAuth).await?;

    eprintln!("Retrying original command…");
    let mut retry_client = IpcClient::connect_with_source(OperationSource::Cli).await?;
    match retry_client.request(request).await? {
        Response::Ok { data } => Ok(data),
        Response::Error { message, .. } => anyhow::bail!(message),
    }
}

/// Phase 13 (P13-I) — reload the daemon's view of the config file
/// without a restart. Player backend swaps still require a restart;
/// the daemon returns a clear Ack with the message.
pub async fn ipc_reload() -> Result<()> {
    match daemon_request(Request::Reload).await? {
        ResponseData::Ack { message } => {
            println!("{message}");
            Ok(())
        }
        _ => unexpected_response(),
    }
}

/// Phase 13 (P13-I) — request the daemon re-register its active player
/// backend (useful after a VPN flap).
pub async fn ipc_reconnect() -> Result<()> {
    match daemon_request(Request::Reconnect).await? {
        ResponseData::Ack { message } => {
            println!("{message}");
            Ok(())
        }
        _ => unexpected_response(),
    }
}

fn unexpected_response<T>() -> Result<T> {
    anyhow::bail!("unexpected response from daemon")
}

fn parse_lyrics_offset(value: &str) -> Result<i64> {
    let raw = value.trim().strip_suffix("ms").unwrap_or(value.trim());
    raw.parse::<i64>()
        .with_context(|| format!("expected offset like +50ms or -200ms, got `{value}`"))
}

fn read_input(path: &Path) -> Result<String> {
    if path == Path::new("-") {
        let mut raw = String::new();
        std::io::stdin().read_to_string(&mut raw)?;
        return Ok(raw);
    }
    std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))
}
