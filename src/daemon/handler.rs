use std::sync::Arc;
use std::time::Instant;

use crate::actions::{self, CommandKind};
use crate::analytics::{now_ms, search_performed_event};
use crate::daemon::state::DaemonState;
use crate::protocol::{
    CommandReceipt, DaemonEvent, PlaybackCommand, PlaylistCreateReceipt, Request, Response,
    ResponseData, SearchScopeData, SearchSourceData,
};
use crate::selection;
use crate::spotify::{MediaItem, MediaKind};

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
            // Phase 6.9: pass the daemon's recent-event snapshot so the
            // report includes RateLimited / AuthError / SchemaCompat
            // findings.
            report: crate::diagnostics::collect_report_with_events(
                state.status(),
                state.event_log_snapshot().await,
            )
            .await?,
        }),
        Request::PlaybackGet => {
            let mut client = state.spotify_client().await?;
            let playback = actions::status(&mut client).await?;
            cache_playback(&state, &playback).await;
            Ok(ResponseData::Playback { playback })
        }
        Request::PlaybackCommand { command } => {
            let mut client = state.spotify_client().await?;
            let action = playback_command_action(&command);
            let command = playback_command_kind(command);
            let result = actions::execute(&mut client, command).await?;
            let message = result.message.clone().unwrap_or_else(|| action.to_string());
            state.emit_event(DaemonEvent::PlaybackChanged {
                action: action.to_string(),
            });
            emit_mutation_finished(&state, action, &message);
            Ok(ResponseData::Mutation {
                receipt: receipt(action, result.message),
            })
        }
        Request::DevicesList => {
            let mut client = state.spotify_client().await?;
            let devices = actions::devices(&mut client).await?;
            cache_devices(&state, &devices).await;
            Ok(ResponseData::Devices { devices })
        }
        Request::DeviceTransfer { device } => {
            let mut client = state.spotify_client().await?;
            let devices = actions::devices(&mut client).await?;
            let device = selection::resolve_device(&devices, &device)?;
            let play = actions::status(&mut client).await?.is_playing;
            let result =
                actions::execute(&mut client, CommandKind::Transfer { device, play }).await?;
            let message = result
                .message
                .clone()
                .unwrap_or_else(|| "transfer".to_string());
            state.emit_event(DaemonEvent::DevicesChanged {
                action: "transfer".to_string(),
            });
            state.emit_event(DaemonEvent::PlaybackChanged {
                action: "transfer".to_string(),
            });
            emit_mutation_finished(&state, "transfer", &message);
            Ok(ResponseData::Mutation {
                receipt: receipt("transfer", result.message),
            })
        }
        Request::Search {
            query,
            scope,
            source,
            limit,
        } => Ok(ResponseData::SearchResults {
            items: search_with_source(state.clone(), query, scope, source, limit).await?,
        }),
        Request::Reindex => Ok(ResponseData::Reindex {
            stats: crate::reindex::reindex(state.store(), state.search()).await?,
        }),
        Request::CacheStatus => {
            let index_documents = state.search().num_docs().await.unwrap_or(0);
            Ok(ResponseData::CacheStatus {
                status: state.store().cache_status(index_documents).await?,
            })
        }
        Request::LibraryList { limit } => Ok(ResponseData::MediaItems {
            items: state.store().list_library_items(limit).await?,
        }),
        Request::LogsTail { lines } => Ok(ResponseData::Logs {
            lines: crate::logging::read_tail(lines)?
                .lines()
                .map(ToString::to_string)
                .collect(),
        }),
        Request::Sync { target } => Ok(ResponseData::Sync {
            summary: crate::sync::sync_target(state.clone(), target).await?,
        }),
        Request::RecentlyPlayed => {
            let mut client = state.spotify_client().await?;
            let items = client.recently_played().await?;
            cache_recent_items(&state, &items).await;
            Ok(ResponseData::MediaItems { items })
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
            let uri_for_event = uri.clone();
            let state_for_event = state.clone();
            let request_summary = format!("{{\"cmd\":\"queue-add\",\"uri\":{:?}}}", uri);
            record_mutation(&state, "queue", &request_summary, async move {
                let mut client = state_for_event.spotify_client().await?;
                let result = actions::execute(
                    &mut client,
                    CommandKind::QueueUri { uri: uri.clone() },
                )
                .await?;
                let message = result
                    .message
                    .clone()
                    .unwrap_or_else(|| "queue".to_string());
                state_for_event.emit_event(DaemonEvent::QueueChanged {
                    action: "queue".to_string(),
                    uris: vec![uri_for_event],
                });
                emit_mutation_finished(&state_for_event, "queue", &message);
                Ok(ResponseData::Mutation {
                    receipt: receipt("queue", result.message),
                })
            })
            .await
        }
        Request::PlaylistsList => {
            let mut client = state.spotify_client().await?;
            let playlists = actions::playlists(&mut client).await?;
            cache_playlists(&state, &playlists).await;
            Ok(ResponseData::Playlists { playlists })
        }
        Request::PlaylistTracks { playlist } => {
            let mut client = state.spotify_client().await?;
            let playlists = actions::playlists(&mut client).await?;
            let playlist = selection::resolve_playlist(&playlists, &playlist)?;
            let items = client.playlist_tracks(&playlist.id).await?;
            cache_playlist_items(&state, &playlist.id, &items).await;
            Ok(ResponseData::MediaItems { items })
        }
        Request::PlaylistAddItems { playlist, uris } => {
            let state_for = state.clone();
            let request_summary =
                format!("{{\"cmd\":\"playlist-add\",\"playlist\":{:?},\"uri_count\":{}}}", playlist, uris.len());
            record_mutation(&state, "playlist-add", &request_summary, async move {
                let mut client = state_for.spotify_client().await?;
                let playlists = actions::playlists(&mut client).await?;
                let playlist = selection::resolve_playlist(&playlists, &playlist)?;
                for uri in &uris {
                    let item = media_item_from_uri(uri)?;
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
                let message = format!("Added items to {}", playlist.name);
                state_for.emit_event(DaemonEvent::PlaylistsChanged {
                    action: "playlist-add".to_string(),
                    playlist: Some(playlist.id.clone()),
                });
                emit_mutation_finished(&state_for, "playlist-add", &message);
                Ok(ResponseData::Mutation {
                    receipt: receipt("playlist-add", Some(message)),
                })
            })
            .await
        }
        Request::PlaylistCreate {
            name,
            description,
            uris,
        } => {
            if uris.is_empty() {
                anyhow::bail!("no resolved track URIs to add");
            }
            for uri in &uris {
                if selection::media_kind_from_uri(uri)? != MediaKind::Track {
                    anyhow::bail!("playlist creation candidates must be track URIs: {uri}");
                }
            }
            let mut client = state.spotify_client().await?;
            let playlist = client
                .create_playlist(&name, description.as_deref(), false)
                .await?;
            client.add_items_to_playlist(&playlist.id, &uris).await?;
            cache_playlists(&state, std::slice::from_ref(&playlist)).await;
            state.emit_event(DaemonEvent::PlaylistsChanged {
                action: "playlist-create".to_string(),
                playlist: Some(playlist.id.clone()),
            });
            emit_mutation_finished(
                &state,
                "playlist-create",
                &format!("Created playlist `{name}` with {} item(s)", uris.len()),
            );
            Ok(ResponseData::PlaylistCreate {
                receipt: PlaylistCreateReceipt {
                    ok: true,
                    action: "playlist-create".to_string(),
                    playlist_uri: selection::playlist_uri(&playlist.id),
                    playlist_id: playlist.id,
                    name: playlist.name,
                    added_item_count: uris.len(),
                    message: format!("Created playlist `{name}` with {} item(s)", uris.len()),
                },
            })
        }
        Request::LibrarySave { uri, current } => {
            let mut client = state.spotify_client().await?;
            let event_uris = uri.iter().cloned().collect::<Vec<_>>();
            let command = if current {
                CommandKind::SaveCurrent
            } else {
                let uri = uri.ok_or_else(|| anyhow::anyhow!("provide uri or current=true"))?;
                CommandKind::SaveItem {
                    item: media_item_from_uri(&uri)?,
                }
            };
            let result = actions::execute(&mut client, command).await?;
            let message = result.message.clone().unwrap_or_else(|| "save".to_string());
            state.emit_event(DaemonEvent::LibraryChanged {
                action: "save".to_string(),
                uris: event_uris,
            });
            emit_mutation_finished(&state, "save", &message);
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

async fn search_with_source(
    state: Arc<DaemonState>,
    query: String,
    scope: SearchScopeData,
    source: SearchSourceData,
    limit: u32,
) -> anyhow::Result<Vec<MediaItem>> {
    match source {
        SearchSourceData::Local => local_cached_search(&state, &query, scope, limit).await,
        SearchSourceData::Spotify => spotify_search_and_cache(state, query, scope, limit).await,
        SearchSourceData::Hybrid => {
            let local = local_cached_search(&state, &query, scope, limit).await?;
            if !local.is_empty() {
                let refresh_state = state.clone();
                let refresh_query = query.clone();
                tokio::spawn(async move {
                    if let Err(err) =
                        spotify_search_and_cache(refresh_state, refresh_query, scope, limit).await
                    {
                        tracing::debug!(error = %err, "background hybrid search refresh failed");
                    }
                });
                return Ok(local);
            }
            spotify_search_and_cache(state, query, scope, limit).await
        }
    }
}

async fn local_cached_search(
    state: &DaemonState,
    query: &str,
    scope: SearchScopeData,
    limit: u32,
) -> anyhow::Result<Vec<MediaItem>> {
    let hits = state
        .search()
        .search(query, scope, limit as usize)
        .await
        .unwrap_or_default();
    if !hits.is_empty() {
        let uris = hits.into_iter().map(|hit| hit.uri).collect::<Vec<_>>();
        let items = state.store().media_items_by_uris(&uris).await?;
        if !items.is_empty() {
            return Ok(items);
        }
    }
    state.store().local_search(query, scope, limit).await
}

async fn spotify_search_and_cache(
    state: Arc<DaemonState>,
    query: String,
    scope: SearchScopeData,
    limit: u32,
) -> anyhow::Result<Vec<MediaItem>> {
    let mut client = state.spotify_client().await?;
    let kinds = scope_media_kinds(scope);
    let started = Instant::now();
    let mut items = client
        .search_with_limit(&query, &kinds, limit as u8)
        .await?;
    client
        .record_analytics_event(search_performed_event(
            client.analytics_source(),
            &query,
            items.len(),
            started.elapsed().as_millis(),
            now_ms(),
        ))
        .await;
    for item in &mut items {
        item.source = Some("spotify".to_string());
        item.freshness = Some("fresh".to_string());
    }
    state
        .store()
        .cache_search_results(&query, scope, SearchSourceData::Spotify, &items)
        .await?;
    state.emit_event(DaemonEvent::SearchUpdated {
        query: query.clone(),
        count: items.len(),
    });
    let entries = items
        .iter()
        .cloned()
        .map(|item| crate::store::IndexedMediaItem {
            item,
            liked: false,
            saved: false,
            added_at_ms: Some(crate::store::now_ms()),
            source: "spotify".to_string(),
        })
        .collect();
    if let Err(err) = state
        .search()
        .apply_batch(crate::search::SearchUpdateBatch {
            entries,
            removed_uris: Vec::new(),
        })
        .await
    {
        tracing::warn!(error = %err, "failed to update search index from Spotify results");
    }
    Ok(items)
}

fn scope_media_kinds(scope: SearchScopeData) -> Vec<MediaKind> {
    match scope {
        SearchScopeData::All => vec![
            MediaKind::Track,
            MediaKind::Episode,
            MediaKind::Album,
            MediaKind::Artist,
            MediaKind::Playlist,
        ],
        SearchScopeData::Track => vec![MediaKind::Track],
        SearchScopeData::Episode => vec![MediaKind::Episode],
        SearchScopeData::Album => vec![MediaKind::Album],
        SearchScopeData::Artist => vec![MediaKind::Artist],
        SearchScopeData::Playlist => vec![MediaKind::Playlist],
    }
}

async fn cache_playback(state: &DaemonState, playback: &crate::spotify::Playback) {
    if let Err(err) = state.store().persist_playback(playback).await {
        tracing::warn!(error = %err, "failed to cache playback snapshot");
    }
}

async fn cache_devices(state: &DaemonState, devices: &[crate::spotify::Device]) {
    if let Err(err) = state.store().persist_devices(devices).await {
        tracing::warn!(error = %err, "failed to cache devices");
    }
}

async fn cache_recent_items(state: &DaemonState, items: &[MediaItem]) {
    if let Err(err) = state.store().persist_recent_items(items).await {
        tracing::warn!(error = %err, "failed to cache recent items");
    }
}

async fn cache_playlists(state: &DaemonState, playlists: &[crate::spotify::Playlist]) {
    if let Err(err) = state.store().persist_playlists(playlists).await {
        tracing::warn!(error = %err, "failed to cache playlists");
    }
}

async fn cache_playlist_items(state: &DaemonState, playlist_id: &str, items: &[MediaItem]) {
    if let Err(err) = state
        .store()
        .persist_playlist_items(playlist_id, items)
        .await
    {
        tracing::warn!(error = %err, "failed to cache playlist items");
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

fn emit_mutation_finished(state: &DaemonState, action: &str, message: &str) {
    state.emit_event(DaemonEvent::MutationFinished {
        action: action.to_string(),
        message: message.to_string(),
    });
}

/// Phase 6.6 -- record a pending receipt + emit MutationAccepted, then
/// finalize after the body runs. Best-effort: if the receipts table is
/// unavailable for any reason we still execute the mutation and emit
/// the legacy MutationFinished event, so existing call sites keep
/// working. Returns the body's result unchanged.
async fn record_mutation<T>(
    state: &std::sync::Arc<DaemonState>,
    action: &str,
    request_summary: &str,
    body: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    let receipt_id = spotuify_protocol::ReceiptId::new_v7();
    let started = crate::analytics::now_ms();
    let receipt = spotuify_protocol::Receipt {
        receipt_id,
        action: action.to_string(),
        status: spotuify_protocol::ReceiptStatus::Pending,
        message: "queued".to_string(),
        started_at_ms: started,
        finished_at_ms: None,
        error: None,
    };
    let _ = state
        .store()
        .insert_pending_receipt(&receipt, request_summary)
        .await;
    state.emit_event(spotuify_protocol::DaemonEvent::MutationAccepted {
        receipt_id,
        action: action.to_string(),
    });

    let result = body.await;
    let finished = crate::analytics::now_ms();
    let (status, message, error_summary) = match &result {
        Ok(_) => (
            spotuify_protocol::ReceiptStatus::Confirmed,
            format!("{action} confirmed"),
            None,
        ),
        Err(err) => {
            let msg = err.to_string();
            (
                spotuify_protocol::ReceiptStatus::Failed,
                msg.clone(),
                Some(spotuify_protocol::ApiErrorSummary {
                    kind: spotuify_protocol::IpcErrorKind::Provider,
                    message: msg,
                    retry_after_secs: None,
                }),
            )
        }
    };
    let _ = state
        .store()
        .finalize_receipt(
            receipt_id,
            status,
            &message,
            finished,
            error_summary.as_ref(),
        )
        .await;
    state.emit_event(spotuify_protocol::DaemonEvent::MutationFinalized {
        receipt_id,
        status,
        message: message.clone(),
    });
    result
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
        source: None,
        freshness: None,
        explicit: None,
        is_playable: None,
    })
}
