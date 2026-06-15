//! `library` request handlers (split out of the dispatch god-function).

use std::sync::Arc;

use spotuify_protocol::{DaemonEvent, OperationKind, OperationSource, Request, ResponseData};
use spotuify_spotify::client::{MediaItem, MediaKind};

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
        Request::LibraryList { limit } => Ok(ResponseData::MediaItems {
            items: state.store().list_library_items(limit).await?,
        }),
        Request::SavedTracks { limit, offset } => {
            // Liked songs — live `/me/tracks`, cache fallback on failure.
            // Spotify caps each page at 50, so limits above that paginate.
            let mut client = state.spotify_client().await?;
            let fetch = async {
                let limit = limit as usize;
                let mut offset = offset as u64;
                let mut items: Vec<MediaItem> = Vec::new();
                while items.len() < limit {
                    let page_size = (limit - items.len()).min(50) as u8;
                    let page = client.saved_tracks_page(page_size, offset).await?;
                    let fetched = page.items.len();
                    items.extend(page.items);
                    offset += fetched as u64;
                    if fetched == 0 || offset >= page.total {
                        break;
                    }
                }
                anyhow::Ok(items)
            };
            match fetch.await {
                Ok(items) => Ok(ResponseData::MediaItems { items }),
                Err(err) => {
                    tracing::warn!(error = %err, "saved tracks live fetch failed; serving cache");
                    Ok(ResponseData::MediaItems {
                        items: state.store().list_saved_tracks(limit).await?,
                    })
                }
            }
        }
        Request::SavedShows { limit } => Ok(ResponseData::MediaItems {
            items: state.store().list_saved_shows(limit).await?,
        }),
        Request::ShowEpisodes {
            show,
            limit,
            offset,
        } => {
            let mut client = state.spotify_client().await?;
            let items = client
                .show_episodes(&show, limit.min(50) as u8, offset as u64)
                .await?;
            Ok(ResponseData::MediaItems { items })
        }
        Request::EpisodeFeed {
            limit,
            sort,
            refresh,
        } => Ok(ResponseData::MediaItems {
            items: episode_feed(&state, limit, sort, refresh).await?,
        }),

        // --- Listening reminders + notifications ---
        Request::ArtistAlbums { artist } => {
            let mut client = state.spotify_client().await?;
            let mut items = client.artist_albums(&artist).await?;
            // Tag each album with whether it's already saved, so clients can
            // offer an "in library only" filter without a per-album lookup.
            // A cold/failed cache simply yields an empty set (all not-saved).
            let saved = state.store().saved_album_uris().await.unwrap_or_default();
            for item in &mut items {
                item.in_library = Some(saved.contains(&item.uri));
            }
            Ok(ResponseData::MediaItems { items })
        }
        Request::FollowedArtists { limit } => {
            // Cache-first; fall back to a live fetch + persist on a cold cache.
            let cached = state.store().list_followed_artists(limit).await?;
            let items = if cached.is_empty() {
                let mut client = state.spotify_client().await?;
                let fetched = client.followed_artists().await?;
                if !fetched.is_empty() {
                    let _ = state.store().persist_followed_artists(&fetched).await;
                }
                fetched.into_iter().take(limit as usize).collect()
            } else {
                cached
            };
            Ok(ResponseData::MediaItems { items })
        }
        Request::AlbumTracks { album } => {
            let mut client = state.spotify_client().await?;
            let items = client.album_tracks(&album).await?;
            Ok(ResponseData::MediaItems { items })
        }
        Request::RelatedArtists { artist } => {
            // Mercury-backed (the Web API related-artists endpoint was
            // deprecated Nov 2024). Needs the in-daemon librespot session.
            let Some(mercury_uri) = spotuify_spotify::mercury::related_artists_mercury_uri(&artist)
            else {
                anyhow::bail!(
                    "invalid request: related-artists needs a `spotify:artist:` URI, got `{artist}`"
                );
            };
            let bytes =
                tokio::time::timeout(MERCURY_DISCOVERY_TIMEOUT, state.mercury_get(&mercury_uri))
                    .await
                    .map_err(|_| anyhow::anyhow!("related-artists request timed out"))??;
            let items = spotuify_spotify::mercury::parse_related_artists(&bytes);
            if items.is_empty() {
                tracing::warn!(
                    %artist,
                    "related-artists returned no parseable results; the Mercury endpoint \
                     may have changed"
                );
            }
            Ok(ResponseData::MediaItems { items })
        }
        Request::RadioStart { seed_uri, dry_run } => {
            if !seed_uri.starts_with("spotify:") {
                anyhow::bail!(
                    "invalid request: radio-start needs a `spotify:` URI, got `{seed_uri}`"
                );
            }
            let mercury_uri = spotuify_spotify::mercury::radio_station_mercury_uri(&seed_uri);
            let bytes =
                tokio::time::timeout(MERCURY_DISCOVERY_TIMEOUT, state.mercury_get(&mercury_uri))
                    .await
                    .map_err(|_| anyhow::anyhow!("radio station request timed out"))??;
            let track_uris = spotuify_spotify::mercury::parse_radio_station(&bytes);
            if track_uris.is_empty() {
                anyhow::bail!(
                    "radio station returned no tracks; the Mercury endpoint may have changed \
                     or `{seed_uri}` has no station"
                );
            }
            // Enrich for a useful preview; unresolved URIs fall back to bare items.
            let mut items = state
                .store()
                .media_items_by_uris(&track_uris)
                .await
                .unwrap_or_default();
            if items.len() != track_uris.len() {
                let known: std::collections::HashSet<String> =
                    items.iter().map(|item| item.uri.clone()).collect();
                for uri in &track_uris {
                    if !known.contains(uri) {
                        items.push(MediaItem {
                            id: uri.rsplit(':').next().map(str::to_string),
                            uri: uri.clone(),
                            kind: MediaKind::Track,
                            source: Some("mercury".to_string()),
                            ..MediaItem::default()
                        });
                    }
                }
            }
            if !dry_run {
                // Populate the active device's queue so playback flows into
                // the station. Best-effort per track; queue is a set so
                // duplicates move up rather than duplicate.
                for uri in &track_uris {
                    if let Err(err) = state.queue_add(uri).await {
                        tracing::debug!(error = %err, %uri, "radio queue add failed");
                    }
                }
            }
            Ok(ResponseData::MediaItems { items })
        }
        Request::LibrarySave { uri, current } => {
            let state_for = state.clone();
            let uri_for = uri.clone();
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
                move |op_id| async move {
                    let mut client = state_for.spotify_client().await?;
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
                        if let Err(err) = state_for
                            .store()
                            .update_operation_plan(op_id, Some(&pre_state), Some(&plan))
                            .await
                        {
                            tracing::warn!(error = %err, "failed to persist library_save pre-state");
                        }
                        if let Err(err) = state_for
                            .store()
                            .update_operation_subject_uris(op_id, std::slice::from_ref(real_uri))
                            .await
                        {
                            tracing::warn!(error = %err, "failed to persist library_save subject uri");
                        }
                    }
                    let save_uri = resolved_uri
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("nothing is playing"))?;
                    client.library_save_by_uri(&save_uri).await?;
                    let message = "save".to_string();
                    state_for.emit_event(DaemonEvent::LibraryChanged {
                        action: "save".to_string(),
                        uris: event_uris,
                    });
                    emit_mutation_finished(&state_for, "save", &message);
                    Ok(())
                },
            )
            .await
        }
        Request::LibraryUnsave { uri } => {
            let state_for = state.clone();
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
                move |_op_id| async move {
                    let mut client = state_for.spotify_client().await?;
                    client.library_unsave_by_uri(&uri).await?;
                    let message = format!("Unsaved {uri}");
                    state_for.emit_event(DaemonEvent::LibraryChanged {
                        action: "unsave".to_string(),
                        uris: vec![uri.clone()],
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
                move |_op_id| async move {
                    let mut client = state_for.spotify_client().await?;
                    client.follow_artist(&artist_for).await?;
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
                move |_op_id| async move {
                    let mut client = state_for.spotify_client().await?;
                    client.unfollow_artist(&artist_for).await?;
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
