use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use spotuify_core::{Playback, Playlist};
use spotuify_protocol::{
    IpcClient, PlaybackCommand, Request, Response, ResponseData, SearchScopeData,
    SearchSourceData, SyncTargetData,
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

pub async fn ipc_search(
    query: &str,
    scope: SearchScopeData,
    source: SearchSourceData,
    limit: u32,
    play: bool,
    index: usize,
    format: OutputFormat,
) -> Result<()> {
    let items = match daemon_request(Request::Search {
        query: query.to_string(),
        scope,
        source,
        limit,
    })
    .await?
    {
        ResponseData::SearchResults { items } => items,
        _ => return unexpected_response(),
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
            source: SearchSourceData::Hybrid,
            limit: 10,
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
    ipc_search(query, scope, SearchSourceData::Hybrid, 10, true, 1, format).await
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

pub async fn ipc_sync(target: SyncTargetData, format: OutputFormat) -> Result<()> {
    match daemon_request(Request::Sync { target }).await? {
        ResponseData::Sync { summary } => output::print_sync_summary(&summary, format),
        _ => unexpected_response(),
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
            match daemon_request(Request::PlaylistTracks { playlist }).await? {
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
                source: SearchSourceData::Hybrid,
                limit: 10,
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
    selection::resolve_playlist(&playlists, value)
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
    // IpcClient::connect surfaces a clear "spotuify daemon start" error
    // when the socket isn't reachable. The binary's main.rs is
    // responsible for autostarting the daemon when appropriate.
    let mut client = IpcClient::connect().await?;
    match client.request(request).await? {
        Response::Ok { data } => Ok(data),
        Response::Error { message, .. } => anyhow::bail!(message),
    }
}

fn unexpected_response<T>() -> Result<T> {
    anyhow::bail!("unexpected response from daemon")
}

fn read_input(path: &Path) -> Result<String> {
    if path == Path::new("-") {
        let mut raw = String::new();
        std::io::stdin().read_to_string(&mut raw)?;
        return Ok(raw);
    }
    std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))
}
