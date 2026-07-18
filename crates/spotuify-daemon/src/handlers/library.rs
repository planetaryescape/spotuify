//! `library` request handlers (split out of the dispatch god-function).

use std::sync::Arc;

use spotuify_core::{
    CollectionRequest, LibraryRequest, MediaItem, MediaKind, MusicProvider, Mutation, PageRequest,
    ProviderError, RequestContext, ResourceUri,
};
use spotuify_protocol::{
    DaemonEvent, MutationId, OperationKind, OperationSource, Request, ResponseData,
};
use uuid::Uuid;

use crate::handler::*;
use crate::handlers::playback::{
    dedup_queue_items, live_queue_uris, report_partial_queue_application,
    start_embedded_queue_session,
};
use crate::state::DaemonState;

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
        Request::LibraryList { limit, provider } => {
            let (provider, _) = state.provider_or_default(provider.as_ref()).await?;
            Ok(ResponseData::MediaItems {
                items: state
                    .store()
                    .list_library_items(limit, Some(provider.as_str()))
                    .await?,
            })
        }
        Request::SavedTracks {
            limit,
            offset,
            provider,
        } => {
            if limit == 0 {
                return Err(ProviderError::InvalidInput {
                    field: "limit".to_string(),
                    message: "saved-track page limit must be greater than zero".to_string(),
                }
                .into());
            }
            // Liked songs — one page window `[offset, offset + limit)` from live
            // `/me/tracks`, still chunked to Spotify's 50-per-request cap when
            // `limit > 50`. Returns the library `total` so scroll clients can
            // size the full list and stop paginating; cache fallback on failure
            // keeps a consistent `SavedTracksPage` shape.
            let (_, provider) = state.provider_or_default(provider.as_ref()).await?;
            require_provider_capability(
                provider.as_ref(),
                "track library reads",
                provider.capabilities().library.can_read(&MediaKind::Track),
            )?;
            match fetch_saved_tracks_page(provider.as_ref(), limit, offset).await {
                Ok((items, total)) => Ok(ResponseData::SavedTracksPage {
                    items,
                    total: u32::try_from(total).unwrap_or(u32::MAX),
                    offset,
                }),
                Err(err) if saved_tracks_cache_fallback_allowed(&err) => {
                    tracing::warn!(error = %err, "saved tracks live fetch failed; serving cache");
                    let (items, total) = state
                        .store()
                        .list_saved_tracks_page(limit, offset, Some(provider.id().as_str()))
                        .await?;
                    Ok(ResponseData::SavedTracksPage {
                        items,
                        total: u32::try_from(total).unwrap_or(u32::MAX),
                        offset,
                    })
                }
                Err(err) => Err(err),
            }
        }
        Request::SavedShows { limit, provider } => {
            let (provider, _) = state.provider_or_default(provider.as_ref()).await?;
            Ok(ResponseData::MediaItems {
                items: state
                    .store()
                    .list_saved_shows(limit, Some(provider.as_str()))
                    .await?,
            })
        }
        Request::ShowEpisodes {
            show,
            limit,
            offset,
        } => {
            let uri = ResourceUri::parse(&show)?;
            require_resource_kind(&uri, MediaKind::Show, "show")?;
            let provider = state.provider_for_uri(&uri).await?;
            require_provider_capability(
                provider.as_ref(),
                "show episodes",
                provider.capabilities().catalog.show_episodes,
            )?;
            let max_page = provider
                .capabilities()
                .catalog
                .show_episodes_max_page_size
                .unwrap_or(50) as u32;
            let page_request = PageRequest::new(limit.min(max_page), u64::from(offset));
            let items = provider
                .show_episodes(
                    RequestContext::FOREGROUND,
                    CollectionRequest {
                        uri,
                        page: page_request.clone(),
                    },
                )
                .await?;
            validate_provider_page_offset(&page_request, &items, "show_episodes")?;
            validate_provider_collection_items(
                provider.as_ref(),
                "show_episodes",
                &[MediaKind::Episode],
                &items.items,
            )?;
            Ok(ResponseData::MediaItems { items: items.items })
        }
        Request::EpisodeFeed {
            limit,
            sort,
            refresh,
            provider,
        } => {
            let (provider, _) = state.provider_or_default(provider.as_ref()).await?;
            Ok(ResponseData::MediaItems {
                items: episode_feed(&state, &provider, limit, sort, refresh).await?,
            })
        }

        // --- Listening reminders + notifications ---
        Request::ArtistAlbums { artist } => {
            let uri = ResourceUri::parse(&artist)?;
            require_resource_kind(&uri, MediaKind::Artist, "artist")?;
            let provider = state.provider_for_uri(&uri).await?;
            require_provider_capability(
                provider.as_ref(),
                "artist albums",
                provider.capabilities().catalog.artist_albums,
            )?;
            let provider_id = provider.id().clone();
            let mut items = collect_artist_albums(provider, uri).await?;
            // Tag each album with whether it's already saved, so clients can
            // offer an "in library only" filter without a per-album lookup.
            // A cold/failed cache simply yields an empty set (all not-saved).
            let saved = state
                .store()
                .saved_album_uris(Some(provider_id.as_str()))
                .await
                .unwrap_or_default();
            for item in &mut items {
                item.in_library = Some(saved.contains(&item.uri));
            }
            Ok(ResponseData::MediaItems { items })
        }
        Request::FollowedArtists { limit, provider } => {
            let (provider_id, provider) = state.provider_or_default(provider.as_ref()).await?;
            // Cache-first; fall back to a live fetch on a cold cache.
            let cached = state
                .store()
                .list_followed_artists(limit, Some(provider_id.as_str()))
                .await?;
            let items = if cached.is_empty() {
                require_provider_capability(
                    provider.as_ref(),
                    "artist library reads",
                    provider.capabilities().library.can_read(&MediaKind::Artist),
                )?;
                let page_request = PageRequest::new(
                    limit
                        .max(1)
                        .min(provider.capabilities().library.max_page_size.unwrap_or(50) as u32),
                    0,
                );
                let fetched = provider
                    .library_items(
                        RequestContext::FOREGROUND,
                        LibraryRequest {
                            kind: MediaKind::Artist,
                            page: page_request.clone(),
                        },
                    )
                    .await?;
                validate_provider_page_offset(&page_request, &fetched, "library_items")?;
                let fetched = fetched.items;
                validate_provider_collection_items(
                    provider.as_ref(),
                    "library_items",
                    &[MediaKind::Artist],
                    &fetched,
                )?;
                fetched.into_iter().take(limit as usize).collect()
            } else {
                cached
            };
            Ok(ResponseData::MediaItems { items })
        }
        Request::AlbumTracks { album } => {
            let uri = ResourceUri::parse(&album)?;
            require_resource_kind(&uri, MediaKind::Album, "album")?;
            let provider = state.provider_for_uri(&uri).await?;
            require_provider_capability(
                provider.as_ref(),
                "album tracks",
                provider.capabilities().catalog.album_tracks,
            )?;
            let items = collect_album_tracks(provider, uri).await?;
            Ok(ResponseData::MediaItems { items })
        }
        Request::RelatedArtists { artist } => {
            let artist = ResourceUri::parse(&artist)?;
            require_resource_kind(&artist, MediaKind::Artist, "artist")?;
            let providers = state.providers().await?;
            let runtime = providers.provider_for_uri(&artist)?;
            require_provider_capability(
                runtime.music().as_ref(),
                "related artists",
                runtime.capabilities().extras.related_artists,
            )?;
            let extras = runtime.extras()?;
            let items = tokio::time::timeout(
                PROVIDER_EXTRAS_TIMEOUT,
                extras.related_artists(RequestContext::FOREGROUND, &artist),
            )
            .await
            .map_err(|_| anyhow::anyhow!("related-artists request timed out"))??;
            validate_provider_collection_items(
                runtime.music().as_ref(),
                "related_artists",
                &[MediaKind::Artist],
                &items,
            )?;
            Ok(ResponseData::MediaItems { items })
        }
        Request::RadioStart { seed_uri, dry_run } => {
            if !dry_run {
                if let Some(replay) =
                    replay_existing_optimistic_mutation(&state, mutation_id, &request_json).await?
                {
                    return Ok(replay);
                }
            }
            if dry_run {
                return Ok(ResponseData::MediaItems {
                    items: discover_radio(&state, &seed_uri, false).await?.items,
                });
            }
            let state_for = state.clone();
            let initial_subject = vec![seed_uri.clone()];
            let queue_lane = mutation_lane;
            return spawn_optimistic_mutation(
                &state,
                OperationKind::QueueAdd,
                operation_source,
                initial_subject,
                "radio queue",
                request_json,
                None,
                Some(spotuify_protocol::ReversalPlan::NotReversible {
                    reason: "the remote queue has no remove operation".to_string(),
                }),
                None,
                mutation_id,
                move |operation_id| async move {
                    let discovery = discover_radio(&state_for, &seed_uri, true).await?;
                    state_for
                        .store()
                        .update_operation_subject_uris(operation_id, &discovery.track_uris)
                        .await?;
                    let provider = discovery.provider;
                    let transport = discovery
                        .transport
                        .ok_or_else(|| ProviderError::unsupported("radio queue transport"))?;
                    let queue_read = discovery.queue_read;
                    let queued_items = discovery.items;
                    // Radio discovery can take seconds and does not mutate
                    // transport state. Serialize only the live queue read and
                    // writes so pause/next/play stay responsive meanwhile.
                    let _queue_guard = match queue_lane {
                        Some(lane) => Some(lane.lock_owned().await),
                        None => None,
                    };
                    let mut reconcile_seq = state_for.bump_mutation_seq();
                    let already_queued = live_queue_uris(transport.as_ref(), queue_read).await;
                    let (queued_items, skipped_dupes) =
                        dedup_queue_items(queued_items, &already_queued);
                    let queued_uris = queued_items
                        .iter()
                        .map(|item| item.uri.clone())
                        .collect::<Vec<_>>();
                    if queued_uris.is_empty() {
                        let message = if skipped_dupes > 0 {
                            state_for.set_active_transport_provider(provider.id().clone());
                            reconcile_seq = state_for.bump_mutation_seq();
                            format!("already queued, skipped {skipped_dupes} item(s)")
                        } else {
                            "radio station returned nothing to queue".to_string()
                        };
                        emit_mutation_finished(&state_for, "radio", &message);
                        spawn_queue_refresh_for_pair(
                            state_for.clone(),
                            provider.clone(),
                            transport.clone(),
                            reconcile_seq,
                        );
                        return Ok(());
                    }

                    let mut started_item = None;
                    let mut applied_items = Vec::new();
                    let mut applied_uris = Vec::new();
                    if let Some(first) = queued_uris.first().cloned() {
                        match queue_one(provider.as_ref(), transport.as_ref(), &first).await? {
                            QueueAttempt::Queued => {
                                applied_items.push(queued_items[0].clone());
                                applied_uris.push(first);
                            }
                            QueueAttempt::NoActiveDevice => {
                                start_embedded_queue_session(
                                    state_for.as_ref(),
                                    provider.as_ref(),
                                    transport.as_ref(),
                                    &first,
                                )
                                .await?;
                                started_item = Some(queued_items[0].clone());
                            }
                        }
                        state_for.set_active_transport_provider(provider.id().clone());
                        reconcile_seq = state_for.bump_mutation_seq();
                    }

                    for (queue_uri, queue_item) in
                        queued_uris.iter().zip(queued_items.iter()).skip(1)
                    {
                        let mut attempt = 0u32;
                        loop {
                            match queue_one(provider.as_ref(), transport.as_ref(), queue_uri).await
                            {
                                Ok(QueueAttempt::Queued) => {
                                    applied_items.push(queue_item.clone());
                                    applied_uris.push(queue_uri.clone());
                                    break;
                                }
                                Ok(QueueAttempt::NoActiveDevice)
                                    if started_item.is_some() && attempt < 6 =>
                                {
                                    attempt += 1;
                                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                                }
                                Ok(QueueAttempt::NoActiveDevice) => {
                                    return Err(report_partial_queue_application(
                                        state_for.clone(),
                                        provider.clone(),
                                        transport.clone(),
                                        applied_items,
                                        applied_uris,
                                        &already_queued,
                                        started_item.clone(),
                                        reconcile_seq,
                                        anyhow::Error::new(ProviderError::NoActiveDevice).context(
                                            format!("radio queue add for {queue_uri} failed"),
                                        ),
                                    )
                                    .await);
                                }
                                Err(error) => {
                                    return Err(report_partial_queue_application(
                                        state_for.clone(),
                                        provider.clone(),
                                        transport.clone(),
                                        applied_items,
                                        applied_uris,
                                        &already_queued,
                                        started_item.clone(),
                                        reconcile_seq,
                                        error.context(format!(
                                            "radio queue add for {queue_uri} failed"
                                        )),
                                    )
                                    .await);
                                }
                            }
                        }
                    }

                    let queue = cache_optimistic_queue_application(
                        &state_for,
                        provider.id(),
                        started_item.clone(),
                        applied_items,
                        &already_queued,
                    )
                    .await;
                    let skip_note = if skipped_dupes > 0 {
                        format!(", skipped {skipped_dupes} already queued")
                    } else {
                        String::new()
                    };
                    let message = if started_item.is_some() {
                        format!(
                            "playing radio now, queued {} item(s){skip_note}",
                            applied_uris.len()
                        )
                    } else {
                        format!("queued {} radio item(s){skip_note}", applied_uris.len())
                    };
                    state_for.emit_event(DaemonEvent::QueueChanged {
                        action: "radio".to_string(),
                        uris: applied_uris.clone(),
                        queue,
                    });
                    state_for.warm_queue_uris(applied_uris);
                    spawn_queue_refresh_for_pair(
                        state_for.clone(),
                        provider,
                        transport,
                        reconcile_seq,
                    );
                    emit_mutation_finished(&state_for, "radio", &message);
                    Ok(())
                },
            )
            .await;
        }
        Request::LibrarySave { uri, current } => {
            let state_for = state.clone();
            let uri_for = uri.clone();
            let provider_mutation_id = mutation_id.map_or_else(Uuid::now_v7, |id| id.0);
            spawn_optimistic_mutation(
                &state,
                OperationKind::LibrarySave,
                operation_source,
                uri.iter().cloned().collect(),
                "save",
                request_json.clone(),
                None,
                None,
                mutation_lane,
                mutation_id,
                move |op_id| async move {
                    // Resolve the URI early so we can register a real
                    // reversal plan. SaveCurrent uses the daemon-owned
                    // playback snapshot instead of hitting GET /me/player.
                    let resolved_uri = match uri_for.clone() {
                        Some(u) => Some(u),
                        None if current => state_for.snapshot_playback().item.map(|item| item.uri),
                        None => None,
                    };
                    let event_uris = resolved_uri.iter().cloned().collect::<Vec<_>>();
                    if let Some(ref real_uri) = resolved_uri {
                        let pre_state = spotuify_protocol::PreState::LibrarySave {
                            uri: real_uri.clone(),
                            prior_was_saved: false,
                        };
                        let plan = spotuify_protocol::ReversalPlan::LibraryUnsave {
                            uri: real_uri.clone(),
                        };
                        state_for
                            .store()
                            .update_operation_plan(op_id, Some(&pre_state), Some(&plan))
                            .await?;
                        state_for
                            .store()
                            .update_operation_subject_uris(op_id, std::slice::from_ref(real_uri))
                            .await?;
                    }
                    let save_uri = resolved_uri
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("nothing is playing"))?;
                    apply_uri_mutation(&state_for, provider_mutation_id, &save_uri, |uri| {
                        Mutation::LibrarySave { uris: vec![uri] }
                    })
                    .await?;
                    let provider = provider_id_for_uri(&state_for, &save_uri).await?;
                    let message = "save".to_string();
                    state_for.emit_event(DaemonEvent::LibraryChanged {
                        action: "save".to_string(),
                        uris: event_uris,
                        provider: Some(provider),
                    });
                    emit_mutation_finished(&state_for, "save", &message);
                    Ok(())
                },
            )
            .await
        }
        Request::LibraryUnsave { uri } => {
            let state_for = state.clone();
            let provider_mutation_id = mutation_id.map_or_else(Uuid::now_v7, |id| id.0);
            spawn_optimistic_mutation(
                &state,
                OperationKind::LibraryUnsave,
                operation_source,
                vec![uri.clone()],
                "unsave",
                request_json.clone(),
                Some(spotuify_protocol::PreState::LibrarySave {
                    uri: uri.clone(),
                    prior_was_saved: true,
                }),
                Some(spotuify_protocol::ReversalPlan::LibrarySave {
                    uri: uri.clone(),
                    prior_added_at_ms: None,
                }),
                mutation_lane,
                mutation_id,
                move |_op_id| async move {
                    apply_uri_mutation(&state_for, provider_mutation_id, &uri, |uri| {
                        Mutation::LibraryUnsave { uris: vec![uri] }
                    })
                    .await?;
                    let provider = provider_id_for_uri(&state_for, &uri).await?;
                    let message = format!("Unsaved {uri}");
                    state_for.emit_event(DaemonEvent::LibraryChanged {
                        action: "unsave".to_string(),
                        uris: vec![uri.clone()],
                        provider: Some(provider),
                    });
                    emit_mutation_finished(&state_for, "unsave", &message);
                    Ok(())
                },
            )
            .await
        }
        Request::ArtistFollow { artist } => {
            let state_for = state.clone();
            let artist_for = artist.clone();
            let provider_mutation_id = mutation_id.map_or_else(Uuid::now_v7, |id| id.0);
            spawn_optimistic_mutation(
                &state,
                OperationKind::ArtistFollow,
                operation_source,
                vec![artist.clone()],
                "artist-follow",
                request_json.clone(),
                None,
                None,
                mutation_lane,
                mutation_id,
                move |_op_id| async move {
                    apply_uri_mutation(&state_for, provider_mutation_id, &artist_for, |uri| {
                        Mutation::Follow { uris: vec![uri] }
                    })
                    .await?;
                    let provider = provider_id_for_uri(&state_for, &artist_for).await?;
                    if let Err(err) = state_for
                        .store()
                        .set_artist_followed(&artist_for, true)
                        .await
                    {
                        tracing::warn!(error = %err, "failed to mark artist followed in cache");
                    }
                    let message = format!("Followed {artist_for}");
                    state_for.emit_event(DaemonEvent::LibraryChanged {
                        action: "artist-follow".to_string(),
                        uris: vec![artist_for.clone()],
                        provider: Some(provider),
                    });
                    emit_mutation_finished(&state_for, "artist-follow", &message);
                    Ok(())
                },
            )
            .await
        }
        Request::ArtistUnfollow { artist } => {
            let state_for = state.clone();
            let artist_for = artist.clone();
            let provider_mutation_id = mutation_id.map_or_else(Uuid::now_v7, |id| id.0);
            spawn_optimistic_mutation(
                &state,
                OperationKind::ArtistUnfollow,
                operation_source,
                vec![artist.clone()],
                "artist-unfollow",
                request_json.clone(),
                None,
                None,
                mutation_lane,
                mutation_id,
                move |_op_id| async move {
                    apply_uri_mutation(&state_for, provider_mutation_id, &artist_for, |uri| {
                        Mutation::Unfollow { uris: vec![uri] }
                    })
                    .await?;
                    let provider = provider_id_for_uri(&state_for, &artist_for).await?;
                    if let Err(err) = state_for
                        .store()
                        .set_artist_followed(&artist_for, false)
                        .await
                    {
                        tracing::warn!(error = %err, "failed to clear artist followed in cache");
                    }
                    let message = format!("Unfollowed {artist_for}");
                    state_for.emit_event(DaemonEvent::LibraryChanged {
                        action: "artist-unfollow".to_string(),
                        uris: vec![artist_for.clone()],
                        provider: Some(provider),
                    });
                    emit_mutation_finished(&state_for, "artist-unfollow", &message);
                    Ok(())
                },
            )
            .await
        }
        _ => unreachable!("non-library request routed to library dispatcher"),
    }
}

