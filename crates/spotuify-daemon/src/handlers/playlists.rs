//! `playlists` request handlers (split out of the dispatch god-function).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine as _;
use spotuify_core::{
    AccessOutcome, CollectionRequest, MediaItem, MediaKind, MusicProvider, Mutation,
    MutationOutcome, MutationReceipt, PageRequest, Playlist, PlaylistInsertion, PlaylistItemRef,
    ProviderError, RequestContext, ResourceUri,
};
use spotuify_protocol::{
    DaemonEvent, MutationId, OperationKind, OperationSource, PlaylistCreateReceipt,
    PlaylistItemMutationAction, Request, ResponseData,
};
use uuid::Uuid;

use crate::handler::*;
use crate::state::DaemonState;

#[cfg(not(test))]
const PLAYLIST_REMOVE_PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(20);
#[cfg(test)]
const PLAYLIST_REMOVE_PREFLIGHT_TIMEOUT: Duration = Duration::from_millis(250);

#[cfg(test)]
static FAIL_NEXT_PLAYLIST_REMOVE_PLAN_ACTIVATION: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static PLAYLIST_REMOVE_PRESTATE_OBSERVED_BEFORE_APPLY: AtomicBool = AtomicBool::new(false);

struct PlaylistRemovePlan {
    provider: Arc<dyn MusicProvider>,
    resolved: Playlist,
    playlist_uri: ResourceUri,
    mutation_items: Vec<PlaylistItemRef>,
    removed_items: Vec<(String, u32)>,
}

impl PlaylistRemovePlan {
    fn mutation(&self) -> Mutation {
        Mutation::PlaylistRemove {
            playlist_uri: self.playlist_uri.clone(),
            items: self.mutation_items.clone(),
            expected_version: self.resolved.version_token.clone(),
        }
    }
}

