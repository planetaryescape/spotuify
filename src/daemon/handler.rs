use std::sync::Arc;

use crate::actions::{self, CommandKind, SearchScope};
use crate::daemon::state::DaemonState;
use crate::protocol::{
    CommandReceipt, PlaybackCommand, Request, Response, ResponseData, SearchScopeData,
};
use crate::selection;
use crate::spotify::MediaItem;

pub(crate) async fn handle_request(state: Arc<DaemonState>, request: Request) -> Response {
    match dispatch(state, request).await {
        Ok(data) => Response::Ok { data },
        Err(err) => Response::error(err.to_string()),
    }
}

async fn dispatch(state: Arc<DaemonState>, request: Request) -> anyhow::Result<ResponseData> {
    match request {
        Request::Ping => Ok(ResponseData::Pong),
        Request::GetDaemonStatus => Ok(ResponseData::DaemonStatus {
            status: state.status(),
        }),
        Request::GetDoctorReport => Ok(ResponseData::DoctorReport {
            report: crate::diagnostics::collect_report(state.status()).await?,
        }),
        Request::PlaybackGet => {
            let mut client = state.spotify_client().await?;
            Ok(ResponseData::Playback {
                playback: actions::status(&mut client).await?,
            })
        }
        Request::PlaybackCommand { command } => {
            let mut client = state.spotify_client().await?;
            let action = playback_command_action(&command);
            let command = playback_command_kind(command);
            let result = actions::execute(&mut client, command).await?;
            Ok(ResponseData::Mutation {
                receipt: receipt(action, result.message),
            })
        }
        Request::DevicesList => {
            let mut client = state.spotify_client().await?;
            Ok(ResponseData::Devices {
                devices: actions::devices(&mut client).await?,
            })
        }
        Request::DeviceTransfer { device } => {
            let mut client = state.spotify_client().await?;
            let devices = actions::devices(&mut client).await?;
            let device = selection::resolve_device(&devices, &device)?;
            let play = actions::status(&mut client).await?.is_playing;
            let result =
                actions::execute(&mut client, CommandKind::Transfer { device, play }).await?;
            Ok(ResponseData::Mutation {
                receipt: receipt("transfer", result.message),
            })
        }
        Request::Search { query, scope } => {
            let mut client = state.spotify_client().await?;
            Ok(ResponseData::SearchResults {
                items: actions::search(&mut client, &query, scope.into()).await?,
            })
        }
        Request::RecentlyPlayed => {
            let mut client = state.spotify_client().await?;
            Ok(ResponseData::MediaItems {
                items: client.recently_played().await?,
            })
        }
        Request::Image { url } => {
            let client = state.spotify_client().await?;
            Ok(ResponseData::Image {
                bytes: client.image(&url).await?,
            })
        }
        Request::QueueGet => {
            let mut client = state.spotify_client().await?;
            Ok(ResponseData::Queue {
                queue: actions::queue(&mut client).await?,
            })
        }
        Request::QueueAdd { uri } => {
            let mut client = state.spotify_client().await?;
            let result = actions::execute(&mut client, CommandKind::QueueUri { uri }).await?;
            Ok(ResponseData::Mutation {
                receipt: receipt("queue", result.message),
            })
        }
        Request::PlaylistsList => {
            let mut client = state.spotify_client().await?;
            Ok(ResponseData::Playlists {
                playlists: actions::playlists(&mut client).await?,
            })
        }
        Request::PlaylistTracks { playlist } => {
            let mut client = state.spotify_client().await?;
            let playlists = actions::playlists(&mut client).await?;
            let playlist = selection::resolve_playlist(&playlists, &playlist)?;
            Ok(ResponseData::MediaItems {
                items: client.playlist_tracks(&playlist.id).await?,
            })
        }
        Request::PlaylistAddItems { playlist, uris } => {
            let mut client = state.spotify_client().await?;
            let playlists = actions::playlists(&mut client).await?;
            let playlist = selection::resolve_playlist(&playlists, &playlist)?;
            for uri in uris {
                let item = media_item_from_uri(&uri)?;
                actions::execute(
                    &mut client,
                    CommandKind::AddToPlaylist {
                        item,
                        playlist_id: playlist.id.clone(),
                        playlist_name: playlist.name.clone(),
                    },
                )
                .await?;
            }
            Ok(ResponseData::Mutation {
                receipt: receipt(
                    "playlist-add",
                    Some(format!("Added items to {}", playlist.name)),
                ),
            })
        }
        Request::LibrarySave { uri, current } => {
            let mut client = state.spotify_client().await?;
            let command = if current {
                CommandKind::SaveCurrent
            } else {
                let uri = uri.ok_or_else(|| anyhow::anyhow!("provide uri or current=true"))?;
                CommandKind::SaveItem {
                    item: media_item_from_uri(&uri)?,
                }
            };
            let result = actions::execute(&mut client, command).await?;
            Ok(ResponseData::Mutation {
                receipt: receipt("save", result.message),
            })
        }
        Request::Shutdown => {
            state.request_shutdown();
            Ok(ResponseData::Shutdown)
        }
    }
}