fn saved_tracks_cache_fallback_allowed(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<ProviderError>(),
        Some(
            ProviderError::RateLimited { .. }
                | ProviderError::Network(_)
                | ProviderError::Transient { .. }
                | ProviderError::Upstream {
                    status: 500..=599,
                    ..
                }
        )
    )
}

async fn provider_id_for_uri(
    state: &DaemonState,
    uri: &str,
) -> anyhow::Result<spotuify_core::ProviderId> {
    let resource = ResourceUri::parse(uri)?;
    let providers = state.providers().await?;
    Ok(providers.provider_for_uri(&resource)?.id().clone())
}

async fn fetch_saved_tracks_page(
    provider: &dyn MusicProvider,
    limit: u32,
    offset: u32,
) -> anyhow::Result<(Vec<MediaItem>, u64)> {
    if limit == 0 {
        return Err(ProviderError::InvalidInput {
            field: "limit".to_string(),
            message: "saved-track page limit must be greater than zero".to_string(),
        }
        .into());
    }

    require_provider_capability(
        provider,
        "track library reads",
        provider.capabilities().library.can_read(&MediaKind::Track),
    )?;

    let max_page = u32::try_from(provider.capabilities().library.max_page_size.unwrap_or(50))
        .unwrap_or(u32::MAX)
        .max(1);
    let mut request = PageRequest::new(limit.min(max_page), u64::from(offset));
    let mut items = Vec::new();
    let mut exact_total = None;
    let mut seen_cursors = std::collections::HashSet::new();

    for page_index in 0..PROVIDER_PAGINATION_MAX_PAGES {
        let page = provider
            .library_items(
                RequestContext::FOREGROUND,
                LibraryRequest {
                    kind: MediaKind::Track,
                    page: request.clone(),
                },
            )
            .await?;
        validate_provider_page_offset(&request, &page, "library_items")?;
        validate_provider_collection_items(
            provider,
            "library_items",
            &[MediaKind::Track],
            &page.items,
        )?;
        if let Some(total) = page.total {
            exact_total = Some(total);
        }
        let fetched = page.items.len() as u64;
        let logical_offset = request.offset.saturating_add(fetched);
        let remaining = limit as usize - items.len();
        items.extend(page.items.into_iter().take(remaining));
        let continuation = page.next;
        let lower_bound = match continuation.as_ref() {
            Some(spotuify_core::PageContinuation::Offset(next_offset)) => {
                logical_offset.max(*next_offset).saturating_add(1)
            }
            Some(spotuify_core::PageContinuation::Cursor(_)) => logical_offset.saturating_add(1),
            None => logical_offset,
        };

        if items.len() >= limit as usize {
            return Ok((items, exact_total.unwrap_or(lower_bound)));
        }
        let Some(continuation) = continuation else {
            return Ok((items, exact_total.unwrap_or(lower_bound)));
        };

        request = next_provider_page(
            &request,
            continuation,
            logical_offset,
            &mut seen_cursors,
            page_index + 1,
            "saved tracks",
        )?;
        request.limit = limit.saturating_sub(items.len() as u32).min(max_page);
    }

    Err(ProviderError::Provider(format!(
        "saved track pagination exceeded {PROVIDER_PAGINATION_MAX_PAGES} pages"
    ))
    .into())
}

