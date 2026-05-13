use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};

use crate::daemon::ipc_client::IpcClient;
use crate::output::{self, OutputFormat};
use crate::protocol::{
    PlaybackCommand, Request, Response, ResponseData, SearchScopeData, SearchSourceData,
    SyncTargetData,
};
use crate::selection;
use crate::spotify::Playback;

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
            uri,
            search,
            format,
        }) => ipc_queue_add(uri, search, format).await,
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
            uri,
            format,
        } => print_mutation(
            daemon_request(Request::PlaylistAddItems {
                playlist,
                uris: vec![uri],
            })
            .await?,
            format,
        ),
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
    uri: Option<String>,
    search: Option<String>,
    format: OutputFormat,
) -> Result<()> {
    match (uri, search) {
        (Some(uri), None) => {
            print_mutation(daemon_request(Request::QueueAdd { uri }).await?, format)
        }
        (None, Some(query)) => {
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
        (Some(_), Some(_)) => anyhow::bail!("provide URI or --search, not both"),
        (None, None) => anyhow::bail!("provide a URI or --search QUERY"),
    }
}

async fn daemon_request(request: Request) -> Result<ResponseData> {
    crate::daemon::server::ensure_daemon_running().await?;
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