pub(crate) async fn dispatch(
    state: Arc<DaemonState>,
    request: Request,
    source: Option<OperationSource>,
    mutation_id: Option<MutationId>,
) -> anyhow::Result<ResponseData> {
    let operation_source = source.unwrap_or(OperationSource::DaemonInternal);
    let request_json = serde_json::to_string(&request).unwrap_or_else(|_| "{}".to_string());
    let mutation_lane = state.mutation_lane(&request).await;
    match request {
        Request::PlaylistsList { provider } => {
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
            let (provider_id, provider) = state.provider_or_default(provider.as_ref()).await?;
            let cached = state
                .store()
                .list_provider_playlists(500, Some(&provider_id))
                .await?;
            if !cached.is_empty() {
                spawn_provider_playlists_refresh(state.clone(), provider_id);
                return Ok(ResponseData::Playlists { playlists: cached });
            }
            let playlists = collect_playlists(provider.clone(), RequestContext::FOREGROUND).await?;
            let playlists = normalize_provider_playlist_list(provider.as_ref(), playlists)?;
            cache_playlists(&state, &provider_id, &playlists).await;
            Ok(ResponseData::Playlists { playlists })
        }
        Request::PlaylistTracks {
            playlist,
            wait,
            provider,
        } => {
            // Serve cached tracks when present, but don't turn an
            // empty cache into a false "this playlist has no tracks".
            // TUI calls use wait=false so first-load paging happens in
            // the background and completes via PlaylistsChanged. CLI/MCP
            // calls use wait=true for authoritative one-shot output.
            let (provider_id, _) =
                resolve_playlist_provider(&state, &playlist, provider.as_ref()).await?;
            if !wait {
                let cached_playlists = state
                    .store()
                    .list_provider_playlists(500, Some(&provider_id))
                    .await?;
                let cached_playlist = resolve_playlist(&cached_playlists, &playlist).ok();
                if let Some(resolved) = cached_playlist.as_ref() {
                    let cached_items = state
                        .store()
                        .playlist_items_for_provider(&resolved.id, 500, Some(&provider_id))
                        .await?;
                    if !cached_items.is_empty() {
                        return Ok(ResponseData::MediaItems {
                            items: cached_items,
                        });
                    }
                }
                reject_if_auth_blocked(&state, Some(&provider_id)).await?;
                spawn_provider_playlist_items_refresh(state.clone(), playlist, provider_id);
                return Ok(ResponseData::MediaItems { items: Vec::new() });
            }

            reject_if_auth_blocked(&state, Some(&provider_id)).await?;
            let (selected_provider, resolved, playlist_uri) = resolve_provider_playlist(
                &state,
                &playlist,
                provider.as_ref(),
                RequestContext::FOREGROUND,
            )
            .await?;
            cache_playlists(&state, &provider_id, std::slice::from_ref(&resolved)).await;
            let items = match collect_playlist_items(
                selected_provider.clone(),
                playlist_uri.clone(),
                RequestContext::FOREGROUND,
            )
            .await?
            {
                AccessOutcome::Available(items) => items,
                AccessOutcome::Unavailable(reason) => {
                    let observed_version_token = resolve_provider_playlist_version(
                        selected_provider,
                        &playlist_uri,
                        resolved.version_token.clone(),
                    )
                    .await;
                    let _ = state
                        .store()
                        .mark_playlist_tracks_inaccessible_at_version(
                            &resolved.id,
                            observed_version_token.as_deref(),
                        )
                        .await;
                    state.emit_event(DaemonEvent::PlaylistsChanged {
                        action: "tracks-inaccessible".to_string(),
                        playlist: Some(resolved.id.clone()),
                        provider: Some(provider_id),
                    });
                    return Err(ProviderError::Forbidden {
                        operation: format!(
                            "read playlist {} items ({reason:?})",
                            playlist_uri.as_uri()
                        ),
                    }
                    .into());
                }
            };
            cache_playlist_items(&state, &provider_id, &resolved.id, &items).await;
            Ok(ResponseData::MediaItems { items })
        }
        Request::PlaylistAddItems {
            playlist,
            uris,
            provider,
        } => {
            // A completed mutation must remain replayable even when the
            // playlist has since disappeared or become unreadable. Check the
            // durable claim before provider discovery/preflight; the atomic
            // claim inside `spawn_optimistic_mutation` still arbitrates a
            // concurrent first execution.
            if let Some(replay) =
                replay_existing_optimistic_mutation(&state, mutation_id, &request_json).await?
            {
                return Ok(replay);
            }
            let (selected_provider, resolved, playlist_uri, items) =
                preflight_playlist_add_request(&state, &playlist, &uris, provider.as_ref()).await?;
            let state_for = state.clone();
            let subject_uris = uris.clone();
            let provider_mutation_id = mutation_id.map_or_else(Uuid::now_v7, |id| id.0);
            spawn_optimistic_mutation(
                &state,
                OperationKind::PlaylistAdd,
                operation_source,
                subject_uris,
                "playlist-add",
                request_json.clone(),
                // Initial values are placeholders; synchronous preflight has
                // already resolved the authoritative provider playlist. The
                // body persists that captured version in the real plan.
                None,
                None,
                mutation_lane,
                mutation_id,
                move |op_id| async move {
                    let provider_id = selected_provider.id().clone();
                    let version_token = resolved.version_token.clone();
                    let pre_state = spotuify_protocol::PreState::PlaylistAdd {
                        playlist_id: resolved.id.clone(),
                        version_token: version_token.clone(),
                        added_uris: uris.clone(),
                    };
                    let plan = spotuify_protocol::ReversalPlan::PlaylistRemoveTracks {
                        playlist_id: resolved.id.clone(),
                        uris: uris.clone(),
                        version_token,
                    };
                    state_for
                        .store()
                        .update_operation_plan(op_id, Some(&pre_state), Some(&plan))
                        .await?;
                    apply_provider_mutation(
                        selected_provider,
                        provider_mutation_id,
                        Mutation::PlaylistAdd {
                            playlist_uri,
                            items,
                            expected_version: resolved.version_token.clone(),
                        },
                    )
                    .await?;
                    let message = format!("Added items to {}", resolved.name);
                    state_for.emit_event(DaemonEvent::PlaylistsChanged {
                        action: "playlist-add".to_string(),
                        playlist: Some(resolved.id.clone()),
                        provider: Some(provider_id),
                    });
                    emit_mutation_finished(&state_for, "playlist-add", &message);
                    Ok(())
                },
            )
            .await
        }
        Request::PlaylistItemsPreview {
            playlist,
            uris,
            action,
            provider,
        } => {
            let resolved = match action {
                PlaylistItemMutationAction::Add => {
                    let (_, resolved, _, _) =
                        preflight_playlist_add_request(&state, &playlist, &uris, provider.as_ref())
                            .await?;
                    resolved
                }
                PlaylistItemMutationAction::Remove => {
                    let plan =
                        build_playlist_remove_plan(&state, &playlist, &uris, provider.as_ref())
                            .await?;
                    plan.resolved
                }
            };
            Ok(ResponseData::Playlists {
                playlists: vec![resolved],
            })
        }
        Request::PlaylistRemoveItems {
            playlist,
            uris,
            provider,
        } => {
            // A confirmed/failed mutation may no longer satisfy positional
            // preflight (the item can already be absent). Replay the durable
            // receipt before taking the lane or touching the provider.
            if let Some(replay) =
                replay_existing_optimistic_mutation(&state, mutation_id, &request_json).await?
            {
                return Ok(replay);
            }
            // Hold the per-playlist lane from the authoritative read through
            // the provider write. The exact captured positions therefore
            // back the initial durable pre-state and the live mutation.
            let lane_guard = match mutation_lane {
                Some(lane) => Some(lane.lock_owned().await),
                None => None,
            };
            // A same-key request can race the first claim and wait behind its
            // lane guard. Recheck after acquiring the lane so it replays the
            // newly durable receipt instead of planning against changed state.
            if let Some(replay) =
                replay_existing_optimistic_mutation(&state, mutation_id, &request_json).await?
            {
                return Ok(replay);
            }
            let plan =
                build_playlist_remove_plan(&state, &playlist, &uris, provider.as_ref()).await?;
            let initial_pre_state = spotuify_protocol::PreState::PlaylistRemove {
                playlist_id: plan.resolved.id.clone(),
                version_token: plan.resolved.version_token.clone(),
                removed_items: plan.removed_items.clone(),
            };
            let state_for = state.clone();
            let subject_uris = uris.clone();
            let provider_mutation_id = mutation_id.map_or_else(Uuid::now_v7, |id| id.0);
            spawn_optimistic_mutation(
                &state,
                OperationKind::PlaylistRemove,
                operation_source,
                subject_uris,
                "playlist-remove",
                request_json.clone(),
                Some(initial_pre_state.clone()),
                Some(spotuify_protocol::ReversalPlan::NotReversible {
                    reason: "playlist removal reversal awaits the provider's post-mutation version"
                        .to_string(),
                }),
                None,
                mutation_id,
                move |op_id| async move {
                    let _lane_guard = lane_guard;
                    let provider_id = plan.provider.id().clone();
                    let mutation = plan.mutation();
                    #[cfg(test)]
                    assert_playlist_remove_prestate_staged(&state_for, op_id, &initial_pre_state)
                        .await?;
                    let receipt = apply_provider_mutation(
                        plan.provider.clone(),
                        provider_mutation_id,
                        mutation.clone(),
                    )
                    .await?;
                    if plan.provider.capabilities().playlists.version_tokens {
                        let post_version = receipt.version_token.clone().ok_or_else(|| {
                            provider_mutation_reconciliation_required_after_local_failure(
                                provider_id.clone(),
                                mutation.clone(),
                                &receipt,
                                "provider omitted the required post-mutation playlist version",
                            )
                        })?;
                        let reversal = spotuify_protocol::ReversalPlan::PlaylistAddAtPositions {
                            playlist_id: plan.resolved.id.clone(),
                            items: plan.removed_items.clone(),
                            version_token: Some(post_version),
                        };
                        if let Err(error) = activate_playlist_remove_reversal_plan(
                            &state_for,
                            op_id,
                            &initial_pre_state,
                            &reversal,
                        )
                        .await
                        {
                            return Err(
                                provider_mutation_reconciliation_required_after_local_failure(
                                    provider_id,
                                    mutation,
                                    &receipt,
                                    error,
                                ),
                            );
                        }
                    }
                    let message =
                        format!("Removed {} item(s) from {}", uris.len(), plan.resolved.name);
                    state_for.emit_event(DaemonEvent::PlaylistsChanged {
                        action: "playlist-remove".to_string(),
                        playlist: Some(plan.resolved.id.clone()),
                        provider: Some(provider_id),
                    });
                    emit_mutation_finished(&state_for, "playlist-remove", &message);
                    Ok(())
                },
            )
            .await
        }
        Request::PlaylistCreatePreview {
            name,
            description,
            uris,
            provider,
        } => {
            preflight_playlist_create_request(
                &state,
                &name,
                description.as_deref(),
                &uris,
                provider.as_ref(),
            )
            .await?;
            Ok(ResponseData::Playlists {
                playlists: Vec::new(),
            })
        }
        Request::PlaylistCreate {
            name,
            description,
            uris,
            provider,
        } => {
            if let Some(replay) =
                replay_existing_recorded_mutation(&state, mutation_id, &request_json).await?
            {
                return Ok(replay);
            }
            let (provider_id, provider, create_mutation, items) =
                preflight_playlist_create_request(
                    &state,
                    &name,
                    description.as_deref(),
                    &uris,
                    provider.as_ref(),
                )
                .await?;
            let request_summary = request_json.clone();
            let state_for = state.clone();
            let name_for = name.clone();
            let uris_for = uris.clone();
            let provider_mutation_id = mutation_id.map_or_else(Uuid::now_v7, |id| id.0);
            record_operation(
                &state,
                OperationKind::PlaylistCreate,
                operation_source,
                vec![],
                "playlist-create",
                &request_summary,
                mutation_id,
                None,
                None,
                mutation_lane,
                move |op_id| async move {
                    let create_receipt = apply_provider_mutation(
                        provider.clone(),
                        provider_mutation_id,
                        create_mutation,
                    )
                    .await?;
                    let MutationOutcome::PlaylistCreated { mut playlist } = create_receipt.outcome
                    else {
                        anyhow::bail!("provider returned the wrong outcome for playlist create");
                    };
                    let playlist_uri =
                        playlist_resource_for_provider(provider.as_ref(), &playlist.id)?;
                    playlist.id = playlist_uri.as_uri();
                    let playlist_uri_string = playlist_uri.as_uri();
                    let pre_state = spotuify_protocol::PreState::PlaylistCreate {
                        playlist_id: playlist.id.clone(),
                    };
                    let plan = spotuify_protocol::ReversalPlan::PlaylistDelete {
                        playlist_id: playlist.id.clone(),
                    };
                    let local_plan = async {
                        state_for
                            .store()
                            .update_operation_plan(op_id, Some(&pre_state), Some(&plan))
                            .await?;
                        state_for
                            .store()
                            .update_operation_subject_uris(
                                op_id,
                                std::slice::from_ref(&playlist_uri_string),
                            )
                            .await?;
                        anyhow::Ok(())
                    }
                    .await;
                    if let Err(error) = local_plan {
                        let mut rollback_error = rollback_created_playlist(
                            provider,
                            playlist_uri,
                            "persisting its local reversal plan",
                            error,
                        )
                        .await;
                        retain_operation_recovery(
                            &mut rollback_error,
                            pre_state,
                            plan,
                            vec![playlist_uri_string],
                        );
                        return Err(rollback_error);
                    }
                    if !items.is_empty() {
                        if let Err(mut error) = populate_created_playlist(
                            provider,
                            Uuid::now_v7(),
                            playlist_uri.clone(),
                            items,
                            playlist.version_token.clone(),
                        )
                        .await
                        {
                            retain_operation_recovery(
                                &mut error,
                                pre_state,
                                plan,
                                vec![playlist_uri_string],
                            );
                            return Err(error);
                        }
                    }
                    cache_playlists(&state_for, &provider_id, std::slice::from_ref(&playlist))
                        .await;
                    state_for.emit_event(DaemonEvent::PlaylistsChanged {
                        action: "playlist-create".to_string(),
                        playlist: Some(playlist.id.clone()),
                        provider: Some(provider_id),
                    });
                    let message = format!(
                        "Created playlist `{name_for}` with {} item(s)",
                        uris_for.len()
                    );
                    emit_mutation_finished(&state_for, "playlist-create", &message);
                    Ok(ResponseData::PlaylistCreate {
                        receipt: PlaylistCreateReceipt {
                            ok: true,
                            action: "playlist-create".to_string(),
                            playlist_uri: playlist_uri.as_uri(),
                            playlist_id: playlist.id,
                            name: playlist.name,
                            added_item_count: uris_for.len(),
                            message,
                            receipt_id: None,
                            mutation_id: None,
                            replayed: false,
                        },
                    })
                },
            )
            .await
        }
        Request::PlaylistUnfollow { playlist, provider } => {
            if let Some(replay) =
                replay_existing_optimistic_mutation(&state, mutation_id, &request_json).await?
            {
                return Ok(replay);
            }
            let state_for = state.clone();
            let (_, selected_provider) =
                resolve_playlist_provider(&state, &playlist, provider.as_ref()).await?;
            let preflight_uri = playlist_preflight_resource(selected_provider.as_ref())?;
            require_provider_mutation_capability(
                selected_provider.as_ref(),
                &Mutation::PlaylistUnfollow {
                    playlist_uri: preflight_uri,
                },
            )?;
            let (selected_provider, resolved, playlist_uri) = resolve_provider_playlist(
                &state,
                &playlist,
                provider.as_ref(),
                RequestContext::FOREGROUND,
            )
            .await?;
            let provider_mutation_id = mutation_id.map_or_else(Uuid::now_v7, |id| id.0);
            spawn_optimistic_mutation(
                &state,
                OperationKind::PlaylistUnfollow,
                operation_source,
                vec![playlist_uri.as_uri()],
                "playlist-unfollow",
                request_json.clone(),
                None,
                None,
                mutation_lane,
                mutation_id,
                move |_op_id| async move {
                    let provider_id = selected_provider.id().clone();
                    apply_provider_mutation(
                        selected_provider,
                        provider_mutation_id,
                        Mutation::PlaylistUnfollow {
                            playlist_uri: playlist_uri.clone(),
                        },
                    )
                    .await?;
                    let message = format!("Unfollowed playlist {}", resolved.name);
                    state_for.emit_event(DaemonEvent::PlaylistsChanged {
                        action: "playlist-unfollow".to_string(),
                        playlist: Some(resolved.id),
                        provider: Some(provider_id),
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
            provider,
        } => {
            if let Some(replay) =
                replay_existing_optimistic_mutation(&state, mutation_id, &request_json).await?
            {
                return Ok(replay);
            }
            // Spotify caps the base64-encoded body at 256 KB. The CLI
            // checks too, but a fast bail here protects MCP callers and
            // any future direct-IPC clients.
            const MAX_IMAGE_BASE64_BYTES: usize = 256 * 1024;
            if image_base64.is_empty() {
                return Err(ProviderError::InvalidInput {
                    field: "image_base64".to_string(),
                    message: "image is empty".to_string(),
                }
                .into());
            }
            if image_base64.len() > MAX_IMAGE_BASE64_BYTES {
                return Err(ProviderError::InvalidInput {
                    field: "image_base64".to_string(),
                    message: format!(
                        "encoded image is {} bytes, exceeds the provider limit of 256 KB",
                        image_base64.len()
                    ),
                }
                .into());
            }
            let state_for = state.clone();
            let (_, selected_provider) =
                resolve_playlist_provider(&state, &playlist, provider.as_ref()).await?;
            let preflight_uri = playlist_preflight_resource(selected_provider.as_ref())?;
            require_provider_mutation_capability(
                selected_provider.as_ref(),
                &Mutation::PlaylistSetImage {
                    playlist_uri: preflight_uri,
                    jpeg: Vec::new(),
                },
            )?;
            let image_for = base64::engine::general_purpose::STANDARD
                .decode(&image_base64)
                .map_err(|error| ProviderError::InvalidInput {
                    field: "image_base64".to_string(),
                    message: format!("malformed base64: {error}"),
                })?;
            let (selected_provider, resolved, playlist_uri) = resolve_provider_playlist(
                &state,
                &playlist,
                provider.as_ref(),
                RequestContext::FOREGROUND,
            )
            .await?;
            let provider_mutation_id = mutation_id.map_or_else(Uuid::now_v7, |id| id.0);
            spawn_optimistic_mutation(
                &state,
                OperationKind::PlaylistSetImage,
                operation_source,
                vec![playlist_uri.as_uri()],
                "playlist-set-image",
                request_json.clone(),
                None,
                None,
                mutation_lane,
                mutation_id,
                move |_op_id| async move {
                    let provider_id = selected_provider.id().clone();
                    apply_provider_mutation(
                        selected_provider,
                        provider_mutation_id,
                        Mutation::PlaylistSetImage {
                            playlist_uri: playlist_uri.clone(),
                            jpeg: image_for,
                        },
                    )
                    .await?;
                    let message = format!("Updated cover for playlist {}", resolved.name);
                    state_for.emit_event(DaemonEvent::PlaylistsChanged {
                        action: "playlist-set-image".to_string(),
                        playlist: Some(resolved.id),
                        provider: Some(provider_id),
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

async fn rollback_created_playlist(
    provider: Arc<dyn MusicProvider>,
    playlist_uri: ResourceUri,
    failed_step: &str,
    cause: anyhow::Error,
) -> anyhow::Error {
    let provider_id = provider.id().clone();
    let compensation = tokio::time::timeout(
        MUTATION_BODY_TIMEOUT,
        apply_provider_mutation(
            provider,
            Uuid::now_v7(),
            Mutation::PlaylistUnfollow {
                playlist_uri: playlist_uri.clone(),
            },
        ),
    )
    .await;
    match compensation {
        Ok(Ok(_)) => ProviderError::Provider(format!(
            "playlist creation was rolled back after {failed_step} failed: {cause}"
        ))
        .into(),
        Ok(Err(rollback_error)) => remote_artifact_retained(
            provider_id,
            format!(
                "playlist `{playlist_uri}` remains: {failed_step} failed ({cause}) and rollback failed ({rollback_error}); delete it manually before retrying"
            ),
        ),
        Err(_) => remote_artifact_retained(
            provider_id,
            format!(
                "playlist `{playlist_uri}` may remain: {failed_step} failed ({cause}) and rollback timed out; inspect/delete it manually before retrying"
            ),
        ),
    }
}

fn resolve_playlist(playlists: &[Playlist], value: &str) -> anyhow::Result<Playlist> {
    let canonical = ResourceUri::parse(value).ok().map(|uri| uri.as_uri());
    playlists
        .iter()
        .find(|playlist| {
            playlist.id == value
                || playlist.name.eq_ignore_ascii_case(value)
                || canonical.as_deref() == Some(playlist.id.as_str())
                || ResourceUri::parse(&playlist.id).is_ok_and(|uri| uri.bare_id() == value)
        })
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no playlist matching `{value}`"))
}

fn preflight_playlist_item_uri(
    provider: &dyn MusicProvider,
    value: &str,
) -> anyhow::Result<ResourceUri> {
    let resource = ResourceUri::parse(value)?;
    if !matches!(resource.kind(), MediaKind::Track | MediaKind::Episode) {
        return Err(ProviderError::InvalidInput {
            field: "uris".to_string(),
            message: format!(
                "playlist items must be track or episode URIs, got {}",
                resource.kind()
            ),
        }
        .into());
    }
    if resource.scheme() != provider.uri_scheme() {
        return Err(ProviderError::InvalidInput {
            field: "uri".to_string(),
            message: format!(
                "resource {} belongs to `{}`, not provider {} (`{}`)",
                resource.as_uri(),
                resource.scheme(),
                provider.id(),
                provider.uri_scheme(),
            ),
        }
        .into());
    }
    Ok(resource)
}

fn playlist_preflight_resource(provider: &dyn MusicProvider) -> anyhow::Result<ResourceUri> {
    Ok(ResourceUri::new(
        provider.uri_scheme().clone(),
        MediaKind::Playlist,
        "preflight",
    )?)
}

async fn preflight_playlist_add_request(
    state: &DaemonState,
    playlist: &str,
    uris: &[String],
    requested_provider: Option<&spotuify_core::ProviderId>,
) -> anyhow::Result<(
    Arc<dyn MusicProvider>,
    Playlist,
    ResourceUri,
    Vec<PlaylistInsertion>,
)> {
    if uris.is_empty() {
        anyhow::bail!("no track URIs to add");
    }
    let (_, provider) = resolve_playlist_provider(state, playlist, requested_provider).await?;
    let items = preflight_playlist_add_items(provider.as_ref(), uris)?;
    let (provider, resolved, playlist_uri) = resolve_provider_playlist(
        state,
        playlist,
        requested_provider,
        RequestContext::FOREGROUND,
    )
    .await?;
    Ok((provider, resolved, playlist_uri, items))
}

async fn preflight_playlist_remove_request(
    state: &DaemonState,
    playlist: &str,
    uris: &[String],
    requested_provider: Option<&spotuify_core::ProviderId>,
) -> anyhow::Result<(Arc<dyn MusicProvider>, Vec<ResourceUri>)> {
    if uris.is_empty() {
        anyhow::bail!("no track URIs to remove");
    }
    let (_, provider) = resolve_playlist_provider(state, playlist, requested_provider).await?;
    let items = preflight_playlist_remove_items(provider.as_ref(), uris)?;
    Ok((provider, items))
}

async fn build_playlist_remove_plan(
    state: &DaemonState,
    playlist: &str,
    uris: &[String],
    requested_provider: Option<&spotuify_core::ProviderId>,
) -> anyhow::Result<PlaylistRemovePlan> {
    let (preflight_provider, item_uris) =
        preflight_playlist_remove_request(state, playlist, uris, requested_provider).await?;
    let (provider, resolved, playlist_uri) = resolve_provider_playlist(
        state,
        playlist,
        requested_provider,
        RequestContext::FOREGROUND,
    )
    .await?;
    if provider.id() != preflight_provider.id() {
        return Err(ProviderError::InvalidInput {
            field: "provider".to_string(),
            message: "playlist provider changed during removal preflight".to_string(),
        }
        .into());
    }
    if provider.capabilities().playlists.version_tokens && resolved.version_token.is_none() {
        return Err(ProviderError::Provider(format!(
            "provider {} advertises playlist version tokens but omitted one for {}",
            provider.id(),
            playlist_uri
        ))
        .into());
    }
    let current_items =
        collect_playlist_remove_pre_state(provider.clone(), playlist_uri.clone()).await?;
    let (mutation_items, mut removed_items) =
        select_playlist_remove_items(&item_uris, &current_items)?;
    removed_items.sort_by_key(|(_, position)| *position);
    let plan = PlaylistRemovePlan {
        provider,
        resolved,
        playlist_uri,
        mutation_items,
        removed_items,
    };
    require_provider_mutation_capability(plan.provider.as_ref(), &plan.mutation())?;
    Ok(plan)
}

type RemovedPlaylistItem = (String, u32);
type PlaylistRemoveSelection = (Vec<PlaylistItemRef>, Vec<RemovedPlaylistItem>);

fn select_playlist_remove_items(
    requested: &[ResourceUri],
    current: &[MediaItem],
) -> anyhow::Result<PlaylistRemoveSelection> {
    let mut available = HashMap::<String, VecDeque<u32>>::new();
    for (position, item) in current.iter().enumerate() {
        let position = u32::try_from(position).map_err(|_| ProviderError::InvalidInput {
            field: "playlist".to_string(),
            message: "playlist position exceeds the supported range".to_string(),
        })?;
        available
            .entry(item.uri.clone())
            .or_default()
            .push_back(position);
    }

    let mut mutation_items = Vec::with_capacity(requested.len());
    let mut removed_items = Vec::with_capacity(requested.len());
    for (index, uri) in requested.iter().enumerate() {
        let canonical = uri.as_uri();
        let position = available
            .get_mut(&canonical)
            .and_then(VecDeque::pop_front)
            .ok_or_else(|| ProviderError::InvalidInput {
                field: "uris".to_string(),
                message: format!(
                    "playlist does not contain requested occurrence {} of `{canonical}`",
                    index + 1
                ),
            })?;
        mutation_items.push(PlaylistItemRef {
            uri: uri.clone(),
            positions: vec![position],
        });
        removed_items.push((canonical, position));
    }
    Ok((mutation_items, removed_items))
}

async fn collect_playlist_remove_pre_state(
    provider: Arc<dyn MusicProvider>,
    playlist_uri: ResourceUri,
) -> anyhow::Result<Vec<spotuify_core::MediaItem>> {
    let provider_id = provider.id().clone();
    let outcome = tokio::time::timeout(
        PLAYLIST_REMOVE_PREFLIGHT_TIMEOUT,
        collect_playlist_items(provider, playlist_uri, RequestContext::FOREGROUND),
    )
    .await
    .map_err(|_| {
        provider_workflow_timeout_error(
            provider_id,
            "playlist removal preflight",
            PLAYLIST_REMOVE_PREFLIGHT_TIMEOUT,
        )
    })??;
    match outcome {
        AccessOutcome::Available(items) => Ok(items),
        AccessOutcome::Unavailable(reason) => Err(ProviderError::Forbidden {
            operation: format!("capture playlist removal pre-state ({reason:?})"),
        }
        .into()),
    }
}

async fn preflight_playlist_create_request(
    state: &DaemonState,
    name: &str,
    description: Option<&str>,
    uris: &[String],
    requested_provider: Option<&spotuify_core::ProviderId>,
) -> anyhow::Result<(
    spotuify_core::ProviderId,
    Arc<dyn MusicProvider>,
    Mutation,
    Vec<PlaylistInsertion>,
)> {
    if uris.is_empty() {
        anyhow::bail!("no resolved track URIs to add");
    }
    let (provider_id, provider) = state.provider_or_default(requested_provider).await?;
    // Validate the complete two-step mutation before anything remote. Preview
    // and the live mutation body share this exact path.
    let (create, items) = preflight_playlist_creation(provider.as_ref(), name, description, uris)?;
    Ok((provider_id, provider, create, items))
}

fn preflight_playlist_add_items(
    provider: &dyn MusicProvider,
    uris: &[String],
) -> anyhow::Result<Vec<PlaylistInsertion>> {
    let items = uris
        .iter()
        .map(|uri| {
            Ok(PlaylistInsertion {
                uri: preflight_playlist_item_uri(provider, uri)?,
                position: None,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let playlist_uri = playlist_preflight_resource(provider)?;
    require_provider_mutation_capability(
        provider,
        &Mutation::PlaylistAdd {
            playlist_uri,
            items: items.clone(),
            expected_version: None,
        },
    )?;
    Ok(items)
}

fn preflight_playlist_remove_items(
    provider: &dyn MusicProvider,
    uris: &[String],
) -> anyhow::Result<Vec<ResourceUri>> {
    let item_uris = uris
        .iter()
        .map(|uri| preflight_playlist_item_uri(provider, uri))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let playlist_uri = playlist_preflight_resource(provider)?;
    let items = item_uris
        .iter()
        .cloned()
        .map(|uri| PlaylistItemRef {
            uri,
            // Capability probing must never model the provider's dangerous
            // "remove every matching URI" form.
            positions: vec![0],
        })
        .collect();
    require_provider_mutation_capability(
        provider,
        &Mutation::PlaylistRemove {
            playlist_uri,
            items,
            expected_version: None,
        },
    )?;
    Ok(item_uris)
}

async fn resolve_provider_playlist(
    state: &DaemonState,
    value: &str,
    requested_provider: Option<&spotuify_core::ProviderId>,
    context: RequestContext,
) -> anyhow::Result<(Arc<dyn MusicProvider>, Playlist, ResourceUri)> {
    if let Ok(resource) = ResourceUri::parse(value) {
        let (_, provider) = resolve_playlist_provider(state, value, requested_provider).await?;
        require_provider_capability(
            provider.as_ref(),
            "playlist metadata",
            provider.capabilities().playlists.list,
        )?;
        let playlist = provider
            .playlist(context, &resource)
            .await?
            .ok_or_else(|| ProviderError::NotFound {
                resource: resource.as_uri(),
            })?;
        let playlist = normalize_provider_playlist_list(provider.as_ref(), vec![playlist])?
            .into_iter()
            .next()
            .expect("single normalized playlist");
        if playlist.id != resource.as_uri() {
            return Err(ProviderError::InvalidInput {
                field: "playlist".to_string(),
                message: format!(
                    "provider {} returned playlist `{}` for requested `{resource}`",
                    provider.id(),
                    playlist.id
                ),
            }
            .into());
        }
        return Ok((provider, playlist, resource));
    }

    let (_, provider) = resolve_playlist_provider(state, value, requested_provider).await?;
    let playlists = normalize_provider_playlist_list(
        provider.as_ref(),
        collect_playlists(provider.clone(), context).await?,
    )?;
    let playlist = resolve_playlist(&playlists, value)?;
    let resource = playlist_resource_for_provider(provider.as_ref(), &playlist.id)?;
    Ok((provider, playlist, resource))
}

async fn resolve_playlist_provider(
    state: &DaemonState,
    value: &str,
    requested_provider: Option<&spotuify_core::ProviderId>,
) -> anyhow::Result<(spotuify_core::ProviderId, Arc<dyn MusicProvider>)> {
    let providers = state.providers().await?;
    let requested = requested_provider
        .map(|provider_id| providers.provider(provider_id))
        .transpose()?;
    let runtime = match ResourceUri::parse(value) {
        Ok(resource) => {
            require_resource_kind(&resource, MediaKind::Playlist, "playlist")?;
            let owner = providers.provider_for_uri(&resource)?;
            if let Some(requested) = requested {
                if requested.id() != owner.id() {
                    return Err(ProviderError::InvalidInput {
                        field: "provider".to_string(),
                        message: format!(
                            "provider `{}` conflicts with playlist URI scheme `{}` owned by `{}`",
                            requested.id(),
                            resource.scheme(),
                            owner.id(),
                        ),
                    }
                    .into());
                }
            }
            owner
        }
        Err(_) => requested.unwrap_or_else(|| providers.default_provider()),
    };
    Ok((runtime.id().clone(), runtime.music()))
}

fn playlist_resource_for_provider(
    provider: &dyn MusicProvider,
    value: &str,
) -> anyhow::Result<ResourceUri> {
    let resource = ResourceUri::parse(value)
        .or_else(|_| ResourceUri::new(provider.uri_scheme().clone(), MediaKind::Playlist, value))?;
    if resource.kind() != MediaKind::Playlist || resource.scheme() != provider.uri_scheme() {
        return Err(ProviderError::InvalidInput {
            field: "playlist".to_string(),
            message: format!(
                "playlist `{resource}` does not belong to provider {}",
                provider.id()
            ),
        }
        .into());
    }
    Ok(resource)
}

fn normalize_provider_playlist_list(
    provider: &dyn MusicProvider,
    playlists: Vec<Playlist>,
) -> anyhow::Result<Vec<Playlist>> {
    playlists
        .into_iter()
        .map(|mut playlist| {
            playlist.id = playlist_resource_for_provider(provider, &playlist.id)?.as_uri();
            Ok(playlist)
        })
        .collect()
}

async fn collect_playlists(
    provider: Arc<dyn MusicProvider>,
    context: RequestContext,
) -> anyhow::Result<Vec<Playlist>> {
    require_provider_capability(
        provider.as_ref(),
        "playlist listing",
        provider.capabilities().playlists.list,
    )?;
    let limit = provider
        .capabilities()
        .playlists
        .list_max_page_size
        .unwrap_or(50) as u32;
    let mut request = PageRequest::new(limit.max(1), 0);
    let mut playlists = Vec::new();
    let mut seen_cursors = std::collections::HashSet::new();
    for page_index in 0..PROVIDER_PAGINATION_MAX_PAGES {
        let page = provider.playlists(context, request.clone()).await?;
        validate_provider_page_offset(&request, &page, "playlists")?;
        playlists.extend(page.items);
        let Some(next) = page.next else {
            return Ok(playlists);
        };
        request = next_provider_page(
            &request,
            next,
            playlists.len() as u64,
            &mut seen_cursors,
            page_index + 1,
            "playlists",
        )?;
    }
    Err(ProviderError::Provider(format!(
        "playlist pagination exceeded {PROVIDER_PAGINATION_MAX_PAGES} pages"
    ))
    .into())
}

async fn collect_playlist_items(
    provider: Arc<dyn MusicProvider>,
    uri: ResourceUri,
    context: RequestContext,
) -> anyhow::Result<AccessOutcome<Vec<MediaItem>>> {
    require_provider_capability(
        provider.as_ref(),
        "playlist item reads",
        provider.capabilities().playlists.item_read,
    )?;
    let limit = provider
        .capabilities()
        .playlists
        .items_max_page_size
        .unwrap_or(50) as u32;
    let mut request = PageRequest::new(limit.max(1), 0);
    let mut items = Vec::new();
    let mut seen_cursors = std::collections::HashSet::new();
    for page_index in 0..PROVIDER_PAGINATION_MAX_PAGES {
        let page = match provider
            .playlist_items(
                context,
                CollectionRequest {
                    uri: uri.clone(),
                    page: request.clone(),
                },
            )
            .await?
        {
            AccessOutcome::Available(page) => page,
            AccessOutcome::Unavailable(reason) => {
                return Ok(AccessOutcome::Unavailable(reason));
            }
        };
        validate_provider_page_offset(&request, &page, "playlist_items")?;
        validate_provider_collection_items(
            provider.as_ref(),
            "playlist_items",
            &[MediaKind::Track, MediaKind::Episode],
            &page.items,
        )?;
        items.extend(page.items);
        let Some(next) = page.next else {
            return Ok(AccessOutcome::Available(items));
        };
        request = next_provider_page(
            &request,
            next,
            items.len() as u64,
            &mut seen_cursors,
            page_index + 1,
            "playlist items",
        )?;
    }
    Err(ProviderError::Provider(format!(
        "playlist item pagination exceeded {PROVIDER_PAGINATION_MAX_PAGES} pages"
    ))
    .into())
}

fn preflight_playlist_creation(
    provider: &dyn MusicProvider,
    name: &str,
    description: Option<&str>,
    uris: &[String],
) -> anyhow::Result<(Mutation, Vec<PlaylistInsertion>)> {
    if name.trim().is_empty() {
        return Err(ProviderError::InvalidInput {
            field: "name".to_string(),
            message: "playlist name cannot be empty".to_string(),
        }
        .into());
    }
    let items = uris
        .iter()
        .map(|uri| {
            let uri = ResourceUri::parse(uri)?;
            if uri.kind() != MediaKind::Track {
                anyhow::bail!("playlist creation candidates must be track URIs: {uri}");
            }
            Ok(PlaylistInsertion {
                uri,
                position: None,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let create = Mutation::PlaylistCreate {
        name: name.to_string(),
        public: Some(false),
        description: description.map(str::to_string),
    };
    require_provider_mutation_capability(provider, &create)?;
    if !items.is_empty() {
        let playlist_uri = ResourceUri::new(
            provider.uri_scheme().clone(),
            MediaKind::Playlist,
            "preflight",
        )?;
        require_provider_mutation_capability(
            provider,
            &Mutation::PlaylistAdd {
                playlist_uri: playlist_uri.clone(),
                items: items.clone(),
                expected_version: None,
            },
        )?;
        require_provider_mutation_capability(
            provider,
            &Mutation::PlaylistUnfollow { playlist_uri },
        )?;
    }
    Ok((create, items))
}

async fn activate_playlist_remove_reversal_plan(
    state: &DaemonState,
    operation_id: spotuify_protocol::OperationId,
    pre_state: &spotuify_protocol::PreState,
    plan: &spotuify_protocol::ReversalPlan,
) -> anyhow::Result<()> {
    #[cfg(test)]
    if FAIL_NEXT_PLAYLIST_REMOVE_PLAN_ACTIVATION.swap(false, Ordering::SeqCst) {
        anyhow::bail!("injected playlist removal reversal-plan activation failure");
    }
    state
        .store()
        .activate_operation_reversal_plan(operation_id, pre_state, plan)
        .await
}

#[cfg(test)]
pub(crate) fn fail_next_playlist_remove_plan_activation() {
    FAIL_NEXT_PLAYLIST_REMOVE_PLAN_ACTIVATION.store(true, Ordering::SeqCst);
}

#[cfg(test)]
async fn assert_playlist_remove_prestate_staged(
    state: &DaemonState,
    operation_id: spotuify_protocol::OperationId,
    expected: &spotuify_protocol::PreState,
) -> anyhow::Result<()> {
    let operation = state.store().get_operation(operation_id).await?;
    if operation.pre_state.as_ref() != Some(expected)
        || operation.reversible
        || !matches!(
            operation.reversal_plan,
            Some(spotuify_protocol::ReversalPlan::NotReversible { .. })
        )
    {
        anyhow::bail!(
            "playlist removal exact pre-state was not staged as non-reversible before apply"
        );
    }
    PLAYLIST_REMOVE_PRESTATE_OBSERVED_BEFORE_APPLY.store(true, Ordering::SeqCst);
    Ok(())
}

#[cfg(test)]
pub(crate) fn reset_playlist_remove_prestate_observation() {
    PLAYLIST_REMOVE_PRESTATE_OBSERVED_BEFORE_APPLY.store(false, Ordering::SeqCst);
}

#[cfg(test)]
pub(crate) fn playlist_remove_prestate_was_observed_before_apply() -> bool {
    PLAYLIST_REMOVE_PRESTATE_OBSERVED_BEFORE_APPLY.load(Ordering::SeqCst)
}

async fn apply_provider_mutation(
    provider: Arc<dyn MusicProvider>,
    mutation_id: Uuid,
    mutation: Mutation,
) -> anyhow::Result<MutationReceipt> {
    apply_provider_mutation_checked(provider.as_ref(), mutation_id, &mutation).await
}

async fn populate_created_playlist(
    provider: Arc<dyn MusicProvider>,
    mutation_id: Uuid,
    playlist_uri: ResourceUri,
    items: Vec<PlaylistInsertion>,
    expected_version: Option<String>,
) -> anyhow::Result<MutationReceipt> {
    let provider_id = provider.id().clone();
    let add = tokio::time::timeout(
        MUTATION_BODY_TIMEOUT,
        apply_provider_mutation(
            provider.clone(),
            mutation_id,
            Mutation::PlaylistAdd {
                playlist_uri: playlist_uri.clone(),
                items,
                expected_version,
            },
        ),
    )
    .await;
    match add {
        Ok(Ok(receipt)) => Ok(receipt),
        Err(_) => Err(remote_artifact_retained(
            provider_id,
            format!(
                "playlist `{playlist_uri}` was created, but adding items timed out; remote outcome is indeterminate — inspect the playlist before retrying"
            ),
        )),
        Ok(Err(add_error)) if mutation_outcome_is_indeterminate(&add_error) => {
            Err(remote_artifact_retained(
                provider_id,
                format!(
                    "playlist `{playlist_uri}` was created, but adding items had an indeterminate outcome: {add_error}; inspect the playlist before retrying"
                ),
            ))
        }
        // Some items landed remotely. Preserve the provider's structured
        // partial receipt for the durable lifecycle and let its reconciliation
        // pass establish authoritative playlist state; compensating here could
        // erase successful work while losing the per-item failure report.
        Ok(Err(add_error)) if is_partial_mutation_error(&add_error) => Err(add_error),
        Ok(Err(add_error)) => {
            let compensation = tokio::time::timeout(
                MUTATION_BODY_TIMEOUT,
                apply_provider_mutation(
                    provider,
                    Uuid::now_v7(),
                    Mutation::PlaylistUnfollow {
                        playlist_uri: playlist_uri.clone(),
                    },
                ),
            )
            .await;
            match compensation {
                Ok(Ok(_)) => Err(ProviderError::Provider(format!(
                    "playlist creation was rolled back because adding items failed: {add_error}"
                ))
                .into()),
                Ok(Err(compensation_error)) => Err(remote_artifact_retained(
                    provider_id,
                    format!(
                        "playlist `{playlist_uri}` remains empty: adding items failed ({add_error}) and rollback failed ({compensation_error}); delete it manually before retrying"
                    ),
                )),
                Err(_) => Err(remote_artifact_retained(
                    provider_id,
                    format!(
                        "playlist `{playlist_uri}` may remain empty: adding items failed ({add_error}) and rollback timed out; inspect/delete it manually before retrying"
                    ),
                )),
            }
        }
    }
}

fn mutation_outcome_is_indeterminate(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<ProviderError>(),
        Some(
            ProviderError::Network(_)
                | ProviderError::Transient { .. }
                | ProviderError::Upstream {
                    status: 500..=599,
                    ..
                }
        )
    )
}

async fn resolve_provider_playlist_version(
    provider: Arc<dyn MusicProvider>,
    uri: &ResourceUri,
    cached: Option<String>,
) -> Option<String> {
    if !provider.capabilities().playlists.list {
        return cached;
    }
    provider
        .playlist(RequestContext::BACKGROUND_SYNC, uri)
        .await
        .ok()
        .flatten()
        .and_then(|playlist| playlist.version_token)
        .or(cached)
}

fn spawn_provider_playlists_refresh(
    state: Arc<DaemonState>,
    provider_id: spotuify_core::ProviderId,
) {
    let task_state = state.clone();
    state.spawn_background("provider-playlists-refresh", async move {
        if skip_refresh_due_to_rate_limit(
            &task_state,
            &provider_id,
            "playlists",
            "playlists-refresh",
        )
        .await
        {
            return;
        }
        let Ok(provider) = task_state.provider(&provider_id).await else {
            return;
        };
        match collect_playlists(provider, RequestContext::BACKGROUND_SYNC).await {
            Ok(playlists) if !playlists.is_empty() => {
                cache_playlists(&task_state, &provider_id, &playlists).await;
                task_state.emit_event(DaemonEvent::PlaylistsChanged {
                    action: "refreshed".to_string(),
                    playlist: None,
                    provider: Some(provider_id),
                });
            }
            Ok(_) => {}
            Err(error) => tracing::debug!(%error, "background provider playlists refresh failed"),
        }
    });
}

fn spawn_provider_playlist_items_refresh(
    state: Arc<DaemonState>,
    playlist: String,
    provider_id: spotuify_core::ProviderId,
) {
    let task_state = state.clone();
    state.spawn_background("provider-playlist-items-refresh", async move {
        if skip_refresh_due_to_rate_limit(
            &task_state,
            &provider_id,
            "playlists",
            "playlist-tracks-refresh",
        )
        .await
        {
            return;
        }
        let Ok((provider, resolved, uri)) = resolve_provider_playlist(
            task_state.as_ref(),
            &playlist,
            Some(&provider_id),
            RequestContext::BACKGROUND_SYNC,
        )
        .await
        else {
            return;
        };
        cache_playlists(
            task_state.as_ref(),
            &provider_id,
            std::slice::from_ref(&resolved),
        )
        .await;
        match collect_playlist_items(
            provider.clone(),
            uri.clone(),
            RequestContext::BACKGROUND_SYNC,
        )
        .await
        {
            Ok(AccessOutcome::Available(items)) => {
                cache_playlist_items(&task_state, &provider_id, &resolved.id, &items).await;
                task_state.emit_event(DaemonEvent::PlaylistsChanged {
                    action: "tracks-refreshed".to_string(),
                    playlist: Some(resolved.id),
                    provider: Some(provider_id),
                });
            }
            Ok(AccessOutcome::Unavailable(_)) => {
                let cached = task_state
                    .store()
                    .playlist_version_token(&resolved.id)
                    .await
                    .ok()
                    .flatten();
                let observed = resolve_provider_playlist_version(provider, &uri, cached).await;
                let _ = task_state
                    .store()
                    .mark_playlist_tracks_inaccessible_at_version(&resolved.id, observed.as_deref())
                    .await;
                task_state.emit_event(DaemonEvent::PlaylistsChanged {
                    action: "tracks-inaccessible".to_string(),
                    playlist: Some(resolved.id),
                    provider: Some(provider_id),
                });
            }
            Err(error) => {
                tracing::debug!(%error, playlist = %uri, "background playlist items refresh failed")
            }
        }
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic, clippy::unwrap_used)]

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use spotuify_core::{
        AccessOutcome, CollectionRequest, MediaItem, MediaKind, MusicProvider, Mutation,
        MutationCompletion, MutationOutcome, MutationReceipt, PageRequest, PlaylistInsertion,
        ProviderCaps, ProviderError, ProviderId, ProviderPage, ProviderResult, RequestContext,
        ResourceUri, UriScheme,
    };

    use super::{
        collect_playlist_remove_pre_state, populate_created_playlist, preflight_playlist_creation,
        select_playlist_remove_items,
    };
    use crate::handler::MutationRequestError;

    struct NoPlaylistAdd(spotuify_provider_fake::FakeProvider);

    struct NoPlaylistRollback(spotuify_provider_fake::FakeProvider);

    struct SlowPlaylistItems(spotuify_provider_fake::FakeProvider);

    #[async_trait::async_trait]
    impl MusicProvider for NoPlaylistAdd {
        fn id(&self) -> &ProviderId {
            MusicProvider::id(&self.0)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(&self.0)
        }

        fn display_name(&self) -> &str {
            "No Playlist Add"
        }

        fn capabilities(&self) -> ProviderCaps {
            let mut capabilities = self.0.capabilities();
            capabilities.playlists.add = false;
            capabilities
        }
    }

    #[async_trait::async_trait]
    impl MusicProvider for NoPlaylistRollback {
        fn id(&self) -> &ProviderId {
            MusicProvider::id(&self.0)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(&self.0)
        }

        fn display_name(&self) -> &str {
            "No Playlist Rollback"
        }

        fn capabilities(&self) -> ProviderCaps {
            let mut capabilities = self.0.capabilities();
            capabilities.playlists.unfollow = false;
            capabilities
        }
    }

    #[async_trait::async_trait]
    impl MusicProvider for SlowPlaylistItems {
        fn id(&self) -> &ProviderId {
            MusicProvider::id(&self.0)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(&self.0)
        }

        fn display_name(&self) -> &str {
            "Slow Playlist Items"
        }

        fn capabilities(&self) -> ProviderCaps {
            self.0.capabilities()
        }

        async fn playlist_items(
            &self,
            context: RequestContext,
            request: CollectionRequest,
        ) -> ProviderResult<AccessOutcome<ProviderPage<MediaItem>>> {
            tokio::time::sleep(Duration::from_secs(1)).await;
            self.0.playlist_items(context, request).await
        }
    }

    struct DefiniteAddFailure {
        inner: spotuify_provider_fake::FakeProvider,
        add_calls: AtomicUsize,
        rollback_calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl MusicProvider for DefiniteAddFailure {
        fn id(&self) -> &ProviderId {
            MusicProvider::id(&self.inner)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(&self.inner)
        }

        fn display_name(&self) -> &str {
            "Definite add failure"
        }

        fn capabilities(&self) -> ProviderCaps {
            self.inner.capabilities()
        }

        async fn apply_mutation(
            &self,
            _context: RequestContext,
            mutation_id: uuid::Uuid,
            mutation: &Mutation,
        ) -> spotuify_core::ProviderResult<MutationReceipt> {
            match mutation {
                Mutation::PlaylistAdd { .. } => {
                    self.add_calls.fetch_add(1, Ordering::SeqCst);
                    Err(ProviderError::InvalidInput {
                        field: "items".to_string(),
                        message: "definite rejection".to_string(),
                    })
                }
                Mutation::PlaylistUnfollow { playlist_uri } => {
                    self.rollback_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(MutationReceipt {
                        mutation_id,
                        provider: self.id().clone(),
                        completion: MutationCompletion::Applied,
                        outcome: MutationOutcome::PlaylistUnfollowed {
                            playlist_uri: playlist_uri.clone(),
                        },
                        version_token: None,
                        failures: Vec::new(),
                    })
                }
                other => panic!("unexpected mutation in compensation test: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn playlist_create_preflight_rejects_before_any_provider_write() {
        let provider = spotuify_provider_fake::FakeProvider::new();
        for uris in [
            vec!["spotify:track:foreign".to_string()],
            vec!["fake:track:track-1".to_string(); 101],
        ] {
            let error = preflight_playlist_creation(&provider, "Focus", None, &uris).unwrap_err();
            assert!(error
                .downcast_ref::<spotuify_core::ProviderError>()
                .is_some());
        }
        assert!(provider.observed_requests().await.is_empty());

        let no_add = NoPlaylistAdd(spotuify_provider_fake::FakeProvider::new());
        let error = preflight_playlist_creation(
            &no_add,
            "Focus",
            None,
            &["fake:track:track-1".to_string()],
        )
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<spotuify_core::ProviderError>(),
            Some(spotuify_core::ProviderError::Unsupported { .. })
        ));
        assert!(no_add.0.observed_requests().await.is_empty());

        let no_rollback = NoPlaylistRollback(spotuify_provider_fake::FakeProvider::new());
        let error = preflight_playlist_creation(
            &no_rollback,
            "Focus",
            None,
            &["fake:track:track-1".to_string()],
        )
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<spotuify_core::ProviderError>(),
            Some(spotuify_core::ProviderError::Unsupported { .. })
        ));
        assert!(
            no_rollback.0.observed_requests().await.is_empty(),
            "rollback capability must fail before the create write"
        );

        let (create, _) = preflight_playlist_creation(
            &provider,
            "Focus",
            Some("Deep work"),
            &["fake:track:track-1".to_string()],
        )
        .unwrap();
        assert!(matches!(
            create,
            Mutation::PlaylistCreate {
                description: Some(ref description),
                ..
            } if description == "Deep work"
        ));
        let error = preflight_playlist_creation(
            &provider,
            "   ",
            Some("Deep work"),
            &["fake:track:track-1".to_string()],
        )
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<ProviderError>(),
            Some(ProviderError::InvalidInput { field, .. }) if field == "name"
        ));
    }

    #[test]
    fn playlist_remove_selection_preserves_requested_multiplicity_and_exact_positions() {
        let item = |uri: &str| MediaItem {
            uri: uri.to_string(),
            kind: MediaKind::Track,
            ..Default::default()
        };
        let current = vec![
            item("fake:track:one"),
            item("fake:track:two"),
            item("fake:track:one"),
        ];
        let requested = vec![
            ResourceUri::parse("fake:track:one").unwrap(),
            ResourceUri::parse("fake:track:one").unwrap(),
        ];

        let (items, removed) = select_playlist_remove_items(&requested, &current).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].positions, vec![0]);
        assert_eq!(items[1].positions, vec![2]);
        assert!(items.iter().all(|item| !item.positions.is_empty()));
        assert_eq!(
            removed,
            vec![
                ("fake:track:one".to_string(), 0),
                ("fake:track:one".to_string(), 2),
            ]
        );

        let missing = vec![
            requested[0].clone(),
            requested[0].clone(),
            requested[0].clone(),
        ];
        let error = select_playlist_remove_items(&missing, &current).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<ProviderError>(),
            Some(ProviderError::InvalidInput { field, .. }) if field == "uris"
        ));
    }

    #[tokio::test]
    async fn playlist_remove_exact_plan_rejects_concurrent_version_drift() {
        let provider = spotuify_provider_fake::FakeProvider::new();
        let playlist_uri = ResourceUri::parse("fake:playlist:playlist-1").unwrap();
        let playlist = provider
            .playlist(RequestContext::FOREGROUND, &playlist_uri)
            .await
            .unwrap()
            .unwrap();
        let expected_version = playlist.version_token.clone();
        let current = match provider
            .playlist_items(
                RequestContext::FOREGROUND,
                CollectionRequest {
                    uri: playlist_uri.clone(),
                    page: PageRequest::new(100, 0),
                },
            )
            .await
            .unwrap()
        {
            AccessOutcome::Available(page) => page.items,
            AccessOutcome::Unavailable(reason) => panic!("unexpected unavailable: {reason:?}"),
        };
        let requested = vec![ResourceUri::parse(&current[0].uri).unwrap()];
        let (items, _) = select_playlist_remove_items(&requested, &current).unwrap();

        provider
            .apply_mutation(
                RequestContext::FOREGROUND,
                uuid::Uuid::now_v7(),
                &Mutation::PlaylistAdd {
                    playlist_uri: playlist_uri.clone(),
                    items: vec![PlaylistInsertion {
                        uri: requested[0].clone(),
                        position: None,
                    }],
                    expected_version: expected_version.clone(),
                },
            )
            .await
            .unwrap();
        let error = provider
            .apply_mutation(
                RequestContext::FOREGROUND,
                uuid::Uuid::now_v7(),
                &Mutation::PlaylistRemove {
                    playlist_uri,
                    items,
                    expected_version,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ProviderError::VersionConflict { .. }));
    }

    #[tokio::test]
    async fn playlist_remove_preflight_times_out_as_a_typed_read_only_error() {
        let provider = Arc::new(SlowPlaylistItems(
            spotuify_provider_fake::FakeProvider::new(),
        ));
        let playlist_uri = ResourceUri::parse("fake:playlist:playlist-1").unwrap();

        let error = collect_playlist_remove_pre_state(provider.clone(), playlist_uri)
            .await
            .expect_err("the complete authoritative read must be bounded");

        assert!(error.downcast_ref::<MutationRequestError>().is_some());
        assert!(error.to_string().contains("playlist removal preflight"));
        assert!(error.to_string().contains("timed out"));
        assert!(provider.0.observed_requests().await.is_empty());
    }

    #[tokio::test]
    async fn definite_playlist_population_failure_rolls_back_created_playlist() {
        let provider = Arc::new(DefiniteAddFailure {
            inner: spotuify_provider_fake::FakeProvider::new(),
            add_calls: AtomicUsize::new(0),
            rollback_calls: AtomicUsize::new(0),
        });

        let error = populate_created_playlist(
            provider.clone(),
            uuid::Uuid::now_v7(),
            ResourceUri::parse("fake:playlist:created").unwrap(),
            vec![PlaylistInsertion {
                uri: ResourceUri::parse("fake:track:track-1").unwrap(),
                position: None,
            }],
            None,
        )
        .await
        .expect_err("definite add failure must fail the composite mutation");

        assert!(error.to_string().contains("rolled back"));
        assert_eq!(provider.add_calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.rollback_calls.load(Ordering::SeqCst), 1);
    }
}