async fn collect_artist_albums(
    provider: Arc<dyn MusicProvider>,
    uri: ResourceUri,
) -> anyhow::Result<Vec<MediaItem>> {
    let limit = provider
        .capabilities()
        .catalog
        .artist_albums_max_page_size
        .unwrap_or(50) as u32;
    let mut request = PageRequest::new(limit.max(1), 0);
    let mut items = Vec::new();
    let mut seen_uris = std::collections::HashSet::new();
    let mut seen_cursors = std::collections::HashSet::new();
    for page_index in 0..PROVIDER_PAGINATION_MAX_PAGES {
        let page = provider
            .artist_albums(
                RequestContext::FOREGROUND,
                CollectionRequest {
                    uri: uri.clone(),
                    page: request.clone(),
                },
            )
            .await?;
        validate_provider_page_offset(&request, &page, "artist_albums")?;
        validate_provider_collection_items(
            provider.as_ref(),
            "artist_albums",
            &[MediaKind::Album],
            &page.items,
        )?;
        let logical_offset = request.offset.saturating_add(page.items.len() as u64);
        items.extend(
            page.items
                .into_iter()
                .filter(|item| seen_uris.insert(item.uri.clone())),
        );
        let Some(next) = page.next else {
            return Ok(items);
        };
        request = next_provider_page(
            &request,
            next,
            logical_offset,
            &mut seen_cursors,
            page_index + 1,
            "artist albums",
        )?;
    }
    Err(ProviderError::Provider(format!(
        "artist album pagination exceeded {PROVIDER_PAGINATION_MAX_PAGES} pages"
    ))
    .into())
}

