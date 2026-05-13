use anyhow::{Context, Result};

use crate::daemon::ipc_client::IpcClient;
use crate::output::{self, OutputFormat};
use crate::protocol::{PlaybackCommand, Request, Response, ResponseData, SearchScopeData};
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
    play: bool,
    index: usize,
    format: OutputFormat,
) -> Result<()> {
    let items = match daemon_request(Request::Search {
        query: query.to_string(),
        scope,
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

pub async fn ipc_play_query(
    query: &str,
    scope: SearchScopeData,
    format: OutputFormat,
) -> Result<()> {
    ipc_search(query, scope, true, 1, format).await
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

pub async fn ipc_save_current(action: &str, format: OutputFormat) -> Result<()> {
    let data = daemon_request(Request::LibrarySave {
        uri: None,
        current: true,
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
