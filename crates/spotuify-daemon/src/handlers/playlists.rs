//! `playlists` request handlers (split out of the dispatch god-function).

use std::sync::Arc;

use spotuify_protocol::{
    DaemonEvent, OperationKind, OperationSource, PlaylistCreateReceipt, Request, ResponseData,
};
use spotuify_spotify::actions::{self, CommandKind};
use spotuify_spotify::client::MediaKind;
use spotuify_spotify::selection;

use crate::handler::*;
use crate::state::DaemonState;

pub(crate) async fn dispatch(
    state: Arc<DaemonState>,
    request: Request,
    source: Option<OperationSource>,
) -> anyhow::Result<ResponseData> {
    let operation_source = source.unwrap_or(OperationSource::DaemonInternal);
    let request_json = serde_json::to_string(&request).unwrap_or_else(|_| "{}".to_string());
    let mutation_lane = state.mutation_lane(&request).await;
    match request {
        Request::PlaylistsList => {
            // Cache hit returns immediately + schedules refresh; cache
            // miss falls through to a blocking Spotify call. Unlike
            // PlaybackGet, PlaylistsList is used by the CLI's
            // playlist-name resolver (`playlist add <name>`) as an
            // authoritative lookup — an empty list there means
            // "unknown playlist", not "still loading", which is the
            // wrong UX for a synchronous one-shot CLI invocation. The
            // TUI's playlists page isn't latency-critical the way the
            // now-playing card is, so paying the round-trip on the
            // very first launch is the right trade.
            let cached = state.store().list_playlists(500).await?;
            if !cached.is_empty() {
                spawn_playlists_refresh(state.clone());
                return Ok(ResponseData::Playlists { playlists: cached });
            }
            let mut client = state.spotify_client().await?;
            let playlists = actions::playlists(&mut client).await?;
            cache_playlists(&state, &playlists).await;
            Ok(ResponseData::Playlists { playlists })
        }
        Request::PlaylistTracks { playlist, wait } => {
            // Serve cached tracks when present, but don't turn an
            // empty cache into a false "this playlist has no tracks".
            // TUI calls use wait=false so first-load paging happens in
            // the background and completes via PlaylistsChanged. CLI/MCP
            // calls use wait=true for authoritative one-shot output.
            let cached_playlists = state.store().list_playlists(500).await?;
            let items =
                if let Ok(resolved) = selection::resolve_playlist(&cached_playlists, &playlist) {
                    let cached_items = state.store().playlist_items(&resolved.id, 500).await?;
                    if !cached_items.is_empty() {
                        cached_items
                    } else if !wait {
                        reject_if_auth_blocked(&state)?;
                        spawn_playlist_tracks_refresh(state.clone(), resolved.id.clone());
                        Vec::new()
                    } else {
                        reject_if_auth_blocked(&state)?;
                        let mut client = state.spotify_client().await?;
                        let items = match client.playlist_tracks(&resolved.id).await {
                            Ok(items) => items,
                            Err(err) => {
                                if is_playlist_tracks_forbidden(&err) {
                                    let _ = state
                                        .store()
                                        .mark_playlist_tracks_inaccessible(&resolved.id)
                                        .await;
                                    state.emit_event(DaemonEvent::PlaylistsChanged {
                                        action: "tracks-inaccessible".to_string(),
                                        playlist: Some(resolved.id.clone()),
                                    });
                                }
                                return Err(err.into());
                            }
                        };
                        cache_playlist_items(&state, &resolved.id, &items).await;
                        items
                    }
                } else {
                    reject_if_auth_blocked(&state)?;
                    spawn_playlists_refresh(state.clone());
                    Vec::new()
                };
            Ok(ResponseData::MediaItems { items })
        }
        Request::PlaylistAddItems { playlist, uris } => {
            let state_for = state.clone();
            let subject_uris = uris.clone();
            spawn_optimistic_mutation(
                &state,
                OperationKind::PlaylistAdd,
                operation_source,
                subject_uris,
                "playlist-add",
                request_json.clone(),
                // Initial values are placeholders; the body captures the
                // resolved playlist's snapshot_id from the same
                // `actions::playlists()` call it already makes for
                // resolution and writes the real plan via
                // `update_operation_plan`.
                None,
                None,
                mutation_lane,
                move |op_id| async move {
                    let mut client = state_for.spotify_client().await?;
                    let playlists = actions::playlists(&mut client).await?;
                    let resolved = selection::resolve_playlist(&playlists, &playlist)?;
                    let snapshot_id = resolved.snapshot_id.clone();
                    let pre_state = spotuify_protocol::PreState::PlaylistAdd {
                        playlist_id: resolved.id.clone(),
                        snapshot_id: snapshot_id.clone(),
                        added_uris: uris.clone(),
                    };
                    let plan = spotuify_protocol::ReversalPlan::PlaylistRemoveTracks {
                        playlist_id: resolved.id.clone(),
                        uris: uris.clone(),
                        snapshot_id,
                    };
                    if let Err(err) = state_for
                        .store()
                        .update_operation_plan(op_id, Some(&pre_state), Some(&plan))
                        .await
                    {
                        tracing::warn!(error = %err, "failed to persist playlist_add pre-state");
                    }
                    for uri in &uris {
                        let item = media_item_from_uri(uri)?;
                        actions::execute(
                            &mut client,
                            CommandKind::AddToPlaylist {
                                item,
                                playlist_id: resolved.id.clone(),
                                playlist_name: resolved.name.clone(),
                            },
                        )
                        .await?;
                    }
                    let message = format!("Added items to {}", resolved.name);
                    state_for.emit_event(DaemonEvent::PlaylistsChanged {
                        action: "playlist-add".to_string(),
                        playlist: Some(resolved.id.clone()),
                    });
                    emit_mutation_finished(&state_for, "playlist-add", &message);
                    Ok(())
                },
            )
            .await
        }
        Request::PlaylistRemoveItems { playlist, uris } => {
            if uris.is_empty() {
                anyhow::bail!("no track URIs to remove");
            }
            let state_for = state.clone();
            let subject_uris = uris.clone();
            spawn_optimistic_mutation(
                &state,
                OperationKind::PlaylistRemove,
                operation_source,
                subject_uris,
                "playlist-remove",
                request_json.clone(),
                None,
                None,
                mutation_lane,
                move |op_id| async move {
                    let mut client = state_for.spotify_client().await?;
                    let playlists = actions::playlists(&mut client).await?;
                    let resolved = selection::resolve_playlist(&playlists, &playlist)?;
                    let snapshot_id = resolved.snapshot_id.clone();
                    let current_items = client
                        .playlist_tracks(&resolved.id)
                        .await
                        .unwrap_or_default();
                    let removed_items = current_items
                        .iter()
                        .enumerate()
                        .filter(|(_, item)| uris.iter().any(|uri| uri == &item.uri))
                        .map(|(position, item)| (item.uri.clone(), position as u32))
                        .collect::<Vec<_>>();
                    let pre_state = spotuify_protocol::PreState::PlaylistRemove {
                        playlist_id: resolved.id.clone(),
                        snapshot_id: snapshot_id.clone(),
                        removed_items: removed_items.clone(),
                    };
                    let plan = spotuify_protocol::ReversalPlan::PlaylistAddAtPositions {
                        playlist_id: resolved.id.clone(),
                        items: removed_items,
                        snapshot_id,
                    };
                    if let Err(err) = state_for
                        .store()
                        .update_operation_plan(op_id, Some(&pre_state), Some(&plan))
                        .await
                    {
                        tracing::warn!(error = %err, "failed to persist playlist_remove pre-state");
                    }
                    client
                        .remove_playlist_items(&resolved.id, &uris, resolved.snapshot_id.as_deref())
                        .await?;
                    let message = format!("Removed {} item(s) from {}", uris.len(), resolved.name);
                    state_for.emit_event(DaemonEvent::PlaylistsChanged {
                        action: "playlist-remove".to_string(),
                        playlist: Some(resolved.id.clone()),
                    });
                    emit_mutation_finished(&state_for, "playlist-remove", &message);
                    Ok(())
                },
            )
            .await
        }
        Request::PlaylistCreate {
            name,
            description,
            uris,
        } => {
            let _mutation_guard = match mutation_lane {
                Some(lane) => Some(lane.lock_owned().await),
                None => None,
            };
            if uris.is_empty() {
                anyhow::bail!("no resolved track URIs to add");
            }
            for uri in &uris {
                if selection::media_kind_from_uri(uri)? != MediaKind::Track {
                    anyhow::bail!("playlist creation candidates must be track URIs: {uri}");
                }
            }
            let request_summary = request_json.clone();
            let state_for = state.clone();
            let name_for = name.clone();
            let description_for = description.clone();
            let uris_for = uris.clone();
            record_operation(
                &state,
                OperationKind::PlaylistCreate,
                operation_source,
                vec![],
                "playlist-create",
                &request_summary,
                None,
                None,
                move |op_id| async move {
                    let mut client = state_for.spotify_client().await?;
                    let playlist = client
                        .create_playlist(&name_for, description_for.as_deref(), false)
                        .await?;
                    let playlist_uri = selection::playlist_uri(&playlist.id);
                    let pre_state = spotuify_protocol::PreState::PlaylistCreate {
                        playlist_id: playlist.id.clone(),
                    };
                    let plan = spotuify_protocol::ReversalPlan::PlaylistDelete {
                        playlist_id: playlist.id.clone(),
                    };
                    if let Err(err) = state_for
                        .store()
                        .update_operation_plan(op_id, Some(&pre_state), Some(&plan))
                        .await
                    {
                        tracing::warn!(error = %err, "failed to persist playlist_create pre-state");
                    }
                    if let Err(err) = state_for
                        .store()
                        .update_operation_subject_uris(op_id, std::slice::from_ref(&playlist_uri))
                        .await
                    {
                        tracing::warn!(error = %err, "failed to persist playlist_create subject uri");
                    }
                    client
                        .add_items_to_playlist(&playlist.id, &uris_for)
                        .await?;
                    cache_playlists(&state_for, std::slice::from_ref(&playlist)).await;
                    state_for.emit_event(DaemonEvent::PlaylistsChanged {
                        action: "playlist-create".to_string(),
                        playlist: Some(playlist.id.clone()),
                    });
                    let message =
                        format!("Created playlist `{name_for}` with {} item(s)", uris_for.len());
                    emit_mutation_finished(&state_for, "playlist-create", &message);
                    Ok(ResponseData::PlaylistCreate {
                        receipt: PlaylistCreateReceipt {
                            ok: true,
                            action: "playlist-create".to_string(),
                            playlist_uri,
                            playlist_id: playlist.id,
                            name: playlist.name,
                            added_item_count: uris_for.len(),
                            message,
                        },
                    })
                },
            )
            .await
        }
        Request::PlaylistUnfollow { playlist } => {
            let state_for = state.clone();
            let playlist_for = playlist.clone();
            let playlist_uri = selection::playlist_uri(&playlist);
            spawn_optimistic_mutation(
                &state,
                OperationKind::PlaylistUnfollow,
                operation_source,
                vec![playlist_uri.clone()],
                "playlist-unfollow",
                request_json.clone(),
                None,
                None,
                mutation_lane,
                move |_op_id| async move {
                    let mut client = state_for.spotify_client().await?;
                    client.unfollow_playlist(&playlist_for).await?;
                    let message = format!("Unfollowed playlist {playlist_for}");
                    state_for.emit_event(DaemonEvent::PlaylistsChanged {
                        action: "playlist-unfollow".to_string(),
                        playlist: Some(playlist_for.clone()),
                    });
                    emit_mutation_finished(&state_for, "playlist-unfollow", &message);
                    Ok(())
                },
            )
            .await
        }
        Request::PlaylistSetImage {
            playlist,
            image_base64,
        } => {
            // Spotify caps the base64-encoded body at 256 KB. The CLI
            // checks too, but a fast bail here protects MCP callers and
            // any future direct-IPC clients.
            const MAX_IMAGE_BASE64_BYTES: usize = 256 * 1024;
            if image_base64.is_empty() {
                anyhow::bail!("playlist-set-image: image_base64 is empty");
            }
            if image_base64.len() > MAX_IMAGE_BASE64_BYTES {
                anyhow::bail!(
                    "playlist-set-image: encoded image is {} bytes, exceeds Spotify's 256 KB cap",
                    image_base64.len()
                );
            }
            let state_for = state.clone();
            let playlist_for = playlist.clone();
            let playlist_uri = selection::playlist_uri(&playlist);
            let image_for = image_base64.clone();
            spawn_optimistic_mutation(
                &state,
                OperationKind::PlaylistSetImage,
                operation_source,
                vec![playlist_uri.clone()],
                "playlist-set-image",
                request_json.clone(),
                None,
                None,
                mutation_lane,
                move |_op_id| async move {
                    let mut client = state_for.spotify_client().await?;
                    client.set_playlist_image(&playlist_for, &image_for).await?;
                    let message = format!("Updated cover for playlist {playlist_for}");
                    state_for.emit_event(DaemonEvent::PlaylistsChanged {
                        action: "playlist-set-image".to_string(),
                        playlist: Some(playlist_for.clone()),
                    });
                    emit_mutation_finished(&state_for, "playlist-set-image", &message);
                    Ok(())
                },
            )
            .await
        }
        _ => unreachable!("non-playlists request routed to playlists dispatcher"),
    }
}