async fn collect_album_tracks(
    provider: Arc<dyn MusicProvider>,
    uri: ResourceUri,
) -> anyhow::Result<Vec<MediaItem>> {
    let limit = provider
        .capabilities()
        .catalog
        .album_tracks_max_page_size
        .unwrap_or(50) as u32;
    let mut request = PageRequest::new(limit.max(1), 0);
    let mut items = Vec::new();
    let mut seen_cursors = std::collections::HashSet::new();
    for page_index in 0..PROVIDER_PAGINATION_MAX_PAGES {
        let page = provider
            .album_tracks(
                RequestContext::FOREGROUND,
                CollectionRequest {
                    uri: uri.clone(),
                    page: request.clone(),
                },
            )
            .await?;
        validate_provider_page_offset(&request, &page, "album_tracks")?;
        validate_provider_collection_items(
            provider.as_ref(),
            "album_tracks",
            &[MediaKind::Track],
            &page.items,
        )?;
        items.extend(page.items);
        let Some(next) = page.next else {
            return Ok(items);
        };
        request = next_provider_page(
            &request,
            next,
            items.len() as u64,
            &mut seen_cursors,
            page_index + 1,
            "album tracks",
        )?;
    }
    Err(ProviderError::Provider(format!(
        "album track pagination exceeded {PROVIDER_PAGINATION_MAX_PAGES} pages"
    ))
    .into())
}

struct RadioDiscovery {
    provider: Arc<dyn MusicProvider>,
    transport: Option<Arc<dyn spotuify_core::RemoteTransport>>,
    queue_read: bool,
    track_uris: Vec<String>,
    items: Vec<MediaItem>,
}

async fn discover_radio(
    state: &DaemonState,
    seed_uri: &str,
    require_queue: bool,
) -> anyhow::Result<RadioDiscovery> {
    let seed = ResourceUri::parse(seed_uri)?;
    let providers = state.providers().await?;
    let runtime = providers.provider_for_uri(&seed)?;
    require_provider_capability(
        runtime.music().as_ref(),
        "radio",
        runtime.capabilities().extras.radio,
    )?;
    let extras = runtime.extras()?;
    let track_resources = tokio::time::timeout(
        PROVIDER_EXTRAS_TIMEOUT,
        extras.radio(RequestContext::FOREGROUND, &seed),
    )
    .await
    .map_err(|_| anyhow::anyhow!("radio station request timed out"))??;
    if let Some(resource) = track_resources.iter().find(|resource| {
        resource.scheme() != runtime.uri_scheme() || resource.kind() != MediaKind::Track
    }) {
        return Err(ProviderError::InvalidInput {
            field: "radio.kind".to_string(),
            message: format!(
                "provider {} returned non-track or foreign radio resource `{resource}`",
                runtime.id()
            ),
        }
        .into());
    }
    let track_uris = track_resources
        .iter()
        .map(ResourceUri::as_uri)
        .collect::<Vec<_>>();
    if track_uris.is_empty() {
        anyhow::bail!(
            "radio station returned no tracks; the provider service may have changed \
             or `{seed_uri}` has no station"
        );
    }

    let mut items_by_uri = state
        .store()
        .media_items_by_uris(&track_uris)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|item| (item.uri.clone(), item))
        .collect::<std::collections::HashMap<_, _>>();
    let items = track_uris
        .iter()
        .map(|uri| {
            items_by_uri.remove(uri).unwrap_or_else(|| MediaItem {
                id: ResourceUri::parse(uri)
                    .ok()
                    .map(|resource| resource.bare_id().to_string()),
                uri: uri.clone(),
                kind: MediaKind::Track,
                source: Some(spotuify_core::ItemSource::Provider(
                    runtime.id().to_string(),
                )),
                ..MediaItem::default()
            })
        })
        .collect();
    let (transport, queue_read) = if require_queue {
        require_provider_capability(
            runtime.music().as_ref(),
            "queue add",
            runtime
                .capabilities()
                .transport
                .as_ref()
                .is_some_and(|caps| caps.queue_add),
        )?;
        (
            Some(runtime.transport()?),
            runtime
                .capabilities()
                .transport
                .as_ref()
                .is_some_and(|caps| caps.queue_read),
        )
    } else {
        (None, false)
    };
    Ok(RadioDiscovery {
        provider: runtime.music(),
        transport,
        queue_read,
        track_uris,
        items,
    })
}