impl From<SearchScopeData> for SearchScope {
    fn from(scope: SearchScopeData) -> Self {
        match scope {
            SearchScopeData::All => Self::All,
            SearchScopeData::Track => Self::Track,
            SearchScopeData::Episode => Self::Episode,
            SearchScopeData::Album => Self::Album,
            SearchScopeData::Artist => Self::Artist,
            SearchScopeData::Playlist => Self::Playlist,
        }
    }
}

fn playback_command_kind(command: PlaybackCommand) -> CommandKind {
    match command {
        PlaybackCommand::Pause => CommandKind::Pause,
        PlaybackCommand::Resume => CommandKind::Resume,
        PlaybackCommand::Toggle => CommandKind::TogglePlayback,
        PlaybackCommand::Next => CommandKind::Next,
        PlaybackCommand::Previous => CommandKind::Previous,
        PlaybackCommand::PlayUri { uri } => CommandKind::PlayUri { uri },
        PlaybackCommand::Seek { position_ms } => CommandKind::Seek { position_ms },
        PlaybackCommand::Volume { volume_percent } => CommandKind::Volume { volume_percent },
        PlaybackCommand::Shuffle { state } => CommandKind::Shuffle { state },
        PlaybackCommand::Repeat { state } => CommandKind::Repeat { state },
    }
}

fn playback_command_action(command: &PlaybackCommand) -> &'static str {
    match command {
        PlaybackCommand::Pause => "pause",
        PlaybackCommand::Resume => "resume",
        PlaybackCommand::Toggle => "toggle",
        PlaybackCommand::Next => "next",
        PlaybackCommand::Previous => "previous",
        PlaybackCommand::PlayUri { .. } => "play-uri",
        PlaybackCommand::Seek { .. } => "seek",
        PlaybackCommand::Volume { .. } => "volume",
        PlaybackCommand::Shuffle { .. } => "shuffle",
        PlaybackCommand::Repeat { .. } => "repeat",
    }
}

fn receipt(action: &str, message: Option<String>) -> CommandReceipt {
    CommandReceipt {
        ok: true,
        action: action.to_string(),
        message: message.unwrap_or_else(|| action.to_string()),
    }
}

fn media_item_from_uri(uri: &str) -> anyhow::Result<MediaItem> {
    let kind = selection::media_kind_from_uri(uri)?;
    let id = uri.rsplit(':').next().map(str::to_string);
    Ok(MediaItem {
        id,
        uri: uri.to_string(),
        name: uri.to_string(),
        subtitle: String::new(),
        context: String::new(),
        duration_ms: 0,
        image_url: None,
        kind,
    })
}