async fn apply_uri_mutation(
    state: &DaemonState,
    mutation_id: Uuid,
    uri: &str,
    mutation: impl FnOnce(ResourceUri) -> Mutation,
) -> anyhow::Result<()> {
    let uri = ResourceUri::parse(uri)?;
    let provider = state.provider_for_uri(&uri).await?;
    let mutation = mutation(uri);
    apply_provider_mutation_checked(provider.as_ref(), mutation_id, &mutation).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic, clippy::unwrap_used)]

    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use spotuify_core::{
        CatalogCaps, LibraryCaps, LibraryRequest, PageContinuation, ProviderCaps, ProviderExtras,
        ProviderExtrasCaps, ProviderId, ProviderPage, ProviderResult, Queue, RemoteTransport,
        TransportCommand, TransportOutcome, UriScheme,
    };
    use spotuify_protocol::{
        IpcErrorKind, IpcMessage, IpcPayload, MutationId, PlaybackCommand, ReceiptId,
        ReceiptStatus, Response,
    };
    use spotuify_provider_fake::FakeProvider;

    use super::*;
    use crate::provider_registry::{
        ProviderPlayer, ProviderRegistry, ProviderRuntime, TransportRecovery,
    };

    struct RadioProvider {
        inner: FakeProvider,
        tracks: Vec<ResourceUri>,
        queue_calls: AtomicUsize,
        queue_reads: AtomicUsize,
        radio_calls: AtomicUsize,
        radio_fails: AtomicBool,
        blocked_radio_call: Option<usize>,
        radio_call_release: tokio::sync::Notify,
        blocked_queue_read: Option<usize>,
        queue_read_release: tokio::sync::Notify,
        no_active_queue_call: Option<usize>,
        fail_queue_call: Option<(usize, ProviderError)>,
    }

    impl RadioProvider {
        fn new(
            namespace: &str,
            no_active_queue_call: Option<usize>,
            fail_queue_call: Option<(usize, ProviderError)>,
        ) -> Self {
            Self {
                inner: FakeProvider::isolated(namespace).unwrap(),
                tracks: ["track-1", "track-2"]
                    .into_iter()
                    .map(|id| ResourceUri::parse(&format!("{namespace}:track:{id}")).unwrap())
                    .collect(),
                queue_calls: AtomicUsize::new(0),
                queue_reads: AtomicUsize::new(0),
                radio_calls: AtomicUsize::new(0),
                radio_fails: AtomicBool::new(false),
                blocked_radio_call: None,
                radio_call_release: tokio::sync::Notify::new(),
                blocked_queue_read: None,
                queue_read_release: tokio::sync::Notify::new(),
                no_active_queue_call,
                fail_queue_call,
            }
        }

        fn with_blocked_queue_read(mut self, call: usize) -> Self {
            self.blocked_queue_read = Some(call);
            self
        }

        fn with_blocked_radio_call(mut self, call: usize) -> Self {
            self.blocked_radio_call = Some(call);
            self
        }

        fn fail_radio(&self) {
            self.radio_fails.store(true, Ordering::SeqCst);
        }

        fn release_queue_read(&self) {
            self.queue_read_release.notify_one();
        }

        fn release_radio_call(&self) {
            self.radio_call_release.notify_one();
        }
    }

    #[async_trait]
    impl MusicProvider for RadioProvider {
        fn id(&self) -> &ProviderId {
            MusicProvider::id(&self.inner)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(&self.inner)
        }

        fn display_name(&self) -> &str {
            "Radio test provider"
        }

        fn capabilities(&self) -> ProviderCaps {
            let mut caps = MusicProvider::capabilities(&self.inner);
            caps.extras.radio = true;
            caps
        }
    }

    #[async_trait]
    impl RemoteTransport for RadioProvider {
        fn provider_id(&self) -> &ProviderId {
            MusicProvider::id(self)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(self)
        }

        async fn queue(&self, context: RequestContext) -> ProviderResult<Queue> {
            let call = self.queue_reads.fetch_add(1, Ordering::SeqCst) + 1;
            if self.blocked_queue_read == Some(call) {
                self.queue_read_release.notified().await;
            }
            RemoteTransport::queue(&self.inner, context).await
        }

        async fn execute(
            &self,
            context: RequestContext,
            command: TransportCommand,
        ) -> ProviderResult<TransportOutcome> {
            if matches!(&command, TransportCommand::QueueAdd(_)) {
                let call = self.queue_calls.fetch_add(1, Ordering::SeqCst) + 1;
                if self.no_active_queue_call == Some(call) {
                    return Err(ProviderError::NoActiveDevice);
                }
                if let Some((failure_call, error)) = &self.fail_queue_call {
                    if *failure_call == call {
                        return Err(error.clone());
                    }
                }
            }
            RemoteTransport::execute(&self.inner, context, command).await
        }
    }

    #[async_trait]
    impl ProviderExtras for RadioProvider {
        fn provider_id(&self) -> &ProviderId {
            MusicProvider::id(self)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(self)
        }

        fn capabilities(&self) -> ProviderExtrasCaps {
            ProviderExtrasCaps {
                radio: true,
                ..Default::default()
            }
        }

        async fn radio(
            &self,
            _context: RequestContext,
            _seed: &ResourceUri,
        ) -> ProviderResult<Vec<ResourceUri>> {
            let should_fail = self.radio_fails.load(Ordering::SeqCst);
            let call = self.radio_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if self.blocked_radio_call == Some(call) {
                self.radio_call_release.notified().await;
            }
            if should_fail {
                return Err(ProviderError::Network(
                    "injected radio discovery failure".to_string(),
                ));
            }
            Ok(self.tracks.clone())
        }
    }

    struct DaemonTestEnv {
        _temp: tempfile::TempDir,
    }

    impl DaemonTestEnv {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            std::env::set_var("SPOTUIFY_CACHE_DB", temp.path().join("cache.sqlite3"));
            std::env::set_var(
                "SPOTUIFY_ANALYTICS_DB",
                temp.path().join("analytics.sqlite3"),
            );
            std::env::set_var("SPOTUIFY_SEARCH_INDEX", temp.path().join("search-index"));
            std::env::set_var("SPOTUIFY_RUNTIME_DIR", temp.path().join("runtime"));
            std::env::set_var("SPOTUIFY_CONFIG", temp.path().join("spotuify.toml"));
            Self { _temp: temp }
        }
    }

    impl Drop for DaemonTestEnv {
        fn drop(&mut self) {
            for key in [
                "SPOTUIFY_CACHE_DB",
                "SPOTUIFY_ANALYTICS_DB",
                "SPOTUIFY_SEARCH_INDEX",
                "SPOTUIFY_RUNTIME_DIR",
                "SPOTUIFY_CONFIG",
            ] {
                std::env::remove_var(key);
            }
        }
    }

    fn mutation_receipt_id(response: &ResponseData) -> ReceiptId {
        let ResponseData::Mutation { receipt } = response else {
            panic!("expected mutation response")
        };
        receipt.receipt_id.expect("pending receipt id")
    }

    async fn wait_for_receipt(
        state: &DaemonState,
        receipt_id: ReceiptId,
    ) -> spotuify_protocol::Receipt {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let receipt = state.store().get_receipt(receipt_id).await.unwrap();
                if receipt.status != ReceiptStatus::Pending {
                    return receipt;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("radio mutation should finalize")
    }

    async fn wait_for_queue_event(
        events: &mut tokio::sync::broadcast::Receiver<IpcMessage>,
        expected_action: &str,
    ) -> (Vec<String>, Option<Queue>) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let IpcPayload::Event(DaemonEvent::QueueChanged {
                    action,
                    uris,
                    queue,
                }) = events.recv().await.unwrap().payload
                {
                    if action == expected_action {
                        return (uris, queue);
                    }
                }
            }
        })
        .await
        .expect("expected actionable queue event")
    }

    struct CursorLibraryProvider {
        id: ProviderId,
        scheme: UriScheme,
        exact_total: Option<u64>,
    }

    struct OffsetLibraryProvider {
        id: ProviderId,
        scheme: UriScheme,
    }

    struct CountingLibraryProvider {
        id: ProviderId,
        scheme: UriScheme,
        calls: AtomicUsize,
    }

    struct DuplicateArtistAlbumsProvider {
        id: ProviderId,
        scheme: UriScheme,
        calls: AtomicUsize,
    }

    impl DuplicateArtistAlbumsProvider {
        fn new() -> Self {
            Self {
                id: ProviderId::new("duplicate-albums").unwrap(),
                scheme: UriScheme::new("duplicate-albums").unwrap(),
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl MusicProvider for DuplicateArtistAlbumsProvider {
        fn id(&self) -> &ProviderId {
            &self.id
        }

        fn uri_scheme(&self) -> &UriScheme {
            &self.scheme
        }

        fn display_name(&self) -> &str {
            "Duplicate Albums"
        }

        fn capabilities(&self) -> ProviderCaps {
            ProviderCaps {
                catalog: CatalogCaps {
                    artist_albums: true,
                    artist_albums_max_page_size: Some(2),
                    ..Default::default()
                },
                ..Default::default()
            }
        }

        async fn artist_albums(
            &self,
            _context: RequestContext,
            request: CollectionRequest,
        ) -> ProviderResult<ProviderPage<MediaItem>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let (ids, next) = match request.page.offset {
                0 => (vec!["one", "shared"], Some(PageContinuation::Offset(2))),
                2 => (vec!["shared", "three"], None),
                offset => {
                    return Err(ProviderError::InvalidInput {
                        field: "offset".to_string(),
                        message: format!("unexpected artist-album offset {offset}"),
                    })
                }
            };
            Ok(ProviderPage {
                items: ids
                    .into_iter()
                    .map(|id| MediaItem {
                        uri: format!("duplicate-albums:album:{id}"),
                        name: id.to_string(),
                        kind: MediaKind::Album,
                        ..Default::default()
                    })
                    .collect(),
                requested_offset: request.page.offset,
                total: Some(4),
                next,
            })
        }
    }

    impl CountingLibraryProvider {
        fn new() -> Self {
            Self {
                id: ProviderId::new("counting-library").unwrap(),
                scheme: UriScheme::new("counting-library").unwrap(),
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl MusicProvider for CountingLibraryProvider {
        fn id(&self) -> &ProviderId {
            &self.id
        }

        fn uri_scheme(&self) -> &UriScheme {
            &self.scheme
        }

        fn display_name(&self) -> &str {
            "Counting Library"
        }

        fn capabilities(&self) -> ProviderCaps {
            ProviderCaps {
                library: LibraryCaps {
                    read_kinds: vec![MediaKind::Track],
                    max_page_size: Some(50),
                    ..Default::default()
                },
                ..Default::default()
            }
        }

        async fn library_items(
            &self,
            _context: RequestContext,
            _request: LibraryRequest,
        ) -> ProviderResult<ProviderPage<MediaItem>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            unreachable!("zero limits must be rejected before provider dispatch")
        }
    }

    impl OffsetLibraryProvider {
        fn new() -> Self {
            Self {
                id: ProviderId::new("offset-library").unwrap(),
                scheme: UriScheme::new("offset-library").unwrap(),
            }
        }
    }

    #[async_trait]
    impl MusicProvider for OffsetLibraryProvider {
        fn id(&self) -> &ProviderId {
            &self.id
        }

        fn uri_scheme(&self) -> &UriScheme {
            &self.scheme
        }

        fn display_name(&self) -> &str {
            "Offset Library"
        }

        fn capabilities(&self) -> ProviderCaps {
            ProviderCaps {
                library: LibraryCaps {
                    read_kinds: vec![MediaKind::Track],
                    max_page_size: Some(2),
                    ..Default::default()
                },
                ..Default::default()
            }
        }

        async fn library_items(
            &self,
            _context: RequestContext,
            request: LibraryRequest,
        ) -> ProviderResult<ProviderPage<MediaItem>> {
            let (names, next) = match (request.page.offset, request.page.cursor.as_deref()) {
                (10, None) => (vec!["Alpha", "Bravo"], 20),
                (20, None) => (vec!["Charlie"], 50),
                _ => {
                    return Err(ProviderError::InvalidInput {
                        field: "page".to_string(),
                        message: format!("unexpected page request: {:?}", request.page),
                    })
                }
            };
            Ok(ProviderPage {
                items: names
                    .into_iter()
                    .map(|name| MediaItem {
                        uri: format!("offset-library:track:{}", name.to_lowercase()),
                        name: name.to_string(),
                        kind: MediaKind::Track,
                        ..Default::default()
                    })
                    .collect(),
                requested_offset: request.page.offset,
                total: None,
                next: Some(PageContinuation::Offset(next)),
            })
        }
    }

    impl CursorLibraryProvider {
        fn new(exact_total: Option<u64>) -> Self {
            Self {
                id: ProviderId::new("cursor-library").unwrap(),
                scheme: UriScheme::new("cursor-library").unwrap(),
                exact_total,
            }
        }
    }

    #[async_trait]
    impl MusicProvider for CursorLibraryProvider {
        fn id(&self) -> &ProviderId {
            &self.id
        }

        fn uri_scheme(&self) -> &UriScheme {
            &self.scheme
        }

        fn display_name(&self) -> &str {
            "Cursor Library"
        }

        fn capabilities(&self) -> ProviderCaps {
            ProviderCaps {
                library: LibraryCaps {
                    read_kinds: vec![MediaKind::Track],
                    max_page_size: Some(2),
                    ..Default::default()
                },
                ..Default::default()
            }
        }

        async fn library_items(
            &self,
            _context: RequestContext,
            request: LibraryRequest,
        ) -> ProviderResult<ProviderPage<MediaItem>> {
            let (names, next) = match (request.page.offset, request.page.cursor.as_deref()) {
                (5, None) => (vec!["Alpha", "Bravo"], Some("cursor-2")),
                (7, Some("cursor-2")) => (vec!["Charlie"], Some("cursor-3")),
                _ => {
                    return Err(ProviderError::InvalidInput {
                        field: "page".to_string(),
                        message: format!("unexpected page request: {:?}", request.page),
                    })
                }
            };
            Ok(ProviderPage {
                items: names
                    .into_iter()
                    .map(|name| MediaItem {
                        uri: format!("cursor-library:track:{}", name.to_lowercase()),
                        name: name.to_string(),
                        kind: MediaKind::Track,
                        ..Default::default()
                    })
                    .collect(),
                requested_offset: request.page.offset,
                total: self.exact_total,
                next: next.map(|cursor| PageContinuation::Cursor(cursor.to_string())),
            })
        }
    }

    #[tokio::test]
    async fn artist_album_collection_deduplicates_uris_across_provider_pages() {
        let provider = Arc::new(DuplicateArtistAlbumsProvider::new());
        let music: Arc<dyn MusicProvider> = provider.clone();
        let items = collect_artist_albums(
            music,
            ResourceUri::parse("duplicate-albums:artist:artist-1").unwrap(),
        )
        .await
        .expect("artist albums collect");

        assert_eq!(
            items
                .iter()
                .map(|item| item.uri.as_str())
                .collect::<Vec<_>>(),
            vec![
                "duplicate-albums:album:one",
                "duplicate-albums:album:shared",
                "duplicate-albums:album:three",
            ]
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn radio_routes_queue_and_active_owner_to_the_seed_provider() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = DaemonTestEnv::new();
        let default = Arc::new(FakeProvider::isolated("radio-default").unwrap());
        let selected = Arc::new(RadioProvider::new("radio-selected", None, None));
        let selected_extras: Arc<dyn ProviderExtras> = selected.clone();
        let registry = ProviderRegistry::new(
            MusicProvider::id(default.as_ref()).clone(),
            [
                ProviderRuntime::with_transport(default.clone()).unwrap(),
                ProviderRuntime::with_transport_and_extras(selected.clone(), selected_extras)
                    .unwrap(),
            ],
        )
        .unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let mut events = state.event_tx.subscribe();

        let response = crate::handler::dispatch_with_mutation(
            state.clone(),
            Request::RadioStart {
                seed_uri: "radio-selected:track:seed".to_string(),
                dry_run: false,
            },
            None,
            Some(MutationId::new_v7()),
        )
        .await
        .unwrap();
        let receipt = wait_for_receipt(&state, mutation_receipt_id(&response)).await;
        assert_eq!(receipt.status, ReceiptStatus::Confirmed);
        assert_eq!(
            state.active_transport_provider().as_ref(),
            Some(MusicProvider::id(selected.as_ref()))
        );
        assert_eq!(selected.radio_calls.load(Ordering::SeqCst), 1);
        assert_eq!(selected.queue_calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            default
                .observed_requests()
                .await
                .iter()
                .filter(|request| request.operation == "transport.execute")
                .count(),
            0,
            "radio must not enqueue through the default provider"
        );

        let expected = vec![
            "radio-selected:track:track-1".to_string(),
            "radio-selected:track:track-2".to_string(),
        ];
        let (event_uris, queue) = wait_for_queue_event(&mut events, "radio").await;
        assert_eq!(event_uris, expected);
        assert_eq!(
            queue
                .expect("radio event carries reconciled queue state")
                .items
                .iter()
                .map(|item| item.uri.clone())
                .collect::<Vec<_>>(),
            expected
        );
        let cached = state
            .store()
            .latest_provider_queue(500, MusicProvider::id(selected.as_ref()))
            .await
            .unwrap()
            .expect("radio queue is cached under selected provider");
        assert!(cached
            .items
            .iter()
            .all(|item| item.uri.starts_with("radio-selected:")));

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn concurrent_same_id_radio_requests_claim_before_discovery() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = DaemonTestEnv::new();
        let provider =
            Arc::new(RadioProvider::new("radio-concurrent", None, None).with_blocked_radio_call(1));
        let extras: Arc<dyn ProviderExtras> = provider.clone();
        let runtime = ProviderRuntime::with_transport_and_extras(provider.clone(), extras).unwrap();
        let registry =
            ProviderRegistry::new(MusicProvider::id(provider.as_ref()).clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let mutation_id = MutationId::new_v7();
        let request = Request::RadioStart {
            seed_uri: "radio-concurrent:track:seed".to_string(),
            dry_run: false,
        };

        let first_state = state.clone();
        let first_request = request.clone();
        let first_task = tokio::spawn(async move {
            crate::handler::dispatch_with_mutation(
                first_state,
                first_request,
                None,
                Some(mutation_id),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while provider.radio_calls.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("claimed body must enter radio discovery");
        let seq_while_discovery_is_blocked = state.current_mutation_seq();
        provider.fail_radio();

        let second =
            crate::handler::dispatch_with_mutation(state.clone(), request, None, Some(mutation_id))
                .await
                .expect("racing request must replay the in-flight claim");
        let ResponseData::Mutation {
            receipt: second_receipt,
        } = second
        else {
            panic!("expected replayed mutation receipt")
        };
        assert!(second_receipt.replayed);
        assert_eq!(second_receipt.status, Some(ReceiptStatus::Pending));
        assert_eq!(provider.radio_calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.queue_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            state.current_mutation_seq(),
            seq_while_discovery_is_blocked,
            "replayed contender must not enter the claimed body"
        );

        provider.release_radio_call();
        let first = first_task.await.unwrap().unwrap();
        let ResponseData::Mutation {
            receipt: first_receipt,
        } = first
        else {
            panic!("expected initial mutation receipt")
        };
        assert!(!first_receipt.replayed);
        assert_eq!(first_receipt.status, Some(ReceiptStatus::Pending));
        assert_eq!(second_receipt.receipt_id, first_receipt.receipt_id);
        assert_eq!(second_receipt.mutation_id, first_receipt.mutation_id);
        assert_eq!(second_receipt.action, first_receipt.action);
        assert_eq!(second_receipt.message, first_receipt.message);
        assert_eq!(first_receipt.message, "radio queue queued");
        let first_receipt_id = first_receipt.receipt_id.expect("pending receipt id");
        assert_eq!(
            wait_for_receipt(&state, first_receipt_id).await.status,
            ReceiptStatus::Confirmed
        );
        assert_eq!(provider.radio_calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.queue_calls.load(Ordering::SeqCst), 2);

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn radio_discovery_does_not_hold_the_transport_lane() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = DaemonTestEnv::new();
        let provider =
            Arc::new(RadioProvider::new("radio-lane", None, None).with_blocked_radio_call(1));
        let extras: Arc<dyn ProviderExtras> = provider.clone();
        let runtime = ProviderRuntime::with_transport_and_extras(provider.clone(), extras).unwrap();
        let registry =
            ProviderRegistry::new(MusicProvider::id(provider.as_ref()).clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());

        let radio = crate::handler::dispatch_with_mutation(
            state.clone(),
            Request::RadioStart {
                seed_uri: "radio-lane:track:seed".to_string(),
                dry_run: false,
            },
            None,
            Some(MutationId::new_v7()),
        )
        .await
        .expect("radio mutation accepted");
        let radio_receipt_id = mutation_receipt_id(&radio);
        tokio::time::timeout(Duration::from_secs(5), async {
            while provider.radio_calls.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("radio discovery must reach its barrier");

        let pause = crate::handler::dispatch_with_mutation(
            state.clone(),
            Request::PlaybackCommand {
                command: PlaybackCommand::Pause,
            },
            None,
            Some(MutationId::new_v7()),
        )
        .await
        .expect("pause mutation accepted while discovery is blocked");
        let pause_receipt_id = mutation_receipt_id(&pause);
        let pause_receipt = tokio::time::timeout(
            Duration::from_secs(1),
            wait_for_receipt(&state, pause_receipt_id),
        )
        .await
        .expect("pause must complete before radio discovery is released");
        assert_eq!(pause_receipt.status, ReceiptStatus::Confirmed);
        assert_eq!(provider.queue_calls.load(Ordering::SeqCst), 0);

        provider.release_radio_call();
        assert_eq!(
            wait_for_receipt(&state, radio_receipt_id).await.status,
            ReceiptStatus::Confirmed
        );
        assert_eq!(provider.radio_calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.queue_calls.load(Ordering::SeqCst), 2);

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn radio_replay_precedes_discovery_and_does_not_invalidate_claimed_refresh() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = DaemonTestEnv::new();
        let provider =
            Arc::new(RadioProvider::new("radio-replay", None, None).with_blocked_queue_read(2));
        let extras: Arc<dyn ProviderExtras> = provider.clone();
        let runtime = ProviderRuntime::with_transport_and_extras(provider.clone(), extras).unwrap();
        let registry =
            ProviderRegistry::new(MusicProvider::id(provider.as_ref()).clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let mut events = state.event_tx.subscribe();
        let mutation_id = MutationId::new_v7();
        let request = Request::RadioStart {
            seed_uri: "radio-replay:track:seed".to_string(),
            dry_run: false,
        };

        let first = crate::handler::dispatch_with_mutation(
            state.clone(),
            request.clone(),
            None,
            Some(mutation_id),
        )
        .await
        .unwrap();
        assert_eq!(
            wait_for_receipt(&state, mutation_receipt_id(&first))
                .await
                .status,
            ReceiptStatus::Confirmed
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            while provider.queue_reads.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("original pinned refresh must start");
        let claimed_seq = state.current_mutation_seq();
        provider.fail_radio();

        let replay =
            crate::handler::dispatch_with_mutation(state.clone(), request, None, Some(mutation_id))
                .await
                .expect("durable replay must not rediscover radio");
        assert!(matches!(
            replay,
            ResponseData::Mutation { receipt }
                if receipt.replayed && receipt.status == Some(ReceiptStatus::Confirmed)
        ));
        assert_eq!(provider.radio_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            state.current_mutation_seq(),
            claimed_seq,
            "replay must not invalidate the claimed mutation's reconciliation"
        );

        provider.release_queue_read();
        let (_, queue) = wait_for_queue_event(&mut events, "refreshed").await;
        assert!(
            queue.is_some(),
            "original authoritative refresh must still land"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn radio_reconciliation_stays_pinned_after_global_owner_changes() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = DaemonTestEnv::new();
        let default = Arc::new(FakeProvider::isolated("refresh-default").unwrap());
        let selected = Arc::new(RadioProvider::new("refresh-selected", None, None));
        let selected_extras: Arc<dyn ProviderExtras> = selected.clone();
        let registry = ProviderRegistry::new(
            MusicProvider::id(default.as_ref()).clone(),
            [
                ProviderRuntime::with_transport(default.clone()).unwrap(),
                ProviderRuntime::with_transport_and_extras(selected.clone(), selected_extras)
                    .unwrap(),
            ],
        )
        .unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        state.set_active_transport_provider(MusicProvider::id(selected.as_ref()).clone());
        let selected_mutation_seq = state.bump_mutation_seq();
        state.set_active_transport_provider(MusicProvider::id(default.as_ref()).clone());
        state.bump_mutation_seq();
        let default_reads_before = default
            .observed_requests()
            .await
            .iter()
            .filter(|request| request.operation == "transport.queue")
            .count();
        let selected_reads_before = selected.queue_reads.load(Ordering::SeqCst);
        let music: Arc<dyn MusicProvider> = selected.clone();
        let transport: Arc<dyn RemoteTransport> = selected.clone();

        crate::handler::spawn_queue_refresh_for_pair(
            state.clone(),
            music,
            transport,
            selected_mutation_seq,
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            while selected.queue_reads.load(Ordering::SeqCst) == selected_reads_before {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("pinned selected-provider refresh");

        assert_eq!(
            default
                .observed_requests()
                .await
                .iter()
                .filter(|request| request.operation == "transport.queue")
                .count(),
            default_reads_before,
            "refresh must not re-resolve the changed global owner"
        );
        assert_eq!(
            state.active_transport_provider().as_ref(),
            Some(MusicProvider::id(default.as_ref()))
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn radio_no_device_recovery_keeps_started_head_on_later_typed_failure() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = DaemonTestEnv::new();
        let provider = Arc::new(RadioProvider::new(
            "radio-partial",
            Some(1),
            Some((
                2,
                ProviderError::Network("injected queue failure".to_string()),
            )),
        ));
        let extras: Arc<dyn ProviderExtras> = provider.clone();
        let (backend, player_events) =
            spotuify_player::backends::mock::MockPlayerBackend::new_for_provider(
                MusicProvider::id(provider.as_ref()).clone(),
                MusicProvider::uri_scheme(provider.as_ref()).clone(),
            );
        let runtime = ProviderRuntime::with_player(
            provider.clone(),
            Some(extras),
            ProviderPlayer::new(Box::new(backend), player_events),
            TransportRecovery::EmbeddedPlayer,
        )
        .unwrap();
        let registry =
            ProviderRegistry::new(MusicProvider::id(provider.as_ref()).clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        state
            .ensure_player_ready("radio-partial-test")
            .await
            .unwrap();
        let mut events = state.event_tx.subscribe();
        let mutation_id = MutationId::new_v7();
        let request = Request::RadioStart {
            seed_uri: "radio-partial:track:seed".to_string(),
            dry_run: false,
        };

        let first = crate::handler::dispatch_with_mutation(
            state.clone(),
            request.clone(),
            None,
            Some(mutation_id),
        )
        .await
        .unwrap();
        let receipt = wait_for_receipt(&state, mutation_receipt_id(&first)).await;
        assert_eq!(receipt.status, ReceiptStatus::Failed);
        let receipt_error = receipt.error.as_ref().expect("typed partial error");
        assert_eq!(receipt_error.kind, IpcErrorKind::Network);
        assert_eq!(
            receipt_error.provider.as_ref(),
            Some(MusicProvider::id(provider.as_ref()))
        );
        assert!(receipt_error
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("partially applied (1 action(s) succeeded)")));
        let contextual_network = anyhow::Error::new(ProviderError::Network("offline".to_string()))
            .context("queue partially applied");
        assert!(matches!(
            crate::handler::error_response_from(&contextual_network),
            Response::Error {
                kind: IpcErrorKind::Network,
                retryable: true,
                ..
            }
        ));

        let succeeded_uri = "radio-partial:track:track-1".to_string();
        let (event_uris, queue) =
            wait_for_queue_event(&mut events, "queue-partially-applied").await;
        assert!(
            event_uris.is_empty(),
            "started head is not an upcoming append"
        );
        let queue = queue.expect("partial event carries durable queue state");
        assert_eq!(
            queue.currently_playing.as_ref().map(|item| &item.uri),
            Some(&succeeded_uri)
        );
        assert!(queue.session_active);
        assert!(queue.items.is_empty());
        let cached = state
            .store()
            .latest_provider_queue(500, MusicProvider::id(provider.as_ref()))
            .await
            .unwrap()
            .expect("successful prefix is cached despite later failure");
        assert_eq!(
            cached.currently_playing.as_ref().map(|item| &item.uri),
            Some(&succeeded_uri)
        );
        assert!(cached.items.is_empty());

        let durable_response = state
            .store()
            .terminal_mutation_response(mutation_id)
            .await
            .unwrap()
            .expect("partial radio mutation must persist its terminal response");
        let replay = crate::handler::handle_request_with_source_and_mutation(
            state.clone(),
            request,
            None,
            Some(mutation_id),
        )
        .await;
        assert_eq!(
            serde_json::to_value(replay).unwrap(),
            serde_json::to_value(durable_response).unwrap(),
            "same-id replay must preserve the exact durable partial failure"
        );
        assert_eq!(
            provider.queue_calls.load(Ordering::SeqCst),
            2,
            "same-id replay must preserve the durable failure without duplicating the prefix"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn saved_tracks_follows_cursor_and_reports_continuation_lower_bound() {
        let provider = CursorLibraryProvider::new(None);

        let (items, total) = fetch_saved_tracks_page(&provider, 3, 5).await.unwrap();

        assert_eq!(
            items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            ["Alpha", "Bravo", "Charlie"]
        );
        assert_eq!(total, 9);
    }

    #[tokio::test]
    async fn saved_tracks_rejects_zero_limit_without_calling_provider() {
        let provider = CountingLibraryProvider::new();

        let error = fetch_saved_tracks_page(&provider, 0, 37).await.unwrap_err();

        assert!(matches!(
            error.downcast_ref::<ProviderError>(),
            Some(ProviderError::InvalidInput { field, .. }) if field == "limit"
        ));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn saved_tracks_preserves_exact_provider_total() {
        let provider = CursorLibraryProvider::new(Some(42));

        let (items, total) = fetch_saved_tracks_page(&provider, 2, 5).await.unwrap();

        assert_eq!(items.len(), 2);
        assert_eq!(total, 42);
    }

    #[tokio::test]
    async fn saved_tracks_follows_offset_continuation_and_uses_it_for_total_lower_bound() {
        let provider = OffsetLibraryProvider::new();

        let (items, total) = fetch_saved_tracks_page(&provider, 3, 10).await.unwrap();

        assert_eq!(
            items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            ["Alpha", "Bravo", "Charlie"]
        );
        assert_eq!(total, 51);
    }
}
