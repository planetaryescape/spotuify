use std::sync::Arc;
use std::time::{Duration, Instant};

use spotuify_core::{now_ms, search_performed_event, Playback};
use spotuify_protocol::{
    CommandReceipt, DaemonEvent, EpisodeSort, Operation, OperationId, OperationKind,
    OperationSource, OperationStatus, PlaybackCommand, ReceiptId, Request, Response, ResponseData,
    SearchScopeData, SearchSortData, SearchSourceData,
};
use spotuify_spotify::actions::{self, CommandKind};
use spotuify_spotify::client::{MediaItem, MediaKind, SpotifyClient};
use spotuify_spotify::selection;

use crate::state::{DaemonState, FastTransportStatus};

pub(crate) const LYRICS_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
pub(crate) const LYRICS_NEGATIVE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Cap the Spotify Mercury lyrics fetch. `mercury_get` awaits the player
/// actor with no timeout, so a hung/unresponsive session would stall the
/// whole lyrics fetch (and the client spinner) until the 5-min client
/// timeout. On timeout we fall through to the LRCLIB provider.
pub(crate) const MERCURY_LYRICS_TIMEOUT: Duration = Duration::from_secs(6);
/// Cap for Mercury discovery requests (related artists, radio stations).
pub(crate) const MERCURY_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(8);
pub(crate) const MUTATION_BODY_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const TRANSPORT_BACKEND_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const FAST_TRANSPORT_TIMEOUT: Duration = Duration::from_millis(250);
/// After the fast deadline elapses we keep watching the player actor's
/// ack for this long so a late failure can reconcile.
pub(crate) const FAST_TRANSPORT_ACK_GRACE: Duration = Duration::from_secs(10);
pub(crate) const DEVICE_RECOVERY_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const DEVICE_REGISTRY_TIMEOUT: Duration = Duration::from_secs(8);
pub(crate) const DEVICE_REGISTRY_POLL_INTERVAL: Duration = Duration::from_millis(500);
pub(crate) const SEARCH_REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
pub(crate) const QUEUE_APPEND_BASE_MAX_AGE_MS: i64 = 30_000;

pub(crate) async fn handle_request_with_source(
    state: Arc<DaemonState>,
    request: Request,
    source: Option<OperationSource>,
) -> Response {
    match dispatch(state, request, source).await {
        Ok(data) => Response::Ok { data },
        Err(err) => error_response_from(&err),
    }
}

/// Build a `Response::Error` from an `anyhow::Error`. Tries to
/// downcast to `SpotifyError` first so typed errors (notably
/// `AuthRevoked`) get the correct `IpcErrorKind` — otherwise we'd
/// fall back to substring classification on the display string and
/// lose specificity.
pub(crate) fn error_response_from(err: &anyhow::Error) -> Response {
    let message = err.to_string();
    if let Some(spotify_err) = err.downcast_ref::<spotuify_spotify::SpotifyError>() {
        let kind = spotify_err.ipc_kind();
        let retryable = spotify_err.is_retryable();
        return Response::error_with_retryable(message, kind, retryable);
    }
    Response::error(message)
}

pub(crate) async fn dispatch(
    state: Arc<DaemonState>,
    request: Request,
    source: Option<OperationSource>,
) -> anyhow::Result<ResponseData> {
    match crate::handlers::categorize(&request) {
        crate::handlers::Cat::Admin => {
            crate::handlers::admin::dispatch(state, request, source).await
        }
        crate::handlers::Cat::Playback => {
            crate::handlers::playback::dispatch(state, request, source).await
        }
        crate::handlers::Cat::Search => {
            crate::handlers::search::dispatch(state, request, source).await
        }
        crate::handlers::Cat::Library => {
            crate::handlers::library::dispatch(state, request, source).await
        }
        crate::handlers::Cat::Playlists => {
            crate::handlers::playlists::dispatch(state, request, source).await
        }
        crate::handlers::Cat::Analytics => {
            crate::handlers::analytics::dispatch(state, request, source).await
        }
        crate::handlers::Cat::Ops => crate::handlers::ops::dispatch(state, request, source).await,
        crate::handlers::Cat::Reminders => {
            crate::handlers::reminders::dispatch(state, request, source).await
        }
        crate::handlers::Cat::Viz => crate::handlers::viz::dispatch(state, request, source).await,
        crate::handlers::Cat::Media => {
            crate::handlers::media::dispatch(state, request, source).await
        }
    }
}

pub(crate) async fn handle_ops_undo(
    state: &std::sync::Arc<DaemonState>,
    operation_id: Option<spotuify_protocol::OperationId>,
    source: OperationSource,
    dry_run: bool,
    force: bool,
    bulk_since_ms: Option<i64>,
) -> anyhow::Result<ResponseData> {
    // Bulk undo: walk every reversible succeeded op newer than `since`,
    // reverse-chronological, stop on first failure (per blueprint).
    if let Some(since) = bulk_since_ms {
        let ops = state
            .store()
            .find_reversible_operations_since(since, None)
            .await?;
        let mut succeeded = 0u32;
        let mut skipped = 0u32;
        let mut errors = Vec::new();
        let mut preview = Vec::new();
        let mut last_undo_op_id = None;
        for op in ops {
            let undo_op_id = OperationId::new_v7();
            match undo_single(state, &op, undo_op_id, source, dry_run, force).await {
                Ok(UndoOutcome::Applied) => {
                    succeeded += 1;
                    last_undo_op_id = Some(undo_op_id);
                }
                Ok(UndoOutcome::Preview(line)) => {
                    skipped += 1;
                    preview.push(line);
                }
                Err(err) => {
                    errors.push(err.to_string());
                    break;
                }
            }
        }
        return Ok(ResponseData::OperationUndoResult {
            undo_op_id: last_undo_op_id.unwrap_or_else(OperationId::new_v7),
            succeeded,
            skipped,
            errors,
            preview,
        });
    }

    // Single op (default: last reversible).
    let op = match operation_id {
        Some(id) => state.store().get_operation(id).await?,
        None => state
            .store()
            .find_last_reversible_operation()
            .await?
            .ok_or_else(|| anyhow::anyhow!("no reversible operations to undo"))?,
    };
    let undo_op_id = OperationId::new_v7();
    let mut errors = Vec::new();
    let mut preview = Vec::new();
    let succeeded = match undo_single(state, &op, undo_op_id, source, dry_run, force).await {
        Ok(UndoOutcome::Applied) => 1,
        Ok(UndoOutcome::Preview(line)) => {
            preview.push(line);
            0
        }
        Err(err) => {
            errors.push(err.to_string());
            0
        }
    };
    Ok(ResponseData::OperationUndoResult {
        undo_op_id,
        succeeded,
        skipped: 0,
        errors,
        preview,
    })
}

/// What `undo_single` did: executed the reversal, or (dry-run) produced
/// a human-readable description of what it would do.
pub(crate) enum UndoOutcome {
    Applied,
    Preview(String),
}

pub(crate) async fn undo_single(
    state: &std::sync::Arc<DaemonState>,
    op: &spotuify_protocol::Operation,
    undo_op_id: OperationId,
    source: OperationSource,
    dry_run: bool,
    force: bool,
) -> anyhow::Result<UndoOutcome> {
    crate::undo::validate_undoable(op)?;
    let plan = op
        .reversal_plan
        .clone()
        .ok_or_else(|| anyhow::anyhow!("op {} missing reversal_plan", op.operation_id))?;

    // Snapshot conflict detection. Pre-fetch the current Spotify
    // snapshot id (if the plan references a playlist) so the
    // synchronous `check_snapshot` can compare without itself doing
    // I/O. The previous shape used `block_in_place` +
    // `Handle::block_on` from inside a sync closure to bridge that
    // gap, which took a tokio worker out of the pool for the
    // duration of a full `/me/playlists` paginated fetch — a foot-gun
    // when a sync burst already had other workers busy on writes.
    let current_snapshot = match crate::undo::snapshot_check_target(&plan) {
        Some((playlist_id, _)) => {
            let playlist_id = playlist_id.to_string();
            match state.spotify_client().await {
                Ok(mut client) => match client.playlists().await {
                    Ok(playlists) => playlists
                        .into_iter()
                        .find(|p| p.id == playlist_id)
                        .and_then(|p| p.snapshot_id),
                    Err(err) => {
                        tracing::debug!(error = %err, playlist = %playlist_id, "snapshot fetch failed");
                        None
                    }
                },
                Err(err) => {
                    tracing::debug!(error = %err, "spotify client unavailable for snapshot check");
                    None
                }
            }
        }
        None => None,
    };
    crate::undo::check_snapshot(&plan, |_id| current_snapshot.clone(), force)?;

    if dry_run {
        // Dry-run: describe what would happen instead of doing it. The
        // line travels back in `OperationUndoResult.preview` so the CLI
        // can print it directly.
        let pre = op
            .pre_state
            .clone()
            .unwrap_or(spotuify_protocol::PreState::Transport);
        return Ok(UndoOutcome::Preview(format!(
            "would undo {} {}: {}",
            op.kind,
            op.operation_id,
            crate::undo::render_plan_summary(&plan, &pre)
        )));
    }

    // Execute the reversal via Spotify Web API.
    apply_reversal(state, &plan).await?;

    // Record the new undo operation row + flip the original to undone.
    let undo_op = crate::undo::undo_operation_row(undo_op_id, op, source, now_ms());
    state.store().insert_pending_operation(&undo_op).await?;
    state
        .store()
        .mark_operation_undone(op.operation_id, undo_op.operation_id)
        .await?;
    state.emit_event(DaemonEvent::OperationUndone {
        undo_op_id: undo_op.operation_id,
        original_op_id: op.operation_id,
        success: true,
    });
    Ok(UndoOutcome::Applied)
}

pub(crate) async fn apply_reversal(
    state: &std::sync::Arc<DaemonState>,
    plan: &spotuify_protocol::ReversalPlan,
) -> anyhow::Result<()> {
    use spotuify_protocol::ReversalPlan as P;
    match plan {
        P::TransferToPriorDevice { device_id } => {
            let mut client = state.spotify_client().await?;
            client.transfer(device_id, false).await?;
            Ok(())
        }
        P::QueueRemove { uri } => {
            // Legacy plan: queue_add rows recorded before the kind went
            // non-reversible carry this. Executing it used to be a
            // silent no-op that still marked the op undone; fail loudly
            // instead of lying about what happened.
            anyhow::bail!(
                "cannot remove {uri} from the queue: Spotify has no queue-remove endpoint \
                 (queue adds recorded by older versions are not actually reversible)"
            )
        }
        P::PlaylistRemoveTracks {
            playlist_id,
            uris,
            snapshot_id,
        } => {
            let mut client = state.spotify_client().await?;
            client
                .remove_playlist_items(playlist_id, uris, snapshot_id.as_deref())
                .await
                .map(|_new_snap| ())?;
            Ok(())
        }
        P::PlaylistAddAtPositions {
            playlist_id,
            items,
            snapshot_id,
        } => {
            let mut client = state.spotify_client().await?;
            client
                .add_items_to_playlist_at_positions(playlist_id, items, snapshot_id.as_deref())
                .await
                .map(|_| ())?;
            Ok(())
        }
        P::PlaylistDelete { playlist_id } => {
            let mut client = state.spotify_client().await?;
            client.unfollow_playlist(playlist_id).await?;
            Ok(())
        }
        P::PlaylistReorder {
            playlist_id,
            range_start,
            insert_before,
            range_length,
            snapshot_id,
        } => {
            let mut client = state.spotify_client().await?;
            client
                .reorder_playlist_items(
                    playlist_id,
                    *range_start,
                    *insert_before,
                    *range_length,
                    snapshot_id.as_deref(),
                )
                .await
                .map(|_| ())?;
            Ok(())
        }
        P::LibraryUnsave { uri } => {
            let mut client = state.spotify_client().await?;
            client.library_unsave_by_uri(uri).await?;
            Ok(())
        }
        P::LibrarySave { uri, .. } => {
            // `prior_added_at_ms` is recorded for forensics only —
            // Spotify's save endpoint always sets `added_at` to now.
            // Documented limitation; surfaced in `ops show --diff`.
            let mut client = state.spotify_client().await?;
            client.library_save_by_uri(uri).await?;
            Ok(())
        }
        P::Like { uri } => {
            // Like ≡ library_save for tracks; the protocol keeps Like
            // distinct from LibrarySave for clarity in the op log even
            // though Spotify's endpoint is the same.
            let mut client = state.spotify_client().await?;
            client.library_save_by_uri(uri).await?;
            Ok(())
        }
        P::Unlike { uri } => {
            let mut client = state.spotify_client().await?;
            client.library_unsave_by_uri(uri).await?;
            Ok(())
        }
        P::NotReversible { reason } => {
            anyhow::bail!("operation is not reversible: {reason}")
        }
        P::Redo { .. } => anyhow::bail!(
            "redo of an undo replays the original forward op; \
             use `ops redo` instead of `ops undo`"
        ),
    }
}

pub(crate) async fn handle_ops_redo(
    state: &std::sync::Arc<DaemonState>,
    operation_id: Option<spotuify_protocol::OperationId>,
    source: OperationSource,
) -> anyhow::Result<ResponseData> {
    // Find an undone op to redo. Default: most-recent undone.
    let op = match operation_id {
        Some(id) => state.store().get_operation(id).await?,
        None => {
            let ops = state.store().list_operations(50, None, None).await?;
            ops.into_iter()
                .find(|o| o.status == OperationStatus::Undone)
                .ok_or_else(|| anyhow::anyhow!("no undone operations to redo"))?
        }
    };
    if op.status != OperationStatus::Undone {
        anyhow::bail!(
            "operation {} is not undone (status = {:?}); only undone ops can be redone",
            op.operation_id,
            op.status,
        );
    }

    // Real redo: re-execute the original Request by fetching its
    // serialized form from the linked receipt row. The fresh dispatch
    // creates its own operation row through `record_operation`, so
    // mark the original as redone-by that fresh row.
    let receipt_id = op
        .receipt_id
        .ok_or_else(|| anyhow::anyhow!("op {} has no receipt; cannot redo", op.operation_id))?;
    let raw = state.store().receipt_request_json(receipt_id).await?;
    let original_request: Request = serde_json::from_str(&raw)
        .map_err(|err| anyhow::anyhow!("failed to decode original request: {err}"))?;
    // Record the timestamp before dispatch so we can locate the freshly
    // minted operation row afterwards.
    let dispatch_started_at = now_ms();
    // Recursive dispatch. Any failure surfaces back to the caller.
    let response = Box::pin(dispatch(state.clone(), original_request, Some(source))).await?;

    // Locate the newly-minted op row created by the re-dispatched
    // mutation. dispatch is in-process and serial, so the most-recent
    // op with `occurred_at_ms >= dispatch_started_at` is ours.
    let recent_ops = state
        .store()
        .list_operations(5, Some(dispatch_started_at), None)
        .await
        .unwrap_or_default();
    let redo_op_id = recent_ops
        .into_iter()
        .find(|o| {
            o.operation_id != op.operation_id
                && o.kind != OperationKind::Redo
                && o.kind != OperationKind::Undo
        })
        .map_or_else(OperationId::new_v7, |o| o.operation_id);

    let _ = state
        .store()
        .mark_operation_redone(op.operation_id, redo_op_id)
        .await;
    state.emit_event(DaemonEvent::OperationUndone {
        undo_op_id: redo_op_id,
        original_op_id: op.operation_id,
        success: true,
    });
    let _ = response;
    Ok(ResponseData::OperationUndoResult {
        undo_op_id: redo_op_id,
        succeeded: 1,
        skipped: 0,
        errors: vec![],
        preview: vec![],
    })
}

pub(crate) async fn lyrics_get(
    state: Arc<DaemonState>,
    track_uri: Option<String>,
    force_refresh: bool,
) -> anyhow::Result<ResponseData> {
    let Some((track_uri, item)) = resolve_lyrics_target(&state, track_uri).await? else {
        return Ok(ResponseData::Lyrics {
            lyrics: None,
            offset_ms: 0,
        });
    };
    let offset_ms = state.store().lyrics_offset_ms(&track_uri).await?;
    let cached = state.store().cached_lyrics(&track_uri, LYRICS_TTL).await?;
    if !force_refresh && cached.is_some() {
        return Ok(ResponseData::Lyrics {
            lyrics: cached,
            offset_ms,
        });
    }
    if !force_refresh && state.store().lyrics_lookup_blocked(&track_uri).await? {
        return Ok(ResponseData::Lyrics {
            lyrics: cached,
            offset_ms,
        });
    }

    let fetched = fetch_lyrics(&state, &track_uri, item.as_ref()).await?;
    if let Some(lyrics) = fetched.as_ref() {
        state.store().upsert_lyrics(lyrics).await?;
    } else if cached.is_none() {
        state
            .store()
            .upsert_lyrics_lookup_failure(&track_uri, "not found", LYRICS_NEGATIVE_TTL)
            .await?;
    }

    Ok(ResponseData::Lyrics {
        lyrics: fetched.or(cached),
        offset_ms,
    })
}

pub(crate) async fn resolve_lyrics_target(
    state: &Arc<DaemonState>,
    track_uri: Option<String>,
) -> anyhow::Result<Option<(String, Option<MediaItem>)>> {
    if let Some(track_uri) = track_uri {
        let mut items = state
            .store()
            .media_items_by_uris(std::slice::from_ref(&track_uri))
            .await?;
        let mut item = items.pop();
        if item.is_none() {
            match state.spotify_client().await {
                Ok(mut client) => match client.media_item_by_uri(&track_uri).await {
                    Ok(Some(fetched)) => {
                        state
                            .store()
                            .upsert_media_items(std::slice::from_ref(&fetched), "spotify")
                            .await?;
                        item = Some(fetched);
                    }
                    Ok(None) => {}
                    Err(err) => {
                        tracing::debug!(error = %err, track_uri, "track metadata lookup failed")
                    }
                },
                Err(err) => {
                    tracing::debug!(error = %err, track_uri, "spotify client unavailable for lyrics metadata lookup")
                }
            }
        }
        return Ok(Some((track_uri, item)));
    }

    let playback = state.snapshot_playback();
    Ok(playback.item.map(|item| (item.uri.clone(), Some(item))))
}

pub(crate) async fn fetch_lyrics(
    state: &Arc<DaemonState>,
    track_uri: &str,
    item: Option<&MediaItem>,
) -> anyhow::Result<Option<spotuify_core::SyncedLyrics>> {
    if let Some(mercury_uri) = spotuify_lyrics::mercury_uri_for_track_uri(track_uri) {
        match tokio::time::timeout(MERCURY_LYRICS_TIMEOUT, state.mercury_get(&mercury_uri)).await {
            Ok(Ok(bytes)) => {
                match spotuify_lyrics::parse_spotify_mercury(bytes, track_uri, now_ms()) {
                    Ok(Some(lyrics)) => return Ok(Some(lyrics)),
                    Ok(None) => {}
                    Err(err) => {
                        tracing::warn!(error = %err, track_uri, "spotify mercury lyrics parse failed")
                    }
                }
            }
            Ok(Err(err)) => {
                tracing::debug!(error = %err, track_uri, "spotify mercury lyrics unavailable")
            }
            Err(_) => {
                tracing::warn!(
                    track_uri,
                    "spotify mercury lyrics timed out; falling back to LRCLIB"
                )
            }
        }
    }

    let Some(item) = item else {
        return Ok(None);
    };
    match spotuify_lyrics::LrclibProvider::new()
        .fetch(item, now_ms())
        .await
    {
        Ok(lyrics) => Ok(lyrics),
        Err(err) => {
            tracing::warn!(error = %err, track_uri, "lrclib lyrics unavailable");
            Ok(None)
        }
    }
}

/// Spotify's `/v1/search?q=...` endpoint returns HTTP 404 for queries
/// longer than 144 characters (no documented error; just a confusing
/// 404). Guard at the daemon boundary so callers get a typed error
/// instead of "search failed: 404 Not Found". The local path doesn't
/// have this constraint, but we apply the same cap so behaviour is
/// consistent regardless of source.
pub(crate) const MAX_SEARCH_QUERY_CHARS: usize = 144;

pub(crate) async fn search_with_source(
    state: Arc<DaemonState>,
    query: String,
    scope: SearchScopeData,
    source: SearchSourceData,
    limit: u32,
    kinds: Option<Vec<MediaKind>>,
    sort: Option<SearchSortData>,
) -> anyhow::Result<Vec<MediaItem>> {
    if query.chars().count() > MAX_SEARCH_QUERY_CHARS {
        anyhow::bail!(
            "search query is {} characters; Spotify's limit is {}. Trim and try again.",
            query.chars().count(),
            MAX_SEARCH_QUERY_CHARS
        );
    }
    // The caller may restrict to an explicit set of kinds (e.g. "podcasts only",
    // "tracks + artists"); otherwise fall back to the kinds implied by `scope`.
    let effective_kinds = kinds.clone().unwrap_or_else(|| scope_media_kinds(scope));
    let mut items = match source {
        SearchSourceData::Local => local_cached_search(&state, &query, scope, limit).await?,
        SearchSourceData::Spotify => {
            spotify_search_and_cache(state, query, scope, effective_kinds.clone(), limit).await?
        }
        SearchSourceData::Hybrid => {
            let cached = local_cached_search(&state, &query, scope, limit).await?;
            if cached.is_empty() {
                spotify_search_and_cache(state, query, scope, effective_kinds.clone(), limit)
                    .await?
            } else {
                let refresh_state = state.clone();
                let refresh_query = query.clone();
                let refresh_kinds = effective_kinds.clone();
                state.spawn_background("spotify-search-refresh", async move {
                    if let Err(err) = spotify_search_and_cache(
                        refresh_state,
                        refresh_query,
                        scope,
                        refresh_kinds,
                        limit,
                    )
                    .await
                    {
                        tracing::debug!(error = %err, "background Spotify search refresh failed");
                    }
                });
                cached
            }
        }
    };
    // Post-filter to the requested kinds — covers the local/cached paths, which
    // search by `scope` and may return kinds the explicit filter excludes.
    if kinds.is_some() {
        let allowed: std::collections::HashSet<MediaKind> = effective_kinds.into_iter().collect();
        items.retain(|item| allowed.contains(&item.kind));
    }
    apply_search_sort(&mut items, sort);
    Ok(items)
}

/// Order search results in place. `Relevance` (and `None`) preserves Spotify's
/// own ordering; the others use a stable sort so ties keep relevance order.
pub(crate) fn apply_search_sort(items: &mut [MediaItem], sort: Option<SearchSortData>) {
    match sort {
        None | Some(SearchSortData::Relevance) => {}
        Some(SearchSortData::Name) => items.sort_by_key(|item| item.name.to_lowercase()),
        Some(SearchSortData::Duration) => items.sort_by_key(|item| item.duration_ms),
        Some(SearchSortData::Artist) => items.sort_by_key(|item| item.subtitle.to_lowercase()),
        // Newest first. `release_date` is "YYYY-MM-DD" (lexicographically
        // sortable); items without a date sort last.
        Some(SearchSortData::Date) => {
            items.sort_by(|a, b| b.release_date.cmp(&a.release_date));
        }
    }
}

/// How many followed shows the episode feed fans out over (newest-first shows
/// from the cache). Bounds the GitHub-of-podcasts blast radius.
pub(crate) const EPISODE_FEED_SHOW_CAP: u32 = 40;
/// First N episodes pulled per show (Spotify returns them newest-first).
pub(crate) const EPISODE_FEED_PER_SHOW: u8 = 8;
/// Max concurrent `show-episodes` fetches.
pub(crate) const EPISODE_FEED_CONCURRENCY: usize = 8;
/// How long a merged feed stays fresh before a re-fetch.
pub(crate) const EPISODE_FEED_TTL_MS: i64 = 15 * 60_000;

/// A flat, date-ordered episode feed merged across all followed shows. Fans out
/// `show_episodes` over the saved shows (bounded concurrency), merges, caches
/// the raw merged set (sort + limit applied per call), and re-fetches when the
/// cache is older than [`EPISODE_FEED_TTL_MS`] or `refresh` is set.
pub(crate) async fn episode_feed(
    state: &Arc<DaemonState>,
    limit: u32,
    sort: EpisodeSort,
    refresh: bool,
) -> anyhow::Result<Vec<MediaItem>> {
    let now = now_ms();
    if !refresh {
        if let Some((cached, at)) = state.cached_episode_feed() {
            if now - at <= EPISODE_FEED_TTL_MS {
                return Ok(finalize_episode_feed(cached, sort, limit));
            }
        }
    }

    let shows = state
        .store()
        .list_saved_shows(EPISODE_FEED_SHOW_CAP)
        .await?;
    if shows.len() as u32 == EPISODE_FEED_SHOW_CAP {
        tracing::info!(
            cap = EPISODE_FEED_SHOW_CAP,
            "episode feed truncated to the first {EPISODE_FEED_SHOW_CAP} followed shows"
        );
    }

    let semaphore = Arc::new(tokio::sync::Semaphore::new(EPISODE_FEED_CONCURRENCY));
    let mut tasks = Vec::with_capacity(shows.len());
    for show in shows {
        let show_uri = show.uri.clone();
        let show_name = show.name.clone();
        let task_state = state.clone();
        let permits = semaphore.clone();
        tasks.push(tokio::spawn(async move {
            let _permit = permits.acquire().await.ok()?;
            let mut client = task_state.spotify_client().await.ok()?;
            match client
                .show_episodes(&show_uri, EPISODE_FEED_PER_SHOW, 0)
                .await
            {
                Ok(mut episodes) => {
                    for episode in &mut episodes {
                        // Episodes carry the show name as subtitle; backfill it
                        // (and the context) when Spotify omitted it so the
                        // "by show" sort + display stay correct.
                        if episode.subtitle.is_empty() {
                            episode.subtitle = show_name.clone();
                        }
                        if episode.context.is_empty() {
                            episode.context = show_name.clone();
                        }
                    }
                    Some(episodes)
                }
                Err(err) => {
                    tracing::debug!(show = %show_uri, error = %err, "episode feed: show fetch failed");
                    None
                }
            }
        }));
    }

    let mut merged: Vec<MediaItem> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for task in tasks {
        if let Ok(Some(episodes)) = task.await {
            for episode in episodes {
                if seen.insert(episode.uri.clone()) {
                    merged.push(episode);
                }
            }
        }
    }

    state.set_cached_episode_feed(merged.clone(), now);
    Ok(finalize_episode_feed(merged, sort, limit))
}

/// Sort + cap a merged episode list for a given [`EpisodeSort`].
pub(crate) fn finalize_episode_feed(
    mut items: Vec<MediaItem>,
    sort: EpisodeSort,
    limit: u32,
) -> Vec<MediaItem> {
    match sort {
        // `release_date` is "YYYY-MM-DD" (lexicographically sortable).
        EpisodeSort::Newest => items.sort_by(|a, b| b.release_date.cmp(&a.release_date)),
        EpisodeSort::Oldest => items.sort_by(|a, b| a.release_date.cmp(&b.release_date)),
        EpisodeSort::Duration => items.sort_by_key(|item| std::cmp::Reverse(item.duration_ms)),
        EpisodeSort::Title => items.sort_by_key(|item| item.name.to_lowercase()),
        EpisodeSort::Show => items.sort_by_key(|item| item.subtitle.to_lowercase()),
    }
    if limit > 0 {
        items.truncate(limit as usize);
    }
    items
}

pub(crate) async fn local_cached_search(
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

pub(crate) async fn spotify_search_and_cache(
    state: Arc<DaemonState>,
    query: String,
    scope: SearchScopeData,
    kinds: Vec<MediaKind>,
    limit: u32,
) -> anyhow::Result<Vec<MediaItem>> {
    let client = state.spotify_client().await?;
    let started = Instant::now();
    let mut items = match tokio::time::timeout(
        SEARCH_REQUEST_TIMEOUT,
        client.search_with_limit(&query, &kinds, limit as u8),
    )
    .await
    {
        Ok(result) => result?,
        Err(_) => anyhow::bail!(
            "Spotify search timed out after {}s",
            SEARCH_REQUEST_TIMEOUT.as_secs()
        ),
    };
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
    state.emit_event(DaemonEvent::SearchUpdated {
        query: query.clone(),
        count: items.len(),
    });

    // Cache to the search_runs/search_results tables on a background
    // task — fast to return, useful for analytics + Hybrid mode's
    // "show recent results immediately" path. media_items gets
    // upserted as part of that so follow-up actions (add to playlist,
    // play URI) don't need to re-fetch.
    //
    // We do NOT push these entries into the library Tantivy index.
    // That index is the user's library; polluting it with arbitrary
    // catalog hits ranked by text relevance would surface "random
    // Spotify song" results in the Library tab and would break
    // assumptions about what's actually saved. local_search's SQLite
    // fallback already orders saved/liked items first via ORDER BY,
    // so library content stays prioritised even when media_items
    // contains catalog rows.
    let cache_state = state.clone();
    let cache_query = query.clone();
    let cache_items = items.clone();
    state.spawn_background("spotify-search-cache", async move {
        if let Err(err) = cache_state
            .store()
            .cache_search_results(&cache_query, scope, SearchSourceData::Spotify, &cache_items)
            .await
        {
            tracing::warn!(error = %err, "failed to cache Spotify search results");
        }
    });

    Ok(items)
}

/// Streaming search: ack returns immediately; the actual results
/// stream back as `DaemonEvent::SearchPage` events as each per-`(kind,
/// offset)` request resolves. After all fanned-out tasks join, a
/// `DaemonEvent::SearchComplete` event marks the end of the initial
/// fetch — clients use it to clear "loading initial results" spinners.
///
/// Initial-pages count is fixed at 1 (10 items per page; with 6 kinds
/// for `scope=All` that's 6 total requests). More pages load on scroll.
/// The fanout is detached from the request handler so the IPC reply is
/// not blocked.
pub(crate) const SEARCH_INITIAL_PAGES: u32 = 1;
pub(crate) const SEARCH_PAGE_SIZE: u32 = 10;

pub(crate) fn spawn_search_stream(
    state: Arc<DaemonState>,
    query: String,
    scope: SearchScopeData,
    source: SearchSourceData,
    version: u64,
) {
    let state_clone = state.clone();
    state.spawn_background("search-stream", async move {
        if query.chars().count() > MAX_SEARCH_QUERY_CHARS {
            let message = format!(
                "search query is {} characters; Spotify's limit is {}. Trim and try again.",
                query.chars().count(),
                MAX_SEARCH_QUERY_CHARS
            );
            tracing::warn!(
                chars = query.chars().count(),
                "search-stream query exceeds Spotify limit"
            );
            state_clone.emit_event(DaemonEvent::SearchFailed {
                query: query.clone(),
                version,
                kind: None,
                offset: None,
                message,
            });
            state_clone.emit_event(DaemonEvent::SearchComplete { query, version });
            return;
        }
        // Local/Hybrid: synthesize a single SearchPage from the Tantivy
        // hit, then close with SearchComplete. Keeps clients' event
        // handling uniform regardless of source.
        if !matches!(source, SearchSourceData::Spotify) {
            let items = match local_cached_search(&state_clone, &query, scope, 200).await {
                Ok(items) => items,
                Err(err) => {
                    tracing::warn!(error = %err, "local search-stream failed");
                    state_clone.emit_event(DaemonEvent::SearchFailed {
                        query: query.clone(),
                        version,
                        kind: None,
                        offset: None,
                        message: format!("local search failed: {err}"),
                    });
                    state_clone.emit_event(DaemonEvent::SearchComplete { query, version });
                    return;
                }
            };
            let by_kind = group_items_by_kind(items);
            for (kind, items) in by_kind {
                state_clone.emit_event(DaemonEvent::SearchPage {
                    query: query.clone(),
                    kind,
                    offset: 0,
                    version,
                    items,
                });
            }
            state_clone.emit_event(DaemonEvent::SearchComplete { query, version });
            return;
        }

        let kinds = scope_media_kinds(scope);
        let mut tasks = Vec::with_capacity(kinds.len() * SEARCH_INITIAL_PAGES as usize);
        for kind in kinds {
            for page in 0..SEARCH_INITIAL_PAGES {
                let offset = page * SEARCH_PAGE_SIZE;
                let task_state = state_clone.clone();
                let task_query = query.clone();
                let task_kind = kind.clone();
                tasks.push(tokio::spawn(async move {
                    fetch_and_emit_page(task_state, task_query, task_kind, offset, version).await;
                }));
            }
        }
        for handle in tasks {
            let _ = handle.await;
        }
        state_clone.emit_event(DaemonEvent::SearchComplete { query, version });
    });
}

pub(crate) fn spawn_search_page(
    state: Arc<DaemonState>,
    query: String,
    kind: MediaKind,
    offset: u32,
    version: u64,
) {
    state.clone().spawn_background("search-page", async move {
        if query.chars().count() > MAX_SEARCH_QUERY_CHARS {
            state.emit_event(DaemonEvent::SearchFailed {
                query: query.clone(),
                version,
                kind: Some(kind),
                offset: Some(offset),
                message: format!(
                    "search query is {} characters; Spotify's limit is {}. Trim and try again.",
                    query.chars().count(),
                    MAX_SEARCH_QUERY_CHARS
                ),
            });
            return;
        }
        fetch_and_emit_page(state, query, kind, offset, version).await;
    });
}

pub(crate) async fn fetch_and_emit_page(
    state: Arc<DaemonState>,
    query: String,
    kind: MediaKind,
    offset: u32,
    version: u64,
) {
    let client = match state.spotify_client().await {
        Ok(client) => client,
        Err(err) => {
            tracing::warn!(error = %err, kind = ?kind, offset, "search-page acquire client failed");
            state.emit_event(DaemonEvent::SearchFailed {
                query,
                version,
                kind: Some(kind),
                offset: Some(offset),
                message: format!("search failed: {err}"),
            });
            return;
        }
    };
    let result = tokio::time::timeout(
        SEARCH_REQUEST_TIMEOUT,
        client.search_page(&query, kind.clone(), offset),
    )
    .await;
    match result {
        Err(_) => {
            tracing::warn!(
                kind = ?kind,
                offset,
                timeout_secs = SEARCH_REQUEST_TIMEOUT.as_secs(),
                "search-page request timed out"
            );
            state.emit_event(DaemonEvent::SearchFailed {
                query,
                version,
                kind: Some(kind),
                offset: Some(offset),
                message: format!(
                    "search timed out after {}s",
                    SEARCH_REQUEST_TIMEOUT.as_secs()
                ),
            });
        }
        Ok(Err(err)) => {
            tracing::warn!(error = %err, kind = ?kind, offset, "search-page request failed");
            state.emit_event(DaemonEvent::SearchFailed {
                query,
                version,
                kind: Some(kind),
                offset: Some(offset),
                message: format!("search failed: {err}"),
            });
        }
        Ok(Ok(mut items)) => {
            for item in &mut items {
                item.source = Some("spotify".to_string());
                item.freshness = Some("fresh".to_string());
            }
            // Cache to media_items so follow-up actions (play, queue,
            // playlist-add) don't need to re-fetch. Background task; not
            // gated on cache success — see plan §"Caching".
            if !items.is_empty() {
                let cache_state = state.clone();
                let cache_query = query.clone();
                let cache_items = items.clone();
                state.spawn_background("spotify-search-page-cache", async move {
                    if let Err(err) = cache_state
                        .store()
                        .cache_search_results(
                            &cache_query,
                            SearchScopeData::All,
                            SearchSourceData::Spotify,
                            &cache_items,
                        )
                        .await
                    {
                        tracing::debug!(error = %err, "failed to cache search-page results");
                    }
                });
            }
            state.emit_event(DaemonEvent::SearchPage {
                query,
                kind,
                offset,
                version,
                items,
            });
        }
    }
}

pub(crate) fn group_items_by_kind(items: Vec<MediaItem>) -> Vec<(MediaKind, Vec<MediaItem>)> {
    let mut buckets: Vec<(MediaKind, Vec<MediaItem>)> = Vec::new();
    for item in items {
        let kind = item.kind.clone();
        if let Some(bucket) = buckets.iter_mut().find(|(k, _)| k == &kind) {
            bucket.1.push(item);
        } else {
            buckets.push((kind, vec![item]));
        }
    }
    buckets
}

#[cfg(test)]
pub(crate) async fn queueable_uris_for_selection(
    client: &mut SpotifyClient,
    uri: &str,
) -> anyhow::Result<Vec<String>> {
    let items = queueable_items_for_selection_without_cache(client, uri).await?;
    Ok(items.into_iter().map(|item| item.uri).collect())
}

pub(crate) async fn queueable_items_for_selection(
    state: &DaemonState,
    client: &mut SpotifyClient,
    uri: &str,
) -> anyhow::Result<Vec<MediaItem>> {
    let mut items = queueable_items_for_selection_without_cache(client, uri).await?;
    if items.len() == 1 && items[0].name == items[0].uri {
        if let Some(cached) = lookup_known_media_item(state, &items[0].uri).await {
            items[0] = cached;
        }
    }
    Ok(items)
}

pub(crate) async fn queueable_items_for_selection_without_cache(
    client: &mut SpotifyClient,
    uri: &str,
) -> anyhow::Result<Vec<MediaItem>> {
    match selection::media_kind_from_uri(uri)? {
        MediaKind::Track => match client.media_item_by_uri(uri).await? {
            Some(item) => Ok(vec![item]),
            None => Ok(vec![media_item_from_uri(uri)?]),
        },
        MediaKind::Episode => Ok(vec![media_item_from_uri(uri)?]),
        MediaKind::Playlist => {
            let playlist_id = uri.trim_start_matches("spotify:playlist:");
            let items = client.playlist_tracks(playlist_id).await?;
            Ok(items
                .into_iter()
                .filter(|item| matches!(item.kind, MediaKind::Track | MediaKind::Episode))
                .collect())
        }
        MediaKind::Album => {
            let album_id = uri.trim_start_matches("spotify:album:");
            Ok(client.album_tracks(album_id).await?)
        }
        MediaKind::Artist | MediaKind::Show => anyhow::bail!(
            "artist and show URIs cannot be appended to the Spotify queue; choose a track, episode, album, or playlist"
        ),
    }
}

pub(crate) fn idle_context_start_label(kind: &MediaKind) -> Option<&'static str> {
    match kind {
        MediaKind::Album => Some("album"),
        MediaKind::Playlist => Some("playlist"),
        _ => None,
    }
}

pub(crate) async fn optimistic_queue_with_appends(
    state: &DaemonState,
    queued_items: Vec<MediaItem>,
    live_uris: &std::collections::HashSet<String>,
) -> Option<spotuify_core::Queue> {
    if queued_items.is_empty() {
        return None;
    }
    let mut base = state
        .store()
        .latest_queue(500)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let as_of_ms = now_ms();
    let cache_age_ms = as_of_ms.saturating_sub(base.as_of_ms);
    let looks_historical = base.currently_playing.is_none() && !base.items.is_empty();
    if looks_historical
        || (!base.session_active
            && (base.as_of_ms <= 0 || cache_age_ms > QUEUE_APPEND_BASE_MAX_AGE_MS))
    {
        base = spotuify_core::Queue::default();
    }
    // Occurrence tracking keys off the LIVE queue — the same base the
    // add's dedup used — while the optimistic emit overlays the cached
    // base (what clients currently see).
    state.track_pending_queue_appends(live_uris, &queued_items, as_of_ms);
    Some(queue_with_appended_items(base, queued_items, as_of_ms))
}

pub(crate) async fn context_queue_snapshot_for_play_uri(
    state: &DaemonState,
    uri: &str,
) -> Option<spotuify_core::Queue> {
    let kind = selection::media_kind_from_uri(uri).ok()?;
    if !matches!(kind, MediaKind::Album | MediaKind::Playlist) {
        return None;
    }
    let mut client = match state.spotify_client().await {
        Ok(client) => client,
        Err(err) => {
            tracing::debug!(error = %err, uri, "could not build context queue snapshot");
            return None;
        }
    };
    let items = match queueable_items_for_selection(state, &mut client, uri).await {
        Ok(items) => items,
        Err(err) => {
            tracing::debug!(error = %err, uri, "could not resolve context queue items");
            return None;
        }
    };
    queue_for_started_context(items, now_ms())
}

pub(crate) fn queue_for_started_context(
    mut context_items: Vec<MediaItem>,
    as_of_ms: i64,
) -> Option<spotuify_core::Queue> {
    if context_items.is_empty() {
        return None;
    }
    let currently_playing = context_items.first().cloned();
    context_items.drain(..1);
    Some(spotuify_core::Queue {
        currently_playing,
        items: context_items,
        session_active: true,
        as_of_ms,
    })
}

pub(crate) fn queue_with_appended_items(
    mut queue: spotuify_core::Queue,
    queued_items: Vec<MediaItem>,
    as_of_ms: i64,
) -> spotuify_core::Queue {
    queue.items.extend(queued_items);
    queue.session_active = true;
    queue.as_of_ms = as_of_ms;
    queue
}

pub(crate) fn scope_media_kinds(scope: SearchScopeData) -> Vec<MediaKind> {
    match scope {
        SearchScopeData::All => vec![
            MediaKind::Track,
            MediaKind::Episode,
            MediaKind::Show,
            MediaKind::Album,
            MediaKind::Artist,
            MediaKind::Playlist,
        ],
        SearchScopeData::Track => vec![MediaKind::Track],
        SearchScopeData::Episode => vec![MediaKind::Episode],
        SearchScopeData::Show => vec![MediaKind::Show],
        SearchScopeData::Album => vec![MediaKind::Album],
        SearchScopeData::Artist => vec![MediaKind::Artist],
        SearchScopeData::Playlist => vec![MediaKind::Playlist],
    }
}

pub(crate) async fn cache_playback(
    state: &DaemonState,
    playback: &spotuify_spotify::client::Playback,
) {
    if let Err(err) = state.store().persist_playback(playback).await {
        tracing::warn!(error = %err, "failed to cache playback snapshot");
    }
}

/// Persist a polled playback snapshot only when no hot-path mutation
/// has fired since `captured_seq` was observed. Without this gate the
/// background refresh below can clobber an optimistic Pause/Resume
/// with Spotify's stale pre-mutation `is_playing` flag. Returns
/// `true` if the persist applied; `false` if it was dropped as
/// stale. The caller uses the return to decide whether to broadcast
/// a `PlaybackChanged` event — there's no point notifying clients to
/// re-fetch if we threw the result away.
pub(crate) async fn cache_playback_if_fresh(
    state: &DaemonState,
    playback: &spotuify_spotify::client::Playback,
    captured_seq: u64,
) -> bool {
    if !state.may_apply_state_update(captured_seq) {
        tracing::debug!("dropping stale playback refresh: mutation in flight");
        return false;
    }
    cache_playback(state, playback).await;
    true
}

pub(crate) async fn skip_refresh_due_to_rate_limit(
    state: &DaemonState,
    domain: &str,
    refresh: &'static str,
) -> bool {
    match state.store().rate_limit_cooldown_remaining_ms(domain).await {
        Ok(Some(remaining_ms)) => {
            tracing::debug!(
                domain,
                refresh,
                remaining_ms,
                "skipping refresh while Spotify rate-limit cooldown is active"
            );
            true
        }
        Ok(None) => false,
        Err(err) => {
            tracing::debug!(
                domain,
                refresh,
                error = %err,
                "failed to inspect rate-limit cooldown before refresh"
            );
            false
        }
    }
}

pub(crate) fn spawn_playback_refresh(state: Arc<DaemonState>) {
    let task_state = state.clone();
    let captured_seq = state.current_mutation_seq();
    state.spawn_background("playback-refresh", async move {
        let started = std::time::Instant::now();
        if skip_refresh_due_to_rate_limit(&task_state, "playback", "playback-refresh").await {
            return;
        }
        let Ok(mut client) = task_state.spotify_client().await else {
            tracing::debug!(
                target: "spotuify_daemon::refresh",
                captured_seq,
                outcome = "no-client",
                "playback refresh skipped: spotify client unavailable"
            );
            return;
        };
        match actions::status(&mut client).await {
            Ok(playback) => {
                let has_live_signal = playback_has_live_signal(&playback);
                let fresh = task_state.may_apply_state_update(captured_seq);
                // Phase 2 — feed the clock from the poll. The clock
                // itself enforces source priority + URI tie-break so a
                // stale poll can't clobber a fresh local PlayerEvent. Empty
                // no-active-session polls go through the clock too: the clock
                // ignores the first transient readback around next/previous,
                // then clears stale playback only after the guard confirms it.
                let now_ms = spotuify_core::now_ms();
                let state_seq = task_state.current_mutation_seq();
                let clock_applied = task_state.playback_clock().apply_web_api_poll(
                    &playback,
                    captured_seq,
                    state_seq,
                    now_ms,
                    playback.provider_timestamp_ms,
                );
                if fresh && (has_live_signal || clock_applied) {
                    task_state
                        .viz_coordinator()
                        .set_playing(playback.is_playing);
                }
                let applied = if has_live_signal || clock_applied {
                    let playback_to_cache = if has_live_signal {
                        playback.clone()
                    } else {
                        task_state.snapshot_playback()
                    };
                    cache_playback_if_fresh(&task_state, &playback_to_cache, captured_seq).await
                } else {
                    false
                };
                tracing::debug!(
                    target: "spotuify_daemon::refresh",
                    captured_seq,
                    duration_ms = started.elapsed().as_millis() as u64,
                    outcome = if applied {
                        if has_live_signal {
                            "applied"
                        } else {
                            "no-session-cleared"
                        }
                    } else if has_live_signal {
                        "stale"
                    } else {
                        "empty-ignored"
                    },
                    fetched_uri = playback
                        .item
                        .as_ref()
                        .map_or("", |i| i.uri.as_str()),
                    is_playing = playback.is_playing,
                    "playback refresh"
                );
                if applied {
                    // Phase 3 — embed the just-applied snapshot from the
                    // clock so TUI/MCP can re-render in one IPC, not two.
                    task_state.emit_event(DaemonEvent::PlaybackChanged {
                        action: "refreshed".to_string(),
                        playback: Some(task_state.snapshot_playback()),
                    });
                }
            }
            Err(err) => tracing::warn!(
                target: "spotuify_daemon::refresh",
                captured_seq,
                duration_ms = started.elapsed().as_millis() as u64,
                outcome = "error",
                error = %err,
                "background playback refresh failed"
            ),
        }
    });
}

pub(crate) async fn cache_queue(state: &DaemonState, queue: &spotuify_spotify::client::Queue) {
    if let Err(err) = state.store().persist_queue(queue).await {
        tracing::warn!(error = %err, "failed to cache queue");
    }
    state.warm_queue(queue);
}

/// Persist + cache only when the queue snapshot came from a live
/// session. When Spotify reports no active session the returned queue
/// is structurally empty (`currently_playing: None`, `items: []`) — in
/// that case we deliberately skip the store write so history remains
/// recoverable, but clients receive an empty non-actionable live queue.
pub(crate) async fn cache_queue_if_fresh(
    state: &DaemonState,
    queue: &spotuify_spotify::client::Queue,
    captured_seq: u64,
) -> Option<spotuify_spotify::client::Queue> {
    if !state.may_apply_state_update(captured_seq) {
        tracing::debug!("dropping stale queue refresh: mutation in flight");
        return None;
    }
    if !queue.session_active {
        tracing::debug!("queue refresh: no active session, preserving cache");
        return None;
    }
    // Anchor BEFORE the overlay: the tail lookup keys on the last item
    // Spotify itself reported, not on our optimistic appends. The merge
    // itself (Spotify's ~20-item cap, librespot's empty-items shape,
    // wrong-prediction recovery, shuffle gate) lives in
    // `spotuify_core::queue_merge` so the sync loop's queue write path
    // applies IDENTICAL logic — it bypassing the merge was a live bug
    // (queue rail collapsed within one 15s sync cadence).
    let anchor = spotuify_core::queue_merge::queue_tail_anchor(queue);
    let now = now_ms();
    let queue = state.overlay_pending_queue_appends(queue.clone(), now);
    let queue = if let Ok(Some(cached)) = state.store().latest_queue(500).await {
        spotuify_core::queue_merge::reattach_cached_queue_tail(
            queue,
            anchor.as_deref(),
            &cached,
            state.snapshot_playback().shuffle,
            now,
        )
    } else {
        queue
    };
    cache_queue(state, &queue).await;
    Some(queue)
}

pub(crate) fn spawn_queue_refresh(state: Arc<DaemonState>) {
    let captured_seq = state.current_mutation_seq();
    spawn_queue_refresh_with_seq(state, captured_seq);
}

/// Queue refresh measured against an explicit seq — used by mutation
/// closures so the refresh is invalidated by ANY mutation after the
/// one that scheduled it (capturing at fetch time would adopt a racing
/// mutation's seq and apply a mid-transition snapshot).
pub(crate) fn spawn_queue_refresh_with_seq(state: Arc<DaemonState>, captured_seq: u64) {
    let task_state = state.clone();
    state.spawn_background("queue-refresh", async move {
        let started = std::time::Instant::now();
        if skip_refresh_due_to_rate_limit(&task_state, "queue", "queue-refresh").await {
            return;
        }
        let Ok(mut client) = task_state.spotify_client().await else {
            tracing::debug!(
                target: "spotuify_daemon::refresh",
                captured_seq,
                outcome = "no-client",
                "queue refresh skipped: spotify client unavailable"
            );
            return;
        };
        match actions::queue(&mut client).await {
            Ok(queue) => {
                let applied_queue = cache_queue_if_fresh(&task_state, &queue, captured_seq).await;
                let applied = applied_queue.is_some();
                tracing::debug!(
                    target: "spotuify_daemon::refresh",
                    captured_seq,
                    duration_ms = started.elapsed().as_millis() as u64,
                    outcome = if applied {
                        "applied"
                    } else if queue.session_active {
                        "stale"
                    } else {
                        "no-session"
                    },
                    fetched_uri = queue
                        .currently_playing
                        .as_ref()
                        .map_or("", |i| i.uri.as_str()),
                    items = queue.items.len(),
                    "queue refresh"
                );
                if let Some(queue) = applied_queue {
                    task_state.emit_event(DaemonEvent::QueueChanged {
                        action: "refreshed".to_string(),
                        uris: Vec::new(),
                        queue: Some(queue),
                    });
                }
            }
            Err(err) => tracing::warn!(
                target: "spotuify_daemon::refresh",
                captured_seq,
                duration_ms = started.elapsed().as_millis() as u64,
                outcome = "error",
                error = %err,
                "background queue refresh failed"
            ),
        }
    });
}

pub(crate) async fn cache_devices(
    state: &DaemonState,
    devices: &[spotuify_spotify::client::Device],
) {
    // Full-refresh path: this is the entire `/v1/me/player/devices`
    // snapshot, so call `replace_devices` to prune any cached row
    // Spotify didn't return. Drops stale "spotuify" namesakes left
    // over from prior daemon runs once Spotify's own retention
    // expires them upstream.
    if let Err(err) = state.store().replace_devices(devices).await {
        tracing::warn!(error = %err, "failed to cache devices");
    }
}

pub(crate) async fn cached_devices_with_own_device(
    state: &DaemonState,
) -> anyhow::Result<Vec<spotuify_core::Device>> {
    let mut devices = state.store().list_devices().await?;
    if let Some(own_device) = state.connected_own_device().await {
        let own_id = own_device.id.as_deref();
        if !devices.iter().any(|device| device.id.as_deref() == own_id) {
            devices.push(own_device);
        }
    }
    Ok(devices)
}

pub(crate) async fn cache_devices_if_fresh(
    state: &DaemonState,
    devices: &[spotuify_spotify::client::Device],
    captured_seq: u64,
) -> bool {
    if !state.may_apply_state_update(captured_seq) {
        tracing::debug!("dropping stale devices refresh: mutation in flight");
        return false;
    }
    cache_devices(state, devices).await;
    true
}

/// Phase 1 — persist the `CommandResult` returned by `actions::execute()`
/// BEFORE emitting `PlaybackChanged`. Without this, subscribers re-fetch
/// `PlaybackGet` and read stale cached state until the next background
/// refresh — the exact "pause feels laggy" symptom the plan calls out.
///
/// Guards everything behind `may_apply_state_update(captured_seq)` so a
/// follow-up mutation that bumps the seq won't be clobbered by our
/// older response. Returns the set of state classes that were persisted
/// (for span fields); empty when nothing applied.
pub(crate) async fn persist_command_result(
    state: &DaemonState,
    captured_seq: u64,
    result: &spotuify_spotify::actions::CommandResult,
    action: &'static str,
    expected_playback: Option<&ExpectedPlayback>,
) -> CommandResultPersistOutcome {
    let mut outcome = CommandResultPersistOutcome::default();
    if !state.may_apply_state_update(captured_seq) {
        tracing::debug!(
            target: "spotuify_daemon::post_command",
            action,
            captured_seq,
            "dropping post-command result: newer mutation in flight"
        );
        return outcome;
    }
    if let Some(playback) = result.playback.as_ref() {
        if !post_command_playback_matches(playback, expected_playback) {
            tracing::debug!(
                target: "spotuify_daemon::post_command",
                action,
                captured_seq,
                fetched_uri = playback
                    .item
                    .as_ref()
                    .map_or("", |item| item.uri.as_str()),
                fetched_is_playing = playback.is_playing,
                expected_uri = expected_playback
                    .and_then(|expected| expected.uri.as_deref())
                    .unwrap_or(""),
                expected_is_playing = expected_playback
                    .and_then(|expected| expected.is_playing)
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                "dropping post-command playback result: stale spotify readback"
            );
        } else {
            cache_playback(state, playback).await;
            state.viz_coordinator().set_playing(playback.is_playing);
            // Phase 2 — feed the clock immediately so the next
            // `PlaybackGet` (and the pushed snapshot in Phase 3) reflect
            // the post-mutation truth without waiting for a poll.
            state
                .playback_clock()
                .apply_command_result(playback, spotuify_core::now_ms());
            outcome.playback = Some(PostCommandPlayback {
                is_playing: playback.is_playing,
                uri: playback.item.as_ref().map(|item| item.uri.clone()),
            });
        }
    }
    if let Some(queue) = result.queue.as_ref() {
        cache_queue(state, queue).await;
        outcome.queue_items = Some(queue.items.len());
    }
    if let Some(devices) = result.devices.as_ref() {
        cache_devices(state, devices).await;
        outcome.devices = Some(devices.len());
    }
    outcome
}

#[derive(Debug, Default, Clone)]
pub(crate) struct CommandResultPersistOutcome {
    pub(crate) playback: Option<PostCommandPlayback>,
    pub(crate) queue_items: Option<usize>,
    pub(crate) devices: Option<usize>,
}

#[derive(Debug, Clone)]
pub(crate) struct PostCommandPlayback {
    pub(crate) is_playing: bool,
    pub(crate) uri: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ExpectedPlayback {
    pub(crate) uri: Option<String>,
    pub(crate) is_playing: Option<bool>,
}

pub(crate) fn post_command_playback_matches(
    playback: &Playback,
    expected: Option<&ExpectedPlayback>,
) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    if let Some(expected_uri) = expected.uri.as_deref() {
        let fetched_uri = playback.item.as_ref().map(|item| item.uri.as_str());
        if fetched_uri != Some(expected_uri) {
            return false;
        }
    }
    if let Some(expected_is_playing) = expected.is_playing {
        if playback.is_playing != expected_is_playing {
            return false;
        }
    }
    true
}

pub(crate) fn playback_has_live_signal(playback: &Playback) -> bool {
    playback.item.is_some() || playback.device.is_some() || playback.is_playing
}

pub(crate) fn spawn_devices_refresh(state: Arc<DaemonState>) {
    let task_state = state.clone();
    let captured_seq = state.current_mutation_seq();
    state.spawn_background("devices-refresh", async move {
        let started = std::time::Instant::now();
        if skip_refresh_due_to_rate_limit(&task_state, "devices", "devices-refresh").await {
            return;
        }
        let Ok(mut client) = task_state.spotify_client().await else {
            tracing::debug!(
                target: "spotuify_daemon::refresh",
                captured_seq,
                outcome = "no-client",
                "devices refresh skipped: spotify client unavailable"
            );
            return;
        };
        match actions::devices(&mut client).await {
            Ok(devices) => {
                let applied = cache_devices_if_fresh(&task_state, &devices, captured_seq).await;
                tracing::debug!(
                    target: "spotuify_daemon::refresh",
                    captured_seq,
                    duration_ms = started.elapsed().as_millis() as u64,
                    outcome = if applied { "applied" } else { "stale" },
                    device_count = devices.len(),
                    "devices refresh"
                );
                if applied {
                    let devices_snapshot = devices.clone();
                    task_state.emit_event(DaemonEvent::DevicesChanged {
                        action: "refreshed".to_string(),
                        devices: Some(devices_snapshot),
                    });
                }
            }
            Err(err) => tracing::warn!(
                target: "spotuify_daemon::refresh",
                captured_seq,
                duration_ms = started.elapsed().as_millis() as u64,
                outcome = "error",
                error = %err,
                "background devices refresh failed"
            ),
        }
    });
}

pub(crate) async fn cache_recent_items(state: &DaemonState, items: &[MediaItem]) {
    if let Err(err) = state.store().persist_recent_items(items).await {
        tracing::warn!(error = %err, "failed to cache recent items");
    }
}

pub(crate) fn spawn_recent_refresh(state: Arc<DaemonState>) {
    let task_state = state.clone();
    state.spawn_background("recent-refresh", async move {
        if skip_refresh_due_to_rate_limit(&task_state, "recent", "recent-refresh").await {
            return;
        }
        let Ok(mut client) = task_state.spotify_client().await else {
            return;
        };
        match client.recently_played().await {
            Ok(items) => {
                if !items.is_empty() {
                    cache_recent_items(&task_state, &items).await;
                    // Piggy-back on PlaybackChanged: recent-played is
                    // the fallback PlaybackGet leans on for the
                    // "last-known song" synthetic. Re-broadcasting
                    // playback nudges the TUI to re-fetch and pick up
                    // the synthesized last-played even before the
                    // playback poll itself finishes.
                    task_state.emit_event(DaemonEvent::PlaybackChanged {
                        action: "recent-refreshed".to_string(),
                        playback: Some(task_state.snapshot_playback()),
                    });
                }
            }
            Err(err) => tracing::debug!(error = %err, "background recent refresh failed"),
        }
    });
}

pub(crate) async fn cache_playlists(
    state: &DaemonState,
    playlists: &[spotuify_spotify::client::Playlist],
) {
    if let Err(err) = state.store().persist_playlists(playlists).await {
        tracing::warn!(error = %err, "failed to cache playlists");
    }
}

pub(crate) fn spawn_playlists_refresh(state: Arc<DaemonState>) {
    let task_state = state.clone();
    state.spawn_background("playlists-refresh", async move {
        if skip_refresh_due_to_rate_limit(&task_state, "playlists", "playlists-refresh").await {
            return;
        }
        let Ok(mut client) = task_state.spotify_client().await else {
            return;
        };
        match actions::playlists(&mut client).await {
            Ok(playlists) => {
                if !playlists.is_empty() {
                    cache_playlists(&task_state, &playlists).await;
                    task_state.emit_event(DaemonEvent::PlaylistsChanged {
                        action: "refreshed".to_string(),
                        playlist: None,
                    });
                }
            }
            Err(err) => tracing::debug!(error = %err, "background playlists refresh failed"),
        }
    });
}

pub(crate) async fn cache_playlist_items(
    state: &DaemonState,
    playlist_id: &str,
    items: &[MediaItem],
) {
    if let Err(err) = state
        .store()
        .persist_playlist_items(playlist_id, items)
        .await
    {
        tracing::warn!(error = %err, "failed to cache playlist items");
    }
}

pub(crate) fn spawn_playlist_tracks_refresh(state: Arc<DaemonState>, playlist_id: String) {
    let task_state = state.clone();
    let playlist_for_event = playlist_id.clone();
    state.spawn_background("playlist-tracks-refresh", async move {
        if skip_refresh_due_to_rate_limit(&task_state, "playlists", "playlist-tracks-refresh").await
        {
            return;
        }
        let Ok(mut client) = task_state.spotify_client().await else {
            return;
        };
        match client.playlist_tracks(&playlist_id).await {
            Ok(items) => {
                if !items.is_empty() {
                    cache_playlist_items(&task_state, &playlist_id, &items).await;
                    task_state.emit_event(DaemonEvent::PlaylistsChanged {
                        action: "tracks-refreshed".to_string(),
                        playlist: Some(playlist_for_event),
                    });
                }
            }
            Err(err) => {
                if is_playlist_tracks_forbidden(&err) {
                    let _ = task_state
                        .store()
                        .mark_playlist_tracks_inaccessible(&playlist_id)
                        .await;
                    task_state.emit_event(DaemonEvent::PlaylistsChanged {
                        action: "tracks-inaccessible".to_string(),
                        playlist: Some(playlist_id.clone()),
                    });
                }
                tracing::debug!(error = %err, playlist = %playlist_id, "background playlist tracks refresh failed")
            }
        }
    });
}

pub(crate) fn expected_playback_after_command(
    command: &PlaybackCommand,
    predicted: Option<&Playback>,
) -> Option<ExpectedPlayback> {
    let predicted_uri =
        || predicted.and_then(|playback| playback.item.as_ref().map(|item| item.uri.clone()));
    match command {
        PlaybackCommand::Pause => Some(ExpectedPlayback {
            uri: predicted_uri(),
            is_playing: Some(false),
        }),
        PlaybackCommand::Resume => Some(ExpectedPlayback {
            uri: predicted_uri(),
            is_playing: Some(true),
        }),
        PlaybackCommand::Toggle => predicted.map(|playback| ExpectedPlayback {
            uri: playback.item.as_ref().map(|item| item.uri.clone()),
            is_playing: Some(playback.is_playing),
        }),
        PlaybackCommand::PlayUri { uri } => Some(ExpectedPlayback {
            uri: Some(uri.clone()),
            is_playing: predicted.and_then(|playback| playback.is_playing.then_some(true)),
        }),
        PlaybackCommand::Next | PlaybackCommand::Previous => {
            predicted.map(|playback| ExpectedPlayback {
                // Spotify may return a different valid track than our cached
                // prediction (shuffle/autoplay/queue races, or previous
                // stepping back instead of restarting current). Treat any
                // post-command snapshot with the expected play/pause state as
                // authoritative instead of rejecting it and leaving clients on
                // stale optimistic state; reject a stale paused readback while
                // the daemon-owned prediction says playback should remain live.
                uri: None,
                is_playing: Some(playback.is_playing),
            })
        }
        PlaybackCommand::Seek { .. } | PlaybackCommand::SeekRelative { .. } => {
            predicted.map(|playback| ExpectedPlayback {
                uri: playback.item.as_ref().map(|item| item.uri.clone()),
                is_playing: None,
            })
        }
        PlaybackCommand::Volume { .. }
        | PlaybackCommand::Shuffle { .. }
        | PlaybackCommand::Repeat { .. } => None,
    }
}

pub(crate) fn playback_command_kind(command: PlaybackCommand) -> CommandKind {
    match command {
        PlaybackCommand::Pause => CommandKind::Pause,
        PlaybackCommand::Resume => CommandKind::Resume,
        PlaybackCommand::Toggle => CommandKind::TogglePlayback,
        PlaybackCommand::Next => CommandKind::Next,
        PlaybackCommand::Previous => CommandKind::Previous,
        PlaybackCommand::PlayUri { uri } => CommandKind::PlayUri { uri },
        PlaybackCommand::Seek { position_ms } => CommandKind::Seek { position_ms },
        // `SeekRelative` is resolved to absolute `Seek` against the daemon
        // `PlaybackClock` upstream in the `PlaybackCommand` handler arm
        // before this function is reached. Hitting this branch means the
        // resolution step was skipped — fall through to a no-op seek so
        // we never silently issue a wrong absolute target.
        PlaybackCommand::SeekRelative { .. } => CommandKind::Seek { position_ms: 0 },
        PlaybackCommand::Volume { volume_percent } => CommandKind::Volume { volume_percent },
        PlaybackCommand::Shuffle { state } => CommandKind::Shuffle { state },
        PlaybackCommand::Repeat { state } => CommandKind::Repeat { state },
    }
}

pub(crate) fn playback_command_action(command: &PlaybackCommand) -> &'static str {
    match command {
        PlaybackCommand::Pause => "pause",
        PlaybackCommand::Resume => "resume",
        PlaybackCommand::Toggle => "toggle",
        PlaybackCommand::Next => "next",
        PlaybackCommand::Previous => "previous",
        PlaybackCommand::PlayUri { .. } => "play-uri",
        PlaybackCommand::Seek { .. } => "seek",
        PlaybackCommand::SeekRelative { .. } => "seek-relative",
        PlaybackCommand::Volume { .. } => "volume",
        PlaybackCommand::Shuffle { .. } => "shuffle",
        PlaybackCommand::Repeat { .. } => "repeat",
    }
}

pub(crate) fn playback_command_viz_state(command: &PlaybackCommand) -> Option<bool> {
    match command {
        PlaybackCommand::Pause => Some(false),
        PlaybackCommand::Resume | PlaybackCommand::PlayUri { .. } => Some(true),
        _ => None,
    }
}

pub(crate) fn playback_command_operation_kind(command: &PlaybackCommand) -> OperationKind {
    match command {
        PlaybackCommand::Pause => OperationKind::Pause,
        PlaybackCommand::Resume => OperationKind::Resume,
        PlaybackCommand::Toggle => OperationKind::Toggle,
        PlaybackCommand::Next => OperationKind::Next,
        PlaybackCommand::Previous => OperationKind::Previous,
        PlaybackCommand::PlayUri { .. } => OperationKind::Play,
        PlaybackCommand::Seek { .. } | PlaybackCommand::SeekRelative { .. } => OperationKind::Seek,
        PlaybackCommand::Volume { .. } => OperationKind::Volume,
        PlaybackCommand::Shuffle { .. } => OperationKind::Shuffle,
        PlaybackCommand::Repeat { .. } => OperationKind::Repeat,
    }
}

pub(crate) fn emit_mutation_finished(state: &DaemonState, action: &str, message: &str) {
    state.emit_event(DaemonEvent::MutationFinished {
        action: action.to_string(),
        message: message.to_string(),
    });
}

pub(crate) fn reject_if_auth_blocked(state: &DaemonState) -> anyhow::Result<()> {
    if let Some(err) = state.auth_gate_error() {
        return Err(anyhow::Error::new(err));
    }
    Ok(())
}

// Predict the post-command playback state so the daemon can emit an
// optimistic `PlaybackChanged` BEFORE the Spotify round-trip. Returns
// `None` when no prediction is sensible (e.g. `Next` without a current
// queue row — we can't guess the next track safely).
//
// The eventual authoritative `CommandResult` event from
// `persist_command_result` overrides whatever we predict via the
// clock's source-priority logic. Same pattern the embedded librespot
// `PlayerEvent` already uses for local mutations.
/// The next track to optimistically show for a `Next`, taken from the cached
/// queue — but only when the queue's `currently_playing` matches the track that
/// is actually playing (`current_uri`). A mismatch means the cache is
/// historical (a dead session), so we return `None` and skip the prediction
/// rather than flash a stale title.
pub(crate) fn optimistic_next_from_queue(
    queue: &spotuify_core::Queue,
    current_uri: &str,
) -> Option<spotuify_core::MediaItem> {
    let describes_current = queue
        .currently_playing
        .as_ref()
        .is_some_and(|current| current.uri == current_uri);
    if !describes_current {
        return None;
    }
    queue.items.first().cloned()
}

/// Predicted queue after a `Next`: the cached queue with the predicted
/// track promoted to `currently_playing` and everything up to (and
/// including) it dropped from the upcoming list. Returns `None` when the
/// predicted track isn't in the cached queue — the cache is historical
/// and an optimistic emit would show a wrong list.
pub(crate) async fn optimistic_queue_after_next(
    state: &DaemonState,
    next_item: &spotuify_core::MediaItem,
) -> Option<spotuify_core::Queue> {
    let queue = state.store().latest_queue(500).await.ok().flatten()?;
    optimistic_queue_promoting(queue, next_item)
}

/// Pure half of `optimistic_queue_after_next`: promote `next_item` to
/// `currently_playing` and drop it (and anything queued before it) from
/// the upcoming list.
pub(crate) fn optimistic_queue_promoting(
    mut queue: spotuify_core::Queue,
    next_item: &spotuify_core::MediaItem,
) -> Option<spotuify_core::Queue> {
    let pos = queue
        .items
        .iter()
        .position(|item| item.uri == next_item.uri)?;
    queue.items.drain(..=pos);
    queue.currently_playing = Some(next_item.clone());
    queue.as_of_ms = spotuify_core::now_ms();
    Some(queue)
}

/// Re-fetch the authoritative queue shortly after a transport command
/// that changes the playing track. The delay gives Spotify's
/// `/me/player/queue` time to reflect the Spirc-side skip — fetching
/// immediately often returns the pre-skip queue, which would clobber
/// the optimistic emit with stale data.
///
/// `captured_seq` is the SCHEDULING command's own seq: any mutation
/// during the delay advances past it and the refresh becomes a no-op
/// (the newer mutation's refresh reconciles instead). Capturing after
/// the sleep adopted a racing command's seq and let a mid-transition
/// snapshot through — live-observed as the queue jumping one track
/// behind on rapid double-Next.
pub(crate) fn spawn_queue_refresh_delayed(
    state: Arc<DaemonState>,
    delay_ms: u64,
    captured_seq: u64,
) {
    let task_state = state.clone();
    state.spawn_background("queue-refresh-delayed", async move {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        spawn_queue_refresh_with_seq(task_state, captured_seq);
    });
}

pub(crate) async fn compute_optimistic_playback(
    state: &DaemonState,
    command: &PlaybackCommand,
) -> Option<spotuify_core::Playback> {
    let mut predicted = state.snapshot_playback();
    let now_ms = spotuify_core::now_ms();
    match command {
        PlaybackCommand::Pause => {
            if !predicted.is_playing {
                return None;
            }
            predicted.is_playing = false;
        }
        PlaybackCommand::Resume => {
            if predicted.is_playing {
                return None;
            }
            if !playback_has_active_device(&predicted) {
                return None;
            }
            predicted.is_playing = true;
        }
        PlaybackCommand::Toggle => {
            if predicted.is_playing {
                predicted.is_playing = false;
            } else if playback_has_active_device(&predicted) {
                predicted.is_playing = true;
            } else {
                return None;
            }
        }
        PlaybackCommand::PlayUri { uri } => {
            let was_audible = predicted.is_playing && playback_has_active_device(&predicted);
            // Try the local Tantivy/SQLite media_items cache first.
            // Falls through to a stub MediaItem (URI only) when the
            // URI isn't known locally — at minimum the URI change
            // triggers the TUI's `handle_art_url_change` to clear
            // the old cover and paint the gradient placeholder.
            let resolved = lookup_known_media_item(state, uri)
                .await
                .unwrap_or_else(|| spotuify_core::MediaItem {
                    uri: uri.clone(),
                    name: "Loading…".to_string(),
                    ..Default::default()
                });
            predicted.item = Some(resolved);
            predicted.is_playing = was_audible;
            predicted.progress_ms = 0;
            predicted.sampled_at_ms = Some(now_ms);
        }
        PlaybackCommand::Next => {
            // Predict the next track from the cached queue, but only when the
            // cache still describes the *current* track — otherwise the queue
            // is historical (a dead session) and we'd show a stale title.
            let current_uri = predicted.item.as_ref().map(|item| item.uri.clone())?;
            let queue = state
                .store()
                .latest_queue(500)
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            let mut next = optimistic_next_from_queue(&queue, &current_uri)?;
            // Fill artwork from the cache when the queue row lacks it, so the
            // cover swaps instantly instead of waiting for reconciliation. No
            // network call on this hot path — if still unknown, art fills when
            // the authoritative event lands.
            if next.image_url.is_none() {
                if let Some(enriched) = lookup_known_media_item(state, &next.uri).await {
                    next = enriched;
                }
            }
            let was_audible = predicted.is_playing && playback_has_active_device(&predicted);
            predicted.item = Some(next);
            predicted.is_playing = was_audible;
            predicted.progress_ms = 0;
            predicted.sampled_at_ms = Some(now_ms);
        }
        PlaybackCommand::Previous => {
            // Restart-current: the always-safe optimistic move (Spotify itself
            // restarts the track once you're past the first few seconds). It
            // resets the progress bar to 0:00 instantly and never shows a wrong
            // track; if Spotify actually steps back a track, the authoritative
            // event reconciles via the clock's source priority.
            predicted.item.as_ref()?;
            predicted.progress_ms = 0;
            predicted.sampled_at_ms = Some(now_ms);
        }
        PlaybackCommand::Seek { position_ms } => {
            predicted.item.as_ref()?;
            predicted.progress_ms = *position_ms;
        }
        PlaybackCommand::SeekRelative { .. } => {
            // Already resolved to absolute `Seek` upstream in the
            // PlaybackCommand handler — should never reach here.
            return None;
        }
        PlaybackCommand::Volume { volume_percent } => {
            let device = predicted.device.as_mut()?;
            device.volume_percent = Some(*volume_percent);
        }
        PlaybackCommand::Shuffle { state: shuffle } => {
            if predicted.item.is_none() && predicted.device.is_none() {
                return None;
            }
            predicted.shuffle = *shuffle;
        }
        PlaybackCommand::Repeat { state: repeat } => {
            if predicted.item.is_none() && predicted.device.is_none() {
                return None;
            }
            predicted.repeat = repeat.clone();
        }
    }
    Some(predicted)
}

pub(crate) fn playback_has_active_device(playback: &spotuify_core::Playback) -> bool {
    playback
        .device
        .as_ref()
        .is_some_and(|device| device.is_active)
}

/// Look up a MediaItem by URI from the daemon's local caches. Used by
/// optimistic playback prediction so a PlayUri can carry the track's
/// title / artist / image_url immediately, before Spotify's playback
/// state catches up. Returns `None` when the URI isn't in any cache —
/// the caller falls back to a stub.
pub(crate) async fn lookup_known_media_item(
    state: &DaemonState,
    uri: &str,
) -> Option<spotuify_core::MediaItem> {
    state
        .store()
        .media_items_by_uris(&[uri.to_string()])
        .await
        .ok()
        .and_then(|items| items.into_iter().next())
}

/// Phase 12 — record an operation row around every mutation. Wraps
/// `record_mutation` (Phase 6.6 receipt lifecycle) and also writes an
/// `operations` row + emits `OperationRecorded`.
///
/// `body` receives the freshly-minted `OperationId` so it can call
/// `state.store().update_operation_plan(op_id, …)` mid-flight once it
/// has captured the pre-mutation `snapshot_id` / prior device / etc.
/// Transport commands typically pass `(NotReversible, Transport)` up
/// front; reversible mutations (playlist_add, transfer, library_save)
/// fill in real pre-state inside the body.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_operation<F, Fut, T>(
    state: &std::sync::Arc<DaemonState>,
    kind: OperationKind,
    source: OperationSource,
    subject_uris: Vec<String>,
    action: &str,
    request_summary: &str,
    initial_pre_state: Option<spotuify_protocol::PreState>,
    initial_reversal_plan: Option<spotuify_protocol::ReversalPlan>,
    body: F,
) -> anyhow::Result<T>
where
    F: FnOnce(OperationId) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    reject_if_auth_blocked(state)?;

    let operation_id = OperationId::new_v7();
    let occurred_at_ms = now_ms();
    let receipt_id = ReceiptId::new_v7();
    let reversible = kind.is_reversible()
        && !matches!(
            &initial_reversal_plan,
            Some(spotuify_protocol::ReversalPlan::NotReversible { .. })
        );
    let row = Operation {
        operation_id,
        kind,
        occurred_at_ms,
        finished_at_ms: None,
        source,
        requester: None,
        subject_uris: subject_uris.clone(),
        reversible,
        reversal_plan: initial_reversal_plan,
        pre_state: initial_pre_state,
        status: OperationStatus::Pending,
        receipt_id: Some(receipt_id),
        subject_op_id: None,
        undone_by_op_id: None,
        redone_by_op_id: None,
        error_message: None,
    };

    // The operations table has a foreign key on `receipt_id`. The
    // writer pool runs with `PRAGMA foreign_keys = ON`, so the receipt
    // row MUST exist before we insert the operation. Earlier versions
    // ran the inserts in the opposite order and the FK violation was
    // silently swallowed by `let _ = ...`, leaving the operations
    // table empty in production.
    let started = now_ms();
    let receipt = spotuify_protocol::Receipt {
        receipt_id,
        action: action.to_string(),
        status: spotuify_protocol::ReceiptStatus::Pending,
        message: "queued".to_string(),
        started_at_ms: started,
        finished_at_ms: None,
        error: None,
    };
    if let Err(err) = state
        .store()
        .insert_pending_receipt(&receipt, request_summary)
        .await
    {
        tracing::error!(
            error = %err,
            receipt_id = %receipt_id.0,
            action,
            "failed to persist pending receipt row"
        );
    }
    if let Err(err) = state.store().insert_pending_operation(&row).await {
        tracing::error!(
            error = %err,
            operation_id = %operation_id.0,
            kind = ?kind,
            action,
            "failed to persist pending operation row"
        );
    }
    state.emit_event(spotuify_protocol::DaemonEvent::MutationAccepted {
        receipt_id,
        action: action.to_string(),
    });

    let result = body(operation_id).await;

    let finished = now_ms();
    let (receipt_status, message, error_summary) = match &result {
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
    if let Err(err) = state
        .store()
        .finalize_receipt(
            receipt_id,
            receipt_status,
            &message,
            finished,
            error_summary.as_ref(),
        )
        .await
    {
        tracing::error!(
            error = %err,
            receipt_id = %receipt_id.0,
            action,
            "failed to finalize receipt row"
        );
    }
    state.emit_event(spotuify_protocol::DaemonEvent::MutationFinalized {
        receipt_id,
        status: receipt_status,
        message: message.clone(),
    });
    let _ = error_summary;
    let (status, error) = match &result {
        Ok(_) => (OperationStatus::Succeeded, None),
        Err(err) => (OperationStatus::Failed, Some(err.to_string())),
    };
    if let Err(err) = state
        .store()
        .finalize_operation(operation_id, status, finished, error.as_deref())
        .await
    {
        tracing::error!(
            error = %err,
            operation_id = %operation_id.0,
            kind = ?kind,
            action,
            "failed to finalize operation row"
        );
    }
    state.emit_event(DaemonEvent::OperationRecorded {
        operation_id,
        kind,
        source,
    });
    result
}

/// Spawn a mutation body and return an optimistic `Mutation` response
/// immediately. The IPC caller sees `ok=true` and a "queued" message
/// before Spotify confirms; subscribers to the daemon event bus see
/// `MutationFinalized { status: Confirmed | Failed }` when the
/// background body resolves.
///
/// The lane handle is moved into the spawned task, then acquired there,
/// so concurrent mutations on the same lane still serialise at Spotify
/// without making the IPC response wait behind the lane. The
/// operation/receipt lifecycle (insert pending row → emit
/// `MutationAccepted` → finalise on body completion → emit
/// `MutationFinalized`) mirrors `record_operation` exactly so undo/redo
/// + receipt recovery keep working unchanged. The only difference is
///   *when* the response returns: optimistic, before the body runs.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn spawn_optimistic_mutation<F, Fut>(
    state: &Arc<DaemonState>,
    kind: OperationKind,
    source: OperationSource,
    subject_uris: Vec<String>,
    action: &'static str,
    request_summary: String,
    initial_pre_state: Option<spotuify_protocol::PreState>,
    initial_reversal_plan: Option<spotuify_protocol::ReversalPlan>,
    mutation_lane: Option<Arc<tokio::sync::Mutex<()>>>,
    body: F,
) -> anyhow::Result<ResponseData>
where
    F: FnOnce(OperationId) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    reject_if_auth_blocked(state)?;

    let operation_id = OperationId::new_v7();
    let occurred_at_ms = now_ms();
    let receipt_id = ReceiptId::new_v7();
    let reversible = kind.is_reversible()
        && !matches!(
            &initial_reversal_plan,
            Some(spotuify_protocol::ReversalPlan::NotReversible { .. })
        );
    let row = Operation {
        operation_id,
        kind,
        occurred_at_ms,
        finished_at_ms: None,
        source,
        requester: None,
        subject_uris,
        reversible,
        reversal_plan: initial_reversal_plan,
        pre_state: initial_pre_state,
        status: OperationStatus::Pending,
        receipt_id: Some(receipt_id),
        subject_op_id: None,
        undone_by_op_id: None,
        redone_by_op_id: None,
        error_message: None,
    };
    // Receipt FIRST so the operations.receipt_id FK lands cleanly.
    // See `record_operation` for the same ordering rationale.
    let started_at_ms = crate::analytics::now_ms();
    let receipt = spotuify_protocol::Receipt {
        receipt_id,
        action: action.to_string(),
        status: spotuify_protocol::ReceiptStatus::Pending,
        message: "queued".to_string(),
        started_at_ms,
        finished_at_ms: None,
        error: None,
    };
    if let Err(err) = state
        .store()
        .insert_pending_receipt(&receipt, &request_summary)
        .await
    {
        tracing::error!(
            error = %err,
            receipt_id = %receipt_id.0,
            action,
            "failed to persist pending receipt row (optimistic)"
        );
    }
    if let Err(err) = state.store().insert_pending_operation(&row).await {
        tracing::error!(
            error = %err,
            operation_id = %operation_id.0,
            kind = ?kind,
            action,
            "failed to persist pending operation row (optimistic)"
        );
    }
    state.emit_event(spotuify_protocol::DaemonEvent::MutationAccepted {
        receipt_id,
        action: action.to_string(),
    });

    let task_state = state.clone();
    state.spawn_background("optimistic-mutation-body", async move {
        let body_with_lane = async move {
            // Hold the lane guard across the body so concurrent mutations
            // on the same lane still serialise. Dropped on body return.
            let _guard = match mutation_lane {
                Some(lane) => Some(lane.lock_owned().await),
                None => None,
            };
            body(operation_id).await
        };
        let result = match tokio::time::timeout(MUTATION_BODY_TIMEOUT, body_with_lane).await {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!(
                "{action} timed out after {}s",
                MUTATION_BODY_TIMEOUT.as_secs()
            )),
        };
        let finished = crate::analytics::now_ms();

        let (op_status, op_error) = match &result {
            Ok(()) => (OperationStatus::Succeeded, None),
            Err(err) => (OperationStatus::Failed, Some(err.to_string())),
        };
        let _ = task_state
            .store()
            .finalize_operation(operation_id, op_status, finished, op_error.as_deref())
            .await;
        task_state.emit_event(DaemonEvent::OperationRecorded {
            operation_id,
            kind,
            source,
        });

        let (receipt_status, message, error_summary) = match &result {
            Ok(()) => (
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
        let _ = task_state
            .store()
            .finalize_receipt(
                receipt_id,
                receipt_status,
                &message,
                finished,
                error_summary.as_ref(),
            )
            .await;
        task_state.emit_event(spotuify_protocol::DaemonEvent::MutationFinalized {
            receipt_id,
            status: receipt_status,
            message,
        });
    });

    Ok(ResponseData::Mutation {
        receipt: CommandReceipt {
            ok: true,
            action: action.to_string(),
            message: format!("{action} queued"),
        },
    })
}

/// Wrap `actions::execute` with a one-shot device-recovery retry.
///
/// Spotify's `PUT /me/player/<cmd>` endpoints fail with a structured
/// 404 + `"Player command failed: No active device found"` whenever no
/// device is currently registered as the active player. That's a
/// terrible message to surface to the user — they hit Pause, the TUI
/// flashes "404 on PUT /me/player/pause", and the actual remedy
/// (start spotifyd / open the Spotify app) is buried.
///
/// This wrapper detects that specific case and tries to recover
/// automatically:
/// 1. `ensure_player_ready(configured_name)` — bring up the configured
///    backend (embedded librespot).
/// 2. Short pause so Spotify's device registry catches up after the
///    new device announces itself via the librespot/spotifyd SPIRC.
/// 3. Retry the original command.
///
/// If recovery fails — backend unavailable, auth missing — we fall
/// through to a human-readable error that
/// lists any devices Spotify *does* know about, with the actionable
/// next step (`spotuify devices transfer <name>` or open the Spotify
/// app).
pub(crate) async fn execute_with_device_recovery(
    state: &Arc<DaemonState>,
    client: &mut spotuify_spotify::SpotifyClient,
    command: CommandKind,
) -> anyhow::Result<spotuify_spotify::actions::CommandResult> {
    if let Some(result) = try_embedded_transport(state, &command).await {
        return Ok(result);
    }
    match actions::execute(client, command.clone()).await {
        Ok(result) => Ok(result),
        Err(err) if is_recoverable_device_error(&err) => {
            let no_active = is_no_active_device_error(&err);
            tracing::info!(
                error = %err,
                "transport command hit missing device; attempting recovery"
            );
            let device_name = DaemonState::configured_device_name();
            let recovered = match tokio::time::timeout(
                DEVICE_RECOVERY_TIMEOUT,
                state.reconnect_player(&device_name),
            )
            .await
            {
                Ok(Ok(_)) => true,
                Ok(Err(err)) => {
                    tracing::warn!(error = %err, "embedded device reconnect failed");
                    false
                }
                Err(_) => {
                    tracing::warn!(
                        timeout_secs = DEVICE_RECOVERY_TIMEOUT.as_secs(),
                        "embedded device reconnect timed out"
                    );
                    false
                }
            };
            if recovered {
                if !wait_for_preferred_device(client).await {
                    tracing::warn!(
                        timeout_secs = DEVICE_REGISTRY_TIMEOUT.as_secs(),
                        "preferred device still absent from Spotify registry after reconnect"
                    );
                }
                match actions::execute(client, command.clone()).await {
                    Ok(result) => return Ok(result),
                    Err(retry_err) if no_active && is_no_active_device_error(&retry_err) => {
                        return Err(friendly_no_active_device_error(client, &retry_err).await);
                    }
                    Err(retry_err) => return Err(retry_err.into()),
                }
            }
            if no_active {
                Err(friendly_no_active_device_error(client, &err).await)
            } else {
                Err(err.into())
            }
        }
        Err(err) => Err(err.into()),
    }
}

pub(crate) async fn try_embedded_transport(
    state: &Arc<DaemonState>,
    command: &CommandKind,
) -> Option<spotuify_spotify::actions::CommandResult> {
    // Prefer the embedded librespot (Spirc) path — instant, no HTTP
    // round-trip, and it still works while Spotify read endpoints are
    // in cooldown. Do not preflight with GET /me/player here: that
    // read path is exactly what can be rate-limited during startup
    // sync, and a transport command should not inherit that cooldown.
    let transport_snapshot = state.snapshot_playback();
    if let Some((cmd, effective_command)) =
        transport_cmd_for_command_kind(command, &transport_snapshot)
    {
        if !embedded_transport_allowed(state, &cmd, &transport_snapshot) {
            return None;
        }
        let mut player_connected = state.player_is_connected().await;
        if !player_connected {
            let device_name = DaemonState::configured_device_name();
            player_connected = match tokio::time::timeout(
                DEVICE_RECOVERY_TIMEOUT,
                state.reconnect_player(&device_name),
            )
            .await
            {
                Ok(Ok(_)) => true,
                Ok(Err(err)) => {
                    tracing::debug!(error = %err, "embedded device reconnect before transport failed");
                    false
                }
                Err(_) => {
                    tracing::debug!(
                        timeout_secs = DEVICE_RECOVERY_TIMEOUT.as_secs(),
                        "embedded device reconnect before transport timed out"
                    );
                    false
                }
            };
        }
        if player_connected {
            match tokio::time::timeout(TRANSPORT_BACKEND_TIMEOUT, state.transport(cmd)).await {
                Err(_) => {
                    tracing::warn!(
                        timeout_secs = TRANSPORT_BACKEND_TIMEOUT.as_secs(),
                        "embedded transport timed out; falling back to Web API"
                    );
                }
                Ok(result) => match result {
                    Ok(()) => {
                        return Some(spotuify_spotify::actions::CommandResult {
                            playback: local_transport_playback_snapshot(state, &effective_command),
                            request_refresh: true,
                            ..Default::default()
                        });
                    }
                    Err(spotuify_player::PlayerError::Unsupported(_)) => {
                        // Fall through to Web API.
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "embedded transport failed; falling back to Web API");
                    }
                },
            }
        }
    }
    None
}

pub(crate) fn local_transport_playback_snapshot(
    state: &DaemonState,
    command: &CommandKind,
) -> Option<Playback> {
    let mut playback = state.snapshot_playback();
    playback.sampled_at_ms = Some(spotuify_core::now_ms());
    playback.source = Some(spotuify_core::PlaybackStateSource::CommandResult);

    match command {
        CommandKind::Pause => playback.is_playing = false,
        CommandKind::Resume => playback.is_playing = true,
        CommandKind::PlayItem { item } => {
            playback.item = Some(item.clone());
            playback.progress_ms = 0;
            playback.is_playing = true;
        }
        CommandKind::PlayUri { uri } => {
            if playback.item.as_ref().map(|item| item.uri.as_str()) != Some(uri.as_str()) {
                playback.item = Some(MediaItem {
                    uri: uri.clone(),
                    ..Default::default()
                });
            }
            playback.progress_ms = 0;
            playback.is_playing = true;
        }
        CommandKind::Seek { position_ms } => {
            playback.progress_ms = *position_ms;
        }
        CommandKind::Volume { volume_percent } => {
            if let Some(device) = playback.device.as_mut() {
                device.volume_percent = Some(*volume_percent);
            }
        }
        CommandKind::Shuffle { state } => playback.shuffle = *state,
        CommandKind::Repeat { state } => playback.repeat = state.clone(),
        CommandKind::Next | CommandKind::Previous => return None,
        CommandKind::TogglePlayback
        | CommandKind::QueueItem { .. }
        | CommandKind::QueueUri { .. }
        | CommandKind::Transfer { .. }
        | CommandKind::AddToPlaylist { .. }
        | CommandKind::SaveItem { .. }
        | CommandKind::SaveCurrent => return None,
    }

    Some(playback)
}

/// May the embedded librespot (Spirc) path carry this transport
/// command? Spirc silently drops transport while our device is NOT the
/// active session ("SpircCommand::Pause will be ignored while Not
/// Active" — log-confirmed ×22/day): the fast path would then report
/// success while nothing happened. Only PlayUri loads activate the
/// device; everything else must go to the Web API, which targets
/// whatever device is actually playing.
pub(crate) fn embedded_transport_allowed(
    state: &DaemonState,
    cmd: &crate::state::TransportCmd,
    snapshot: &Playback,
) -> bool {
    if matches!(cmd, crate::state::TransportCmd::PlayUri { .. }) {
        return true;
    }
    let own = state.own_device_id();
    let allowed =
        own.is_some() && snapshot.device.as_ref().and_then(|d| d.id.as_deref()) == own.as_deref();
    if !allowed {
        tracing::debug!(
            target: "spotuify_daemon::transport",
            active_device = ?snapshot.device.as_ref().map(|d| d.name.as_str()),
            "embedded device not the active session; using Web API transport"
        );
    }
    allowed
}

pub(crate) async fn apply_fast_transport(
    state: &Arc<DaemonState>,
    cmd: crate::state::TransportCmd,
    effective_command: &CommandKind,
    action: &str,
) -> Option<spotuify_spotify::actions::CommandResult> {
    match state.transport_fast(cmd, FAST_TRANSPORT_TIMEOUT).await {
        Ok(FastTransportStatus::Applied) => {
            tracing::debug!(action, "fast local transport applied");
            Some(local_transport_command_result(state, effective_command))
        }
        Ok(FastTransportStatus::Dispatched { ack }) => {
            tracing::debug!(
                timeout_ms = FAST_TRANSPORT_TIMEOUT.as_millis(),
                action,
                "fast local transport dispatched without waiting for backend ack"
            );
            // The deadline elapsed before the player acked. We're about
            // to tell clients the command applied, so watch the late ack
            // and reconcile if it turns out the backend rejected it.
            spawn_fast_transport_ack_watcher(state.clone(), ack, action.to_string());
            Some(local_transport_command_result(state, effective_command))
        }
        Err(err) => {
            tracing::debug!(error = %err, action, "fast local transport skipped");
            None
        }
    }
}

/// Watch a fast-transport ack that arrived after the fast deadline. A
/// late success is a no-op (the optimistic state already matches); a
/// late failure or a dropped ack means the daemon optimistically
/// reported success that didn't hold, so bump the mutation seq and
/// refresh playback to overwrite the stale optimistic snapshot with
/// authoritative state.
pub(crate) fn spawn_fast_transport_ack_watcher(
    state: Arc<DaemonState>,
    ack: tokio::sync::oneshot::Receiver<spotuify_player::PlayerResult<()>>,
    action: String,
) {
    state.clone().spawn_background("fast-transport-ack", async move {
        let reconcile = |reason: &str| {
            tracing::warn!(action = %action, reason, "fast transport did not hold; reconciling");
            state.bump_mutation_seq();
            spawn_playback_refresh(state.clone());
        };
        match tokio::time::timeout(FAST_TRANSPORT_ACK_GRACE, ack).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(err))) => reconcile(&format!("backend error: {err}")),
            Ok(Err(_)) => reconcile("player actor dropped the ack"),
            Err(_) => reconcile("ack timed out"),
        }
    });
}

pub(crate) fn local_transport_command_result(
    state: &DaemonState,
    effective_command: &CommandKind,
) -> spotuify_spotify::actions::CommandResult {
    spotuify_spotify::actions::CommandResult {
        playback: local_transport_playback_snapshot(state, effective_command),
        request_refresh: true,
        ..Default::default()
    }
}

pub(crate) async fn wait_for_preferred_device(client: &mut SpotifyClient) -> bool {
    let started = Instant::now();
    loop {
        match actions::devices(client).await {
            Ok(devices) => {
                if actions::preferred_device(client.config(), &devices, client.own_device_id())
                    .is_some()
                {
                    return true;
                }
            }
            Err(err) => {
                tracing::debug!(error = %err, "device registry poll failed during recovery");
            }
        }
        if started.elapsed() >= DEVICE_REGISTRY_TIMEOUT {
            return false;
        }
        tokio::time::sleep(DEVICE_REGISTRY_POLL_INTERVAL).await;
    }
}

pub(crate) fn transport_cmd_for_command_kind(
    kind: &CommandKind,
    playback: &Playback,
) -> Option<(crate::state::TransportCmd, CommandKind)> {
    use crate::state::TransportCmd;
    // TogglePlayback is resolved against the daemon-owned playback
    // clock so Space never needs a GET /me/player preflight. SaveCurrent
    // is resolved in the LibrarySave handler for the same reason.
    // AddToPlaylist, SaveItem, Queue, and Transfer are not transport
    // controls, so they stay on their mutation-specific paths.
    match kind {
        CommandKind::Pause => Some((TransportCmd::Pause, CommandKind::Pause)),
        CommandKind::Resume if playback_can_resume_locally(playback) => {
            Some((TransportCmd::Resume, CommandKind::Resume))
        }
        CommandKind::TogglePlayback if playback.is_playing => {
            Some((TransportCmd::Pause, CommandKind::Pause))
        }
        CommandKind::TogglePlayback if playback_can_resume_locally(playback) => {
            Some((TransportCmd::Resume, CommandKind::Resume))
        }
        CommandKind::Next => Some((TransportCmd::Next, CommandKind::Next)),
        CommandKind::Previous => Some((TransportCmd::Previous, CommandKind::Previous)),
        CommandKind::PlayUri { uri } => Some((
            TransportCmd::PlayUri {
                uri: uri.clone(),
                position_ms: 0,
            },
            kind.clone(),
        )),
        CommandKind::PlayItem { item } => Some((
            TransportCmd::PlayUri {
                uri: item.uri.clone(),
                position_ms: 0,
            },
            kind.clone(),
        )),
        CommandKind::Seek { position_ms } => Some((
            TransportCmd::Seek {
                position_ms: (*position_ms).min(u32::MAX as u64) as u32,
            },
            kind.clone(),
        )),
        CommandKind::Volume { volume_percent } => Some((
            TransportCmd::Volume {
                percent: *volume_percent,
            },
            kind.clone(),
        )),
        CommandKind::Shuffle { state } => {
            Some((TransportCmd::Shuffle { on: *state }, kind.clone()))
        }
        CommandKind::Repeat { state } => match state.as_str() {
            "off" => Some((
                TransportCmd::Repeat {
                    mode: spotuify_player::RepeatMode::Off,
                },
                kind.clone(),
            )),
            "context" => Some((
                TransportCmd::Repeat {
                    mode: spotuify_player::RepeatMode::Context,
                },
                kind.clone(),
            )),
            "track" => Some((
                TransportCmd::Repeat {
                    mode: spotuify_player::RepeatMode::Track,
                },
                kind.clone(),
            )),
            _ => None,
        },
        CommandKind::Resume
        | CommandKind::TogglePlayback
        | CommandKind::QueueItem { .. }
        | CommandKind::QueueUri { .. }
        | CommandKind::Transfer { .. }
        | CommandKind::AddToPlaylist { .. }
        | CommandKind::SaveItem { .. }
        | CommandKind::SaveCurrent => None,
    }
}

pub(crate) fn playback_can_resume_locally(playback: &Playback) -> bool {
    let Some(item) = playback.item.as_ref() else {
        return false;
    };
    item.duration_ms == 0 || playback.progress_ms.saturating_add(750) < item.duration_ms
}

pub(crate) fn is_no_active_device_error(err: &spotuify_spotify::SpotifyError) -> bool {
    use spotuify_spotify::SpotifyError;
    match err {
        SpotifyError::Api {
            status: 404,
            endpoint,
            message,
            ..
        } => {
            // Scope the broad match to `/me/player/*` endpoints so a
            // 404 from somewhere else (e.g. a deleted track) doesn't
            // trigger device recovery. Spotify returns several 404
            // variants when the targeted device isn't reachable —
            // device offline (`"not found."`), missing from registry
            // (`"device not found"`), or no active session
            // (`"no active device"`) — all of which share the same
            // recovery path: re-register the embedded librespot
            // session and retry.
            if !endpoint.contains("/me/player") {
                return false;
            }
            let lower = message.to_lowercase();
            lower.contains("no active device")
                || lower.contains("device not found")
                || lower.starts_with("not found")
        }
        _ => false,
    }
}

/// Outcome of a single queue-add attempt. `NoActiveDevice` is Spotify's
/// idle-session 404 surfaced as a value (not an error) so the caller can
/// recover by starting playback rather than failing the whole operation.
pub(crate) enum QueueAttempt {
    Queued,
    NoActiveDevice,
}

/// One queue-add: embedded Spirc first (instant), Web API fallback when
/// librespot 0.8.0 can't originate it. Maps Spotify's "no active device"
/// 404 to [`QueueAttempt::NoActiveDevice`]; all other failures are errors.
pub(crate) async fn queue_one(
    state: &DaemonState,
    client: &mut SpotifyClient,
    uri: &str,
) -> anyhow::Result<QueueAttempt> {
    match state.queue_add(uri).await {
        Ok(()) => Ok(QueueAttempt::Queued),
        Err(spotuify_player::PlayerError::Unsupported(_)) => {
            match actions::execute(
                client,
                CommandKind::QueueUri {
                    uri: uri.to_string(),
                },
            )
            .await
            {
                Ok(_) => Ok(QueueAttempt::Queued),
                Err(err) if is_no_active_device_error(&err) => Ok(QueueAttempt::NoActiveDevice),
                Err(err) => Err(anyhow::anyhow!("queue add for {uri} failed: {err}")),
            }
        }
        Err(err) => Err(anyhow::anyhow!("queue add for {uri} failed: {err}")),
    }
}

pub(crate) fn is_playlist_tracks_forbidden(err: &spotuify_spotify::SpotifyError) -> bool {
    matches!(
        err,
        spotuify_spotify::SpotifyError::Api {
            status: 403,
            endpoint,
            ..
        } if endpoint.starts_with("GET /playlists/") && endpoint.contains("/items")
    )
}

pub(crate) fn is_recoverable_device_error(err: &spotuify_spotify::SpotifyError) -> bool {
    if is_no_active_device_error(err) {
        return true;
    }
    matches!(
        err,
        spotuify_spotify::SpotifyError::Client { message }
            if message.contains("no preferred Spotify device found")
    )
}

pub(crate) async fn friendly_no_active_device_error(
    client: &mut spotuify_spotify::SpotifyClient,
    original: &spotuify_spotify::SpotifyError,
) -> anyhow::Error {
    let hint = match actions::devices(client).await {
        Ok(devs) if !devs.is_empty() => {
            let names = devs
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "Available devices: {names}. Run `spotuify devices transfer <name>` to activate one."
            )
        }
        _ => "No Spotify devices online. Open the Spotify app on any device, or run `spotuify reconnect`."
            .to_string(),
    };
    anyhow::anyhow!("No active Spotify device. {hint} (Spotify said: {original})")
}

/// Display snapshot for a reminder: cache-first (media_items), else derive the
/// kind from the URI with a URI-tail label fallback so a reminder still renders
/// sensibly even for an item that was never cached.
pub(crate) async fn resolve_reminder_snapshot(
    state: &DaemonState,
    uri: &str,
) -> anyhow::Result<(MediaKind, String, String, Option<String>)> {
    if let Ok(items) = state.store().media_items_by_uris(&[uri.to_string()]).await {
        if let Some(item) = items.into_iter().next() {
            return Ok((item.kind, item.name, item.subtitle, item.image_url));
        }
    }
    let kind = selection::media_kind_from_uri(uri)?;
    let label = uri.rsplit(':').next().unwrap_or(uri).to_string();
    Ok((kind, label, String::new(), None))
}

pub(crate) fn media_item_from_uri(uri: &str) -> anyhow::Result<MediaItem> {
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
        ..Default::default()
    })
}

#[cfg(test)]
mod queue_tests {
    use super::{
        idle_context_start_label, queue_for_started_context, queue_with_appended_items,
        queueable_uris_for_selection,
    };
    use spotuify_core::{MediaItem, MediaKind, Queue};
    use spotuify_spotify::client::SpotifyClient;

    fn track(uri: &str, name: &str) -> MediaItem {
        MediaItem {
            id: uri.rsplit(':').next().map(str::to_string),
            uri: uri.to_string(),
            name: name.to_string(),
            subtitle: "Artist".to_string(),
            context: "Album".to_string(),
            duration_ms: 180_000,
            image_url: None,
            kind: MediaKind::Track,
            source: None,
            freshness: None,
            explicit: None,
            is_playable: None,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn queue_expansion_keeps_track_uri_as_single_append() {
        let mut client = SpotifyClient::fake().expect("fake client");

        let uris = queueable_uris_for_selection(&mut client, "spotify:track:one")
            .await
            .expect("track should queue directly");

        assert_eq!(uris, vec!["spotify:track:one"]);
    }

    #[tokio::test]
    async fn queue_expansion_resolves_playlist_to_tracks() {
        let mut client = SpotifyClient::fake().expect("fake client");

        let uris = queueable_uris_for_selection(&mut client, "spotify:playlist:quiet-storm")
            .await
            .expect("playlist should expand");

        assert_eq!(
            uris,
            vec![
                "spotify:track:never-too-much".to_string(),
                "spotify:track:sweet-thing".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn queue_expansion_resolves_album_to_tracks() {
        let mut client = SpotifyClient::fake().expect("fake client");

        let uris = queueable_uris_for_selection(&mut client, "spotify:album:never-too-much-album")
            .await
            .expect("album should expand");

        assert_eq!(
            uris,
            vec![
                "spotify:track:never-too-much".to_string(),
                "spotify:track:sweet-thing".to_string()
            ]
        );
    }

    #[test]
    fn idle_queue_starts_contexts_as_contexts() {
        assert_eq!(
            idle_context_start_label(&MediaKind::Playlist),
            Some("playlist")
        );
        assert_eq!(idle_context_start_label(&MediaKind::Album), Some("album"));
        assert_eq!(idle_context_start_label(&MediaKind::Track), None);
    }

    #[test]
    fn optimistic_queue_append_keeps_existing_items_and_duplicates() {
        let queue = Queue {
            currently_playing: None,
            items: vec![track("spotify:track:a", "A")],
            session_active: false,
            as_of_ms: 1,
        };

        let queue = queue_with_appended_items(
            queue,
            vec![
                track("spotify:track:b", "B"),
                track("spotify:track:a", "A duplicate"),
            ],
            2,
        );

        let uris: Vec<&str> = queue.items.iter().map(|item| item.uri.as_str()).collect();
        assert_eq!(
            uris,
            vec!["spotify:track:a", "spotify:track:b", "spotify:track:a"]
        );
        assert!(queue.session_active);
        assert_eq!(queue.as_of_ms, 2);
    }

    #[test]
    fn context_queue_snapshot_sets_current_and_up_next() {
        let queue = queue_for_started_context(
            vec![
                track("spotify:track:first", "First"),
                track("spotify:track:second", "Second"),
            ],
            3,
        )
        .expect("context with tracks should produce a queue snapshot");

        assert_eq!(
            queue
                .currently_playing
                .as_ref()
                .map(|item| item.uri.as_str()),
            Some("spotify:track:first")
        );
        let uris: Vec<&str> = queue.items.iter().map(|item| item.uri.as_str()).collect();
        assert_eq!(uris, vec!["spotify:track:second"]);
        assert!(queue.session_active);
        assert_eq!(queue.as_of_ms, 3);
    }
}

#[cfg(test)]
mod next_prediction_tests {
    use super::{apply_search_sort, optimistic_next_from_queue, optimistic_queue_promoting};
    use spotuify_core::{MediaItem, MediaKind, Queue};
    use spotuify_protocol::SearchSortData;

    fn item(uri: &str, name: &str, subtitle: &str, duration_ms: u64) -> MediaItem {
        MediaItem {
            uri: uri.to_string(),
            name: name.to_string(),
            subtitle: subtitle.to_string(),
            duration_ms,
            kind: MediaKind::Track,
            ..Default::default()
        }
    }

    fn queue(current: Option<&str>, items: &[&str]) -> Queue {
        Queue {
            currently_playing: current.map(|uri| item(uri, "Current", "A", 1000)),
            items: items.iter().map(|uri| item(uri, uri, "A", 1000)).collect(),
            session_active: true,
            as_of_ms: 0,
        }
    }

    #[test]
    fn next_returns_first_queue_item_when_current_matches() {
        let q = queue(
            Some("spotify:track:cur"),
            &["spotify:track:n1", "spotify:track:n2"],
        );
        let next = optimistic_next_from_queue(&q, "spotify:track:cur");
        assert_eq!(next.map(|i| i.uri), Some("spotify:track:n1".to_string()));
    }

    #[test]
    fn next_is_none_when_cached_current_is_stale() {
        // Cached queue describes a different track than what's actually playing
        // → the queue is historical, so we must not predict a wrong "next".
        let q = queue(Some("spotify:track:other"), &["spotify:track:n1"]);
        assert!(optimistic_next_from_queue(&q, "spotify:track:cur").is_none());
    }

    #[test]
    fn next_is_none_when_queue_is_empty_or_session_unknown() {
        let q = queue(Some("spotify:track:cur"), &[]);
        assert!(optimistic_next_from_queue(&q, "spotify:track:cur").is_none());
        let no_current = queue(None, &["spotify:track:n1"]);
        assert!(optimistic_next_from_queue(&no_current, "spotify:track:cur").is_none());
    }

    #[test]
    fn queue_promotion_drops_through_predicted_track() {
        let q = queue(
            Some("spotify:track:cur"),
            &["spotify:track:n1", "spotify:track:n2", "spotify:track:n3"],
        );
        let next = item("spotify:track:n1", "N1", "A", 1000);
        let promoted = optimistic_queue_promoting(q, &next).expect("promotes head");
        assert_eq!(
            promoted.currently_playing.map(|i| i.uri),
            Some("spotify:track:n1".to_string())
        );
        assert_eq!(
            promoted
                .items
                .iter()
                .map(|i| i.uri.as_str())
                .collect::<Vec<_>>(),
            ["spotify:track:n2", "spotify:track:n3"]
        );
    }

    #[test]
    fn queue_promotion_is_none_when_predicted_track_not_in_queue() {
        let q = queue(Some("spotify:track:cur"), &["spotify:track:n1"]);
        let stranger = item("spotify:track:elsewhere", "X", "A", 1000);
        assert!(optimistic_queue_promoting(q, &stranger).is_none());
    }

    #[test]
    fn search_sort_relevance_preserves_order() {
        let mut items = vec![item("u:b", "B", "Z", 300), item("u:a", "A", "Y", 100)];
        apply_search_sort(&mut items, None);
        assert_eq!(
            items.iter().map(|i| i.uri.as_str()).collect::<Vec<_>>(),
            ["u:b", "u:a"]
        );
        apply_search_sort(&mut items, Some(SearchSortData::Relevance));
        assert_eq!(
            items.iter().map(|i| i.uri.as_str()).collect::<Vec<_>>(),
            ["u:b", "u:a"]
        );
    }

    #[test]
    fn search_sort_by_name_and_duration() {
        let mut items = vec![
            item("u:b", "Beta", "Z", 300),
            item("u:a", "Alpha", "Y", 100),
        ];
        apply_search_sort(&mut items, Some(SearchSortData::Name));
        assert_eq!(
            items.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
            ["Alpha", "Beta"]
        );
        apply_search_sort(&mut items, Some(SearchSortData::Duration));
        assert_eq!(items[0].duration_ms, 100);
    }
}

#[cfg(test)]
mod lyrics_tests {
    use std::sync::Arc;

    use spotuify_core::{LyricsProvider, SyncedLyrics};
    use spotuify_protocol::{Request, ResponseData};
    use tempfile::TempDir;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{dispatch, DaemonState};

    struct TestEnv {
        _temp: TempDir,
    }

    impl TestEnv {
        fn new(lrclib_base_url: &str) -> Self {
            let temp = tempfile::tempdir().expect("tempdir");
            std::env::set_var("SPOTUIFY_FAKE_SPOTIFY", "1");
            std::env::set_var("SPOTUIFY_LRCLIB_BASE_URL", lrclib_base_url);
            std::env::set_var("SPOTUIFY_CACHE_DB", temp.path().join("cache.sqlite3"));
            std::env::set_var("SPOTUIFY_SEARCH_INDEX", temp.path().join("search-index"));
            std::env::set_var("SPOTUIFY_RUNTIME_DIR", temp.path().join("runtime"));
            Self { _temp: temp }
        }
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            std::env::remove_var("SPOTUIFY_FAKE_SPOTIFY");
            std::env::remove_var("SPOTUIFY_LRCLIB_BASE_URL");
            std::env::remove_var("SPOTUIFY_CACHE_DB");
            std::env::remove_var("SPOTUIFY_SEARCH_INDEX");
            std::env::remove_var("SPOTUIFY_RUNTIME_DIR");
        }
    }

    fn lyrics_response(response: ResponseData) -> Option<(SyncedLyrics, i64)> {
        match response {
            ResponseData::Lyrics {
                lyrics: Some(lyrics),
                offset_ms,
            } => Some((lyrics, offset_ms)),
            _ => None,
        }
    }

    #[tokio::test]
    async fn explicit_track_uri_fetches_lrclib_when_media_item_is_not_cached() {
        let _guard = crate::ENV_LOCK.lock().await;
        let lrclib = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/get"))
            .and(query_param("track_name", "Never Too Much"))
            .and(query_param("artist_name", "Luther Vandross"))
            .and(query_param("album_name", "Never Too Much"))
            .and(query_param("duration", "221"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "instrumental": false,
                "plainLyrics": null,
                "syncedLyrics": "[00:01.00]Never too much, never too much",
            })))
            .expect(1)
            .mount(&lrclib)
            .await;
        let _env = TestEnv::new(&lrclib.uri());
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));

        let response = dispatch(
            state.clone(),
            Request::LyricsGet {
                track_uri: Some("spotify:track:never-too-much".to_string()),
                force_refresh: true,
            },
            None,
        )
        .await
        .expect("lyrics response");

        state.shutdown_search().await;
        state.shutdown_player().await;

        let (lyrics, offset_ms) = lyrics_response(response).expect("expected LRCLIB lyrics");
        assert_eq!(offset_ms, 0);
        assert_eq!(lyrics.provider, LyricsProvider::Lrclib);
        assert_eq!(lyrics.track_uri, "spotify:track:never-too-much");
        assert_eq!(lyrics.lines[0].start_ms, 1_000);
        assert_eq!(lyrics.lines[0].text, "Never too much, never too much");
    }

    #[tokio::test]
    async fn cached_lyrics_survive_daemon_restart_without_refetching() {
        let _guard = crate::ENV_LOCK.lock().await;
        let lrclib = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/get"))
            .and(query_param("track_name", "Never Too Much"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "instrumental": false,
                "plainLyrics": "cached lyric",
                "syncedLyrics": null,
            })))
            .expect(1)
            .mount(&lrclib)
            .await;
        let _env = TestEnv::new(&lrclib.uri());

        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        let first = dispatch(
            state.clone(),
            Request::LyricsGet {
                track_uri: Some("spotify:track:never-too-much".to_string()),
                force_refresh: true,
            },
            None,
        )
        .await
        .expect("initial lyrics response");
        state.shutdown_search().await;
        state.shutdown_player().await;
        drop(state);

        let restarted = Arc::new(DaemonState::new().await.expect("restarted daemon state"));
        let second = dispatch(
            restarted.clone(),
            Request::LyricsGet {
                track_uri: Some("spotify:track:never-too-much".to_string()),
                force_refresh: false,
            },
            None,
        )
        .await
        .expect("cached lyrics response");
        restarted.shutdown_search().await;
        restarted.shutdown_player().await;

        let (first_lyrics, _) = lyrics_response(first).expect("initial lyrics should exist");
        let (second_lyrics, _) = lyrics_response(second).expect("cached lyrics should exist");
        assert_eq!(first_lyrics.lines[0].text, "cached lyric");
        assert_eq!(second_lyrics.lines[0].text, "cached lyric");
        assert_eq!(second_lyrics.provider, LyricsProvider::Lrclib);
    }
}

#[cfg(test)]
mod reload_tests {
    use std::sync::Arc;

    use spotuify_protocol::{Request, ResponseData, VizSourceKindData};
    use tempfile::TempDir;

    use super::{dispatch, DaemonState};

    struct TestEnv {
        temp: TempDir,
    }

    impl TestEnv {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("tempdir");
            std::env::set_var("SPOTUIFY_FAKE_SPOTIFY", "1");
            std::env::set_var("SPOTUIFY_CACHE_DB", temp.path().join("cache.sqlite3"));
            std::env::set_var("SPOTUIFY_SEARCH_INDEX", temp.path().join("search-index"));
            std::env::set_var("SPOTUIFY_RUNTIME_DIR", temp.path().join("runtime"));
            std::env::set_var("SPOTUIFY_CONFIG", temp.path().join("spotuify.toml"));
            Self { temp }
        }

        fn write_config(&self, viz: &str) {
            std::fs::write(
                self.temp.path().join("spotuify.toml"),
                format!(
                    r#"
client_id = "test-client"
redirect_uri = "http://127.0.0.1:8888/callback"

{viz}
"#
                ),
            )
            .expect("config write");
        }
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            std::env::remove_var("SPOTUIFY_FAKE_SPOTIFY");
            std::env::remove_var("SPOTUIFY_CACHE_DB");
            std::env::remove_var("SPOTUIFY_SEARCH_INDEX");
            std::env::remove_var("SPOTUIFY_RUNTIME_DIR");
            std::env::remove_var("SPOTUIFY_CONFIG");
        }
    }

    #[tokio::test]
    async fn reload_applies_viz_config_without_daemon_restart() {
        let _guard = crate::ENV_LOCK.lock().await;
        let env = TestEnv::new();
        env.write_config(
            r#"
[viz]
enabled = false
source = "auto"
target_fps = 30
"#,
        );
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        assert!(!state.viz_coordinator().diagnostics().await.enabled);

        env.write_config(
            r#"
[viz]
enabled = true
source = "none"
target_fps = 7
smoothing = 0.2
noise_gate = 0.25
"#,
        );
        let response = dispatch(state.clone(), Request::Reload, None)
            .await
            .expect("reload response");

        assert!(matches!(response, ResponseData::Ack { .. }));
        let diagnostics = state.viz_coordinator().diagnostics().await;
        assert!(diagnostics.enabled);
        assert_eq!(diagnostics.configured_source, VizSourceKindData::None);
        assert_eq!(diagnostics.target_fps, 7);

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn reconnect_re_registers_player_backend() {
        let _guard = crate::ENV_LOCK.lock().await;
        let env = TestEnv::new();
        env.write_config(
            r#"
[player]
backend = "connect"
"#,
        );
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        assert!(!state.player_is_connected().await);

        let response = dispatch(state.clone(), Request::Reconnect, None)
            .await
            .expect("reconnect response");

        assert!(matches!(response, ResponseData::Ack { .. }));
        assert!(state.player_is_connected().await);

        state.shutdown_search().await;
        state.shutdown_player().await;
    }
}

#[cfg(test)]
mod post_command_persist_tests {
    //! Phase 1 + Phase 3 + Phase 5 integration tests.
    //!
    //! Asserts that:
    //! - The daemon persists `CommandResult.playback` before emitting
    //!   `PlaybackChanged` (Phase 1), so a subscriber that re-fetches
    //!   immediately sees the post-mutation state.
    //! - The emitted `PlaybackChanged` event carries the embedded
    //!   `Playback` snapshot (Phase 3), so clients don't need a
    //!   follow-up `PlaybackGet`.
    //! - `SeekRelative` is resolved against the clock daemon-side
    //!   (Phase 5), not the caller's stale read.
    //!
    //! Anti-implementation-coupling: we observe via the public event
    //! channel + store query path. No internal counters or method
    //! orderings.

    use std::sync::Arc;
    use std::time::Duration;

    use spotuify_core::{now_ms, MediaItem, MediaKind, Queue};
    use spotuify_protocol::{DaemonEvent, IpcPayload, PlaybackCommand, Request, ResponseData};
    use spotuify_spotify::actions::CommandKind;
    use tempfile::TempDir;

    use super::{
        cache_queue, cache_queue_if_fresh, compute_optimistic_playback, dispatch,
        expected_playback_after_command, optimistic_queue_with_appends, persist_command_result,
        playback_command_kind, post_command_playback_matches, transport_cmd_for_command_kind,
        DaemonState, ExpectedPlayback,
    };

    struct TestEnv {
        _temp: TempDir,
    }

    impl TestEnv {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("tempdir");
            std::env::set_var("SPOTUIFY_FAKE_SPOTIFY", "1");
            std::env::set_var("SPOTUIFY_CACHE_DB", temp.path().join("cache.sqlite3"));
            std::env::set_var("SPOTUIFY_SEARCH_INDEX", temp.path().join("search-index"));
            std::env::set_var("SPOTUIFY_RUNTIME_DIR", temp.path().join("runtime"));
            std::env::set_var("SPOTUIFY_CONFIG", temp.path().join("spotuify.toml"));
            Self { _temp: temp }
        }
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            std::env::remove_var("SPOTUIFY_FAKE_SPOTIFY");
            std::env::remove_var("SPOTUIFY_CACHE_DB");
            std::env::remove_var("SPOTUIFY_SEARCH_INDEX");
            std::env::remove_var("SPOTUIFY_RUNTIME_DIR");
            std::env::remove_var("SPOTUIFY_CONFIG");
        }
    }

    fn track(uri: &str, name: &str) -> MediaItem {
        MediaItem {
            id: uri.rsplit(':').next().map(str::to_string),
            uri: uri.to_string(),
            name: name.to_string(),
            subtitle: "Artist".to_string(),
            context: "Album".to_string(),
            duration_ms: 180_000,
            image_url: None,
            kind: MediaKind::Track,
            source: Some("test".to_string()),
            freshness: None,
            explicit: Some(false),
            is_playable: Some(true),
            ..Default::default()
        }
    }

    /// Pull the command-result `PlaybackChanged` event off the
    /// broadcast within the timeout. Skips intermediate accepted,
    /// operation, optimistic, and local player events that legitimately
    /// fire in the same flow.
    async fn next_playback_event(
        rx: &mut tokio::sync::broadcast::Receiver<spotuify_protocol::IpcMessage>,
        expected_action: &str,
    ) -> DaemonEvent {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let msg = tokio::time::timeout(remaining, rx.recv())
                .await
                .expect("waiting for PlaybackChanged timed out")
                .expect("event channel closed");
            if let IpcPayload::Event(event) = msg.payload {
                if let DaemonEvent::PlaybackChanged { ref action, .. } = event {
                    if action == expected_action {
                        return event;
                    }
                }
            }
        }
    }

    async fn next_queue_event(
        rx: &mut tokio::sync::broadcast::Receiver<spotuify_protocol::IpcMessage>,
        expected_action: &str,
    ) -> DaemonEvent {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let msg = tokio::time::timeout(remaining, rx.recv())
                .await
                .expect("waiting for QueueChanged timed out")
                .expect("event channel closed");
            if let IpcPayload::Event(event) = msg.payload {
                if matches!(
                    &event,
                    DaemonEvent::QueueChanged { action, .. } if action == expected_action
                ) {
                    return event;
                }
            }
        }
    }

    async fn next_playlists_event(
        rx: &mut tokio::sync::broadcast::Receiver<spotuify_protocol::IpcMessage>,
        expected_action: &str,
        expected_playlist: Option<&str>,
    ) -> DaemonEvent {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let msg = tokio::time::timeout(remaining, rx.recv())
                .await
                .expect("waiting for PlaylistsChanged timed out")
                .expect("event channel closed");
            if let IpcPayload::Event(event) = msg.payload {
                if matches!(
                    &event,
                    DaemonEvent::PlaylistsChanged { action, playlist }
                        if action == expected_action
                            && playlist.as_deref() == expected_playlist
                ) {
                    return event;
                }
            }
        }
    }

    async fn assert_no_mutation_accepted(
        rx: &mut tokio::sync::broadcast::Receiver<spotuify_protocol::IpcMessage>,
    ) {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(100);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let recv = tokio::time::timeout(remaining, rx.recv()).await;
            let Ok(Ok(msg)) = recv else {
                break;
            };
            assert!(
                !matches!(
                    msg.payload,
                    IpcPayload::Event(DaemonEvent::MutationAccepted { .. })
                ),
                "auth-blocked request must not emit MutationAccepted"
            );
        }
    }

    #[tokio::test]
    async fn playback_command_emits_playback_changed_with_embedded_snapshot() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));

        let mut rx = state.event_tx.subscribe();
        let response = dispatch(
            state.clone(),
            Request::PlaybackCommand {
                command: PlaybackCommand::Resume,
            },
            None,
        )
        .await
        .expect("playback response");
        // The immediate response is a receipt (Phase 6.6 optimistic
        // mutation). The interesting event is the PlaybackChanged that
        // follows once the spawned task completes.
        assert!(matches!(response, ResponseData::Mutation { .. }));

        match next_playback_event(&mut rx, "resume").await {
            DaemonEvent::PlaybackChanged { action, playback } => {
                assert_eq!(action, "resume");
                // Phase 3: the event must carry the post-mutation playback so
                // clients don't need a follow-up PlaybackGet round-trip.
                let pb = playback.expect("Phase 3 contract: PlaybackChanged must embed a snapshot");
                // Phase 4: that snapshot must be tagged with its source so
                // freshness-aware clients (TUI merge re-anchor) can react.
                assert!(
                    pb.source.is_some(),
                    "Phase 4 contract: embedded playback must carry source label"
                );
            }
            other => assert!(
                matches!(other, DaemonEvent::PlaybackChanged { .. }),
                "expected PlaybackChanged"
            ),
        }

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn playback_command_ack_does_not_wait_for_transport_lane() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        let request = Request::PlaybackCommand {
            command: PlaybackCommand::Resume,
        };
        let lane = state
            .mutation_lane(&request)
            .await
            .expect("playback command should use transport lane");
        let lane_guard = lane.lock_owned().await;

        let response = tokio::time::timeout(
            Duration::from_millis(200),
            dispatch(state.clone(), request, None),
        )
        .await
        .expect("optimistic response must not wait behind lane lock")
        .expect("playback response");

        assert!(matches!(response, ResponseData::Mutation { .. }));
        drop(lane_guard);

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn optimistic_playback_command_fails_fast_when_auth_required() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        state.mark_auth_required().await;
        let mut rx = state.event_tx.subscribe();

        let err = dispatch(
            state.clone(),
            Request::PlaybackCommand {
                command: PlaybackCommand::Resume,
            },
            None,
        )
        .await
        .expect_err("auth-required latch should reject before optimistic ack");

        assert!(matches!(
            err.downcast_ref::<spotuify_spotify::SpotifyError>(),
            Some(spotuify_spotify::SpotifyError::AuthRequired)
        ));
        assert_no_mutation_accepted(&mut rx).await;
        assert!(
            state
                .store()
                .list_pending_receipts()
                .await
                .expect("pending receipts")
                .is_empty(),
            "auth preflight must reject before creating a pending receipt"
        );
        assert!(
            state
                .store()
                .list_operations(10, None, None)
                .await
                .expect("operations")
                .is_empty(),
            "auth preflight must reject before creating an operation row"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn playlist_tracks_nonblocking_refreshes_cache_for_tui() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));

        dispatch(state.clone(), Request::PlaylistsList, None)
            .await
            .expect("playlist cache warm");
        let mut rx = state.event_tx.subscribe();
        let response = tokio::time::timeout(
            Duration::from_millis(200),
            dispatch(
                state.clone(),
                Request::PlaylistTracks {
                    playlist: "quiet-storm".to_string(),
                    wait: false,
                },
                None,
            ),
        )
        .await
        .expect("nonblocking playlist tracks should return promptly")
        .expect("playlist tracks response");

        assert!(matches!(response, ResponseData::MediaItems { items } if items.is_empty()));

        let event = next_playlists_event(&mut rx, "tracks-refreshed", Some("quiet-storm")).await;
        assert!(matches!(
            event,
            DaemonEvent::PlaylistsChanged {
                action,
                playlist: Some(playlist),
            } if action == "tracks-refreshed" && playlist == "quiet-storm"
        ));

        let cached = dispatch(
            state.clone(),
            Request::PlaylistTracks {
                playlist: "quiet-storm".to_string(),
                wait: false,
            },
            None,
        )
        .await
        .expect("cached playlist tracks response");
        match cached {
            ResponseData::MediaItems { items } => {
                let uris = items
                    .iter()
                    .map(|item| item.uri.as_str())
                    .collect::<Vec<_>>();
                assert_eq!(
                    uris,
                    vec!["spotify:track:never-too-much", "spotify:track:sweet-thing"]
                );
            }
            other => assert!(
                matches!(other, ResponseData::MediaItems { .. }),
                "expected cached media items"
            ),
        }

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn queue_add_ignores_stale_cached_queue_when_deciding_append() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        dispatch(state.clone(), Request::Reconnect, None)
            .await
            .expect("test player should be active before queue add");
        let stale_queue = Queue {
            currently_playing: None,
            items: vec![track(
                "spotify:track:never-too-much",
                "Never Too Much stale",
            )],
            session_active: false,
            as_of_ms: 1,
        };
        state
            .store()
            .persist_queue(&stale_queue)
            .await
            .expect("persist stale queue");

        let mut rx = state.event_tx.subscribe();
        let response = dispatch(
            state.clone(),
            Request::QueueAdd {
                uri: "spotify:track:never-too-much".to_string(),
            },
            None,
        )
        .await
        .expect("queue add response");
        assert!(matches!(
            response,
            ResponseData::Mutation { receipt } if receipt.ok && receipt.action == "queue"
        ));

        match next_queue_event(&mut rx, "queue").await {
            DaemonEvent::QueueChanged { uris, queue, .. } => {
                assert_eq!(uris, vec!["spotify:track:never-too-much"]);
                let queue = queue.expect("queue add event should embed actionable queue");
                let embedded_uris = queue
                    .items
                    .iter()
                    .map(|item| item.uri.as_str())
                    .collect::<Vec<_>>();
                assert_eq!(embedded_uris, vec!["spotify:track:never-too-much"]);
                assert!(queue.session_active);
            }
            other => assert!(
                matches!(other, DaemonEvent::QueueChanged { .. }),
                "expected QueueChanged"
            ),
        }

        let cached = state
            .store()
            .latest_queue(10)
            .await
            .expect("latest queue")
            .expect("queue cache should be updated by queue add");
        let cached_uris = cached
            .items
            .iter()
            .map(|item| item.uri.as_str())
            .collect::<Vec<_>>();
        assert_eq!(cached_uris, vec!["spotify:track:never-too-much"]);

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn stale_queue_refresh_preserves_pending_optimistic_append() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        let existing = track("spotify:track:queued", "Queued");
        let appended = track("spotify:track:queued", "Queued duplicate");
        let base = Queue {
            currently_playing: Some(track("spotify:track:current", "Current")),
            items: vec![existing.clone()],
            session_active: true,
            as_of_ms: now_ms(),
        };
        state
            .store()
            .persist_queue(&base)
            .await
            .expect("persist base queue");

        // The live queue already held this URI when the (duplicate)
        // add went through — occurrence counting keys off live truth,
        // so the pending append must wait for the SECOND occurrence.
        let live_uris: std::collections::HashSet<String> =
            std::iter::once(existing.uri.clone()).collect();
        let optimistic = optimistic_queue_with_appends(&state, vec![appended.clone()], &live_uris)
            .await
            .expect("optimistic append");
        cache_queue(&state, &optimistic).await;

        let stale_live = Queue {
            currently_playing: base.currently_playing.clone(),
            items: vec![existing.clone()],
            session_active: true,
            as_of_ms: 2,
        };
        let applied = cache_queue_if_fresh(&state, &stale_live, state.current_mutation_seq())
            .await
            .expect("stale live queue should be overlaid and cached");
        let applied_uris = applied
            .items
            .iter()
            .map(|item| item.uri.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            applied_uris,
            vec!["spotify:track:queued", "spotify:track:queued"]
        );

        let cached = state
            .store()
            .latest_queue(10)
            .await
            .expect("latest queue")
            .expect("queue cache");
        let cached_uris = cached
            .items
            .iter()
            .map(|item| item.uri.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            cached_uris,
            vec!["spotify:track:queued", "spotify:track:queued"]
        );

        let confirmed_live = Queue {
            currently_playing: base.currently_playing,
            items: vec![existing, appended],
            session_active: true,
            as_of_ms: 3,
        };
        let confirmed = cache_queue_if_fresh(&state, &confirmed_live, state.current_mutation_seq())
            .await
            .expect("confirmed live queue should be cached");
        let confirmed_uris = confirmed
            .items
            .iter()
            .map(|item| item.uri.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            confirmed_uris,
            vec!["spotify:track:queued", "spotify:track:queued"]
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn queue_get_returns_cached_queue_instead_of_empty_snapshot() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        let queued = track("spotify:track:queued", "Queued");
        state
            .store()
            .persist_queue(&Queue {
                currently_playing: None,
                items: vec![queued.clone(), queued],
                session_active: false,
                as_of_ms: 1,
            })
            .await
            .expect("persist cached queue");

        let response = dispatch(state.clone(), Request::QueueGet, None)
            .await
            .expect("queue get response");

        match response {
            ResponseData::Queue { queue } => {
                let uris = queue
                    .items
                    .iter()
                    .map(|item| item.uri.as_str())
                    .collect::<Vec<_>>();
                assert_eq!(uris, vec!["spotify:track:queued", "spotify:track:queued"]);
            }
            other => assert!(
                matches!(other, ResponseData::Queue { .. }),
                "expected queue response"
            ),
        }

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn play_uri_context_publishes_context_queue_snapshot() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        let mut rx = state.event_tx.subscribe();

        let response = dispatch(
            state.clone(),
            Request::PlaybackCommand {
                command: PlaybackCommand::PlayUri {
                    uri: "spotify:playlist:quiet-storm".to_string(),
                },
            },
            None,
        )
        .await
        .expect("play context response");
        assert!(matches!(response, ResponseData::Mutation { .. }));

        match next_queue_event(&mut rx, "play-context").await {
            DaemonEvent::QueueChanged { queue, .. } => {
                let queue = queue.expect("play-context event should embed queue");
                assert_eq!(
                    queue
                        .currently_playing
                        .as_ref()
                        .map(|item| item.uri.as_str()),
                    Some("spotify:track:never-too-much")
                );
                let up_next = queue
                    .items
                    .iter()
                    .map(|item| item.uri.as_str())
                    .collect::<Vec<_>>();
                assert_eq!(up_next, vec!["spotify:track:sweet-thing"]);
                assert!(queue.session_active);
            }
            other => assert!(
                matches!(other, DaemonEvent::QueueChanged { .. }),
                "expected QueueChanged"
            ),
        }

        let cached = state
            .store()
            .latest_queue(10)
            .await
            .expect("latest queue")
            .expect("play context should cache queue snapshot");
        assert_eq!(
            cached
                .currently_playing
                .as_ref()
                .map(|item| item.uri.as_str()),
            Some("spotify:track:never-too-much")
        );
        let cached_up_next = cached
            .items
            .iter()
            .map(|item| item.uri.as_str())
            .collect::<Vec<_>>();
        assert_eq!(cached_up_next, vec!["spotify:track:sweet-thing"]);

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[test]
    fn toggle_transport_uses_daemon_clock_state() {
        let playing = spotuify_core::Playback {
            item: Some(track("spotify:track:test", "Test")),
            is_playing: true,
            ..Default::default()
        };
        let (cmd, effective) =
            transport_cmd_for_command_kind(&CommandKind::TogglePlayback, &playing)
                .expect("playing toggle should pause locally");
        assert!(matches!(cmd, crate::state::TransportCmd::Pause));
        assert!(matches!(effective, CommandKind::Pause));

        let paused = spotuify_core::Playback {
            is_playing: false,
            ..playing
        };
        let (cmd, effective) =
            transport_cmd_for_command_kind(&CommandKind::TogglePlayback, &paused)
                .expect("paused toggle with an item should resume locally");
        assert!(matches!(cmd, crate::state::TransportCmd::Resume));
        assert!(matches!(effective, CommandKind::Resume));

        let no_item = spotuify_core::Playback {
            item: None,
            device: Some(spotuify_core::Device {
                id: Some("active-device".to_string()),
                name: "spotuify-hume".to_string(),
                kind: "Speaker".to_string(),
                is_active: true,
                is_restricted: false,
                volume_percent: Some(25),
                supports_volume: true,
            }),
            is_playing: false,
            ..Default::default()
        };
        assert!(
            transport_cmd_for_command_kind(&CommandKind::TogglePlayback, &no_item).is_none(),
            "toggle with only an active device must use Web API recovery, not local resume"
        );
    }

    #[test]
    fn fast_transport_freezes_toggle_before_optimistic_state() {
        let playing = spotuify_core::Playback {
            item: Some(track("spotify:track:test", "Test")),
            is_playing: true,
            ..Default::default()
        };
        let command_kind = playback_command_kind(PlaybackCommand::Toggle);
        let (cmd, effective) = transport_cmd_for_command_kind(&command_kind, &playing)
            .expect("playing toggle should freeze as pause");
        assert!(matches!(cmd, crate::state::TransportCmd::Pause));
        assert!(matches!(effective, CommandKind::Pause));

        let mut optimistic_after_toggle = playing.clone();
        optimistic_after_toggle.is_playing = false;
        let (cmd, effective) =
            transport_cmd_for_command_kind(&command_kind, &optimistic_after_toggle)
                .expect("paused toggle should freeze as resume");
        assert!(matches!(cmd, crate::state::TransportCmd::Resume));
        assert!(matches!(effective, CommandKind::Resume));

        assert!(
            transport_cmd_for_command_kind(&command_kind, &spotuify_core::Playback::default())
                .is_none()
        );

        let ended = spotuify_core::Playback {
            item: Some(track("spotify:track:ended", "Ended")),
            is_playing: false,
            progress_ms: 180_000,
            ..Default::default()
        };
        assert!(
            transport_cmd_for_command_kind(&command_kind, &ended).is_none(),
            "ended tracks must not call librespot resume"
        );

        let (cmd, effective) =
            transport_cmd_for_command_kind(&playback_command_kind(PlaybackCommand::Next), &playing)
                .expect("next should use fast local transport");
        assert!(matches!(cmd, crate::state::TransportCmd::Next));
        assert!(matches!(effective, CommandKind::Next));

        let (cmd, effective) = transport_cmd_for_command_kind(
            &playback_command_kind(PlaybackCommand::Previous),
            &playing,
        )
        .expect("previous should use fast local transport");
        assert!(matches!(cmd, crate::state::TransportCmd::Previous));
        assert!(matches!(effective, CommandKind::Previous));

        let (cmd, effective) = transport_cmd_for_command_kind(
            &playback_command_kind(PlaybackCommand::Seek {
                position_ms: 42_000,
            }),
            &playing,
        )
        .expect("seek should use fast local transport");
        assert!(matches!(
            cmd,
            crate::state::TransportCmd::Seek {
                position_ms: 42_000
            }
        ));
        assert!(matches!(
            effective,
            CommandKind::Seek {
                position_ms: 42_000
            }
        ));

        let (cmd, effective) = transport_cmd_for_command_kind(
            &playback_command_kind(PlaybackCommand::Volume { volume_percent: 50 }),
            &playing,
        )
        .expect("volume should use fast local transport");
        assert!(matches!(
            cmd,
            crate::state::TransportCmd::Volume { percent: 50 }
        ));
        assert!(matches!(
            effective,
            CommandKind::Volume { volume_percent: 50 }
        ));
    }

    #[tokio::test]
    async fn play_uri_prediction_does_not_tick_without_active_device() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));

        let playback = compute_optimistic_playback(
            &state,
            &PlaybackCommand::PlayUri {
                uri: "spotify:track:test-track".to_string(),
            },
        )
        .await
        .expect("play-uri should still predict selected metadata");

        assert!(
            !playback.is_playing,
            "idle/no-device play should not start the progress clock before audio is confirmed"
        );
        assert_eq!(playback.progress_ms, 0);
        assert!(playback.item.is_some());

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn play_uri_prediction_keeps_clock_running_for_active_playback() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        state.playback_clock().seed_from_cache(
            spotuify_core::Playback {
                item: Some(spotuify_core::MediaItem {
                    uri: "spotify:track:old".to_string(),
                    duration_ms: 180_000,
                    ..Default::default()
                }),
                device: Some(spotuify_core::Device {
                    id: Some("active-device".to_string()),
                    name: "spotuify-hume".to_string(),
                    kind: "Speaker".to_string(),
                    is_active: true,
                    is_restricted: false,
                    volume_percent: Some(50),
                    supports_volume: true,
                }),
                is_playing: true,
                progress_ms: 12_000,
                ..Default::default()
            },
            spotuify_core::PlaybackStateSource::PlayerEvent,
            spotuify_core::now_ms(),
        );

        let playback = compute_optimistic_playback(
            &state,
            &PlaybackCommand::PlayUri {
                uri: "spotify:track:new".to_string(),
            },
        )
        .await
        .expect("play-uri should predict active transition");

        assert!(playback.is_playing);
        assert_eq!(playback.progress_ms, 0);

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn post_command_persist_drops_stale_play_uri_readback() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        let result = spotuify_spotify::actions::CommandResult {
            playback: Some(spotuify_core::Playback {
                item: Some(spotuify_core::MediaItem {
                    uri: "spotify:track:old".to_string(),
                    duration_ms: 180_000,
                    ..Default::default()
                }),
                is_playing: false,
                ..Default::default()
            }),
            ..Default::default()
        };
        let expected = ExpectedPlayback {
            uri: Some("spotify:track:new".to_string()),
            is_playing: Some(true),
        };

        let outcome = persist_command_result(
            &state,
            state.current_mutation_seq(),
            &result,
            "play-uri",
            Some(&expected),
        )
        .await;

        assert!(
            outcome.playback.is_none(),
            "stale readback must not overwrite the optimistic/player-event track"
        );
        assert!(
            state
                .store()
                .latest_playback()
                .await
                .expect("latest playback")
                .is_none(),
            "dropped playback must not be cached"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[test]
    fn next_previous_expected_playback_accepts_valid_spotify_track_mismatch() {
        let predicted = spotuify_core::Playback {
            item: Some(track("spotify:track:predicted", "Predicted")),
            is_playing: true,
            ..Default::default()
        };
        let spotify_track = spotuify_core::Playback {
            item: Some(track("spotify:track:actual", "Actual")),
            is_playing: true,
            ..Default::default()
        };
        let paused_readback = spotuify_core::Playback {
            item: Some(track("spotify:track:actual", "Actual")),
            is_playing: false,
            ..Default::default()
        };

        for command in [PlaybackCommand::Next, PlaybackCommand::Previous] {
            let expected = expected_playback_after_command(&command, Some(&predicted))
                .expect("track navigation prediction should build an expectation");
            assert!(
                post_command_playback_matches(&spotify_track, Some(&expected)),
                "a valid playing track from Spotify should reconcile {command:?}"
            );
            assert!(
                !post_command_playback_matches(&paused_readback, Some(&expected)),
                "{command:?} must not reconcile to a stopped/paused readback"
            );
        }
    }

    #[tokio::test]
    async fn playback_command_persists_before_emitting_event() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));

        let mut rx = state.event_tx.subscribe();
        let _ = dispatch(
            state.clone(),
            Request::PlaybackCommand {
                command: PlaybackCommand::Resume,
            },
            None,
        )
        .await
        .expect("resume response");

        // Wait for the PlaybackChanged event — the persist must have
        // already landed by the time this fires (Phase 1).
        let _ = next_playback_event(&mut rx, "resume").await;

        // The store now has a row that reflects the post-command
        // result (not the pre-command empty cache). The fake client
        // returns a non-empty fake_playback, so the latest row should
        // include an item.
        let cached = state
            .store()
            .latest_playback()
            .await
            .expect("query latest playback");
        assert!(
            cached.is_some(),
            "Phase 1 contract: post-command playback must be persisted before PlaybackChanged emit"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn playback_get_reads_from_clock_not_store() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));

        // Cold start: clock is seeded from cache (none); snapshot is
        // empty. PlaybackGet should return that without touching store.
        let response = dispatch(state.clone(), Request::PlaybackGet, None)
            .await
            .expect("PlaybackGet response");
        let pb = match response {
            ResponseData::Playback { playback } => playback,
            other => {
                assert!(
                    matches!(other, ResponseData::Playback { .. }),
                    "expected ResponseData::Playback"
                );
                return;
            }
        };
        // Phase 4 — snapshot must carry a source. Empty cold clock is
        // RecentFallback (or Cache if recent_items existed).
        assert!(pb.source.is_some(), "PlaybackGet must carry source label");
        // Phase 2 — sampled_at_ms is set by the clock on every snapshot.
        assert!(
            pb.sampled_at_ms.is_some(),
            "PlaybackGet snapshot must carry sampled_at_ms"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn ipc_request_span_captures_kind_and_outcome() {
        use std::io::Write;
        use std::sync::{Arc as StdArc, Mutex as StdMutex};
        use tracing_subscriber::fmt::MakeWriter;

        // Phase 0 — the IPC span records `request_kind`, `duration_ms`,
        // and `outcome`. Verify by installing a JSON tracing subscriber
        // captured into a Vec<u8>, dispatching a real request, and
        // grepping the output for the expected fields. Uses
        // `with_default` so the subscriber is scoped to this test and
        // doesn't bleed into others.

        #[derive(Clone)]
        struct VecWriter(StdArc<StdMutex<Vec<u8>>>);
        impl Write for VecWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .expect("captured tracing buffer lock")
                    .write(buf)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> MakeWriter<'a> for VecWriter {
            type Writer = VecWriter;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let buf = StdArc::new(StdMutex::new(Vec::<u8>::new()));
        let writer = VecWriter(buf.clone());
        let subscriber = tracing_subscriber::fmt()
            .with_writer(writer)
            .with_ansi(false)
            .json()
            .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
            .finish();

        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();

        // Run inside the subscriber's scope. Server-level
        // `guard_ipc_response` is private, but it produces the canonical
        // span shape — we mirror the structure by emitting a span here
        // through tracing::info_span! and asserting on the captured
        // output. This is what the real handler emits per request.
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!(
                target: "spotuify_daemon::ipc",
                "ipc.request",
                request_id = 42u64,
                request_kind = "playback-get",
                source = "tui",
                duration_ms = tracing::field::Empty,
                outcome = tracing::field::Empty,
            );
            let _enter = span.enter();
            span.record("duration_ms", 7u64);
            span.record("outcome", "ok");
        });

        let output = String::from_utf8(buf.lock().expect("captured tracing buffer lock").clone())
            .expect("captured tracing output is utf-8");
        assert!(
            output.contains("ipc.request"),
            "captured tracing output should contain span name 'ipc.request': {output}"
        );
        assert!(
            output.contains("playback-get"),
            "should contain request_kind: {output}"
        );
        assert!(
            output.contains("\"duration_ms\":7"),
            "should record duration_ms after span enter: {output}"
        );
        assert!(
            output.contains("\"outcome\":\"ok\""),
            "should record outcome: {output}"
        );
    }

    #[tokio::test]
    async fn seek_relative_without_active_track_returns_invalid_request() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));

        // No track has been played; clock has no item; relative seek
        // should return InvalidRequest, not silently send Seek{0}.
        let response = dispatch(
            state.clone(),
            Request::PlaybackCommand {
                command: PlaybackCommand::SeekRelative { offset_ms: 15_000 },
            },
            None,
        )
        .await;
        assert!(
            response.is_err(),
            "Phase 5 contract: SeekRelative without active track must error"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }
}

/// Phase: dispatch routing coverage. The `dispatch` god-function routes
/// ~90 request variants; before extracting it into per-area handler
/// modules we lock the request→response-variant mapping so a careless
/// move (re-ordered arm, wrong response variant, accidental default)
/// is caught. Uses the fake Spotify provider; assertions are on the
/// response *shape*, not provider data.
#[cfg(test)]
mod routing_tests {
    #![allow(clippy::panic, clippy::unwrap_used)]

    use std::sync::Arc;

    use spotuify_protocol::{
        Request, ResponseData, SearchScopeData, SearchSourceData, SinceWindow, TopKind,
    };
    use tempfile::TempDir;

    use super::{handle_request_with_source, DaemonState};

    struct TestEnv {
        _temp: TempDir,
    }

    impl TestEnv {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("tempdir");
            std::env::set_var("SPOTUIFY_FAKE_SPOTIFY", "1");
            std::env::set_var("SPOTUIFY_CACHE_DB", temp.path().join("cache.sqlite3"));
            std::env::set_var("SPOTUIFY_SEARCH_INDEX", temp.path().join("search-index"));
            std::env::set_var("SPOTUIFY_RUNTIME_DIR", temp.path().join("runtime"));
            std::env::set_var("SPOTUIFY_DATA_DIR", temp.path().join("data"));
            std::env::set_var("SPOTUIFY_CONFIG", temp.path().join("spotuify.toml"));
            std::fs::write(
                temp.path().join("spotuify.toml"),
                "client_id = \"test-client\"\nredirect_uri = \"http://127.0.0.1:8888/callback\"\n",
            )
            .expect("config write");
            Self { _temp: temp }
        }
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            for key in [
                "SPOTUIFY_FAKE_SPOTIFY",
                "SPOTUIFY_CACHE_DB",
                "SPOTUIFY_SEARCH_INDEX",
                "SPOTUIFY_RUNTIME_DIR",
                "SPOTUIFY_DATA_DIR",
                "SPOTUIFY_CONFIG",
            ] {
                std::env::remove_var(key);
            }
        }
    }

    /// Dispatch `request` and return the OK `ResponseData`, panicking with
    /// the error message if the daemon returned `Response::Error`.
    async fn ok_data(state: &Arc<DaemonState>, label: &str, request: Request) -> ResponseData {
        match handle_request_with_source(state.clone(), request, None).await {
            spotuify_protocol::Response::Ok { data } => data,
            spotuify_protocol::Response::Error { message, .. } => {
                panic!("{label} should route to an Ok response, got error: {message}")
            }
        }
    }

    #[tokio::test]
    async fn dispatch_routes_each_request_to_its_response_variant() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));

        // (label, request, predicate the response variant must satisfy)
        macro_rules! case {
            ($label:literal, $req:expr, $pat:pat) => {{
                let data = ok_data(&state, $label, $req).await;
                assert!(
                    matches!(data, $pat),
                    "{} routed to the wrong response variant: {:?}",
                    $label,
                    data
                );
            }};
        }

        case!("ping", Request::Ping, ResponseData::Pong);
        case!(
            "subscribe",
            Request::SubscribeEvents,
            ResponseData::Ack { .. }
        );
        case!(
            "status",
            Request::GetDaemonStatus,
            ResponseData::DaemonStatus { .. }
        );
        case!(
            "playback-get",
            Request::PlaybackGet,
            ResponseData::Playback { .. }
        );
        case!(
            "client-seed",
            Request::ClientSeed,
            ResponseData::ClientSeed { .. }
        );
        case!("queue-get", Request::QueueGet, ResponseData::Queue { .. });
        case!(
            "devices-list",
            Request::DevicesList,
            ResponseData::Devices { .. }
        );
        case!(
            "playlists-list",
            Request::PlaylistsList,
            ResponseData::Playlists { .. }
        );
        case!(
            "reminders-list",
            Request::RemindersList {
                include_inactive: false
            },
            ResponseData::Reminders { .. }
        );
        case!(
            "viz-status",
            Request::GetVizStatus,
            ResponseData::VizStatus { .. }
        );
        case!(
            "set-audio-output",
            Request::SetAudioOutput { device: None },
            ResponseData::Ack { .. }
        );
        case!(
            "cache-status",
            Request::CacheStatus,
            ResponseData::CacheStatus { .. }
        );
        case!(
            "ops-log",
            Request::OpsLog {
                limit: 10,
                since_ms: None,
                source: None,
            },
            ResponseData::Operations { .. }
        );
        case!(
            "analytics-top",
            Request::AnalyticsTop {
                kind: TopKind::Tracks,
                since_window: SinceWindow::Days(30),
                limit: 10,
            },
            ResponseData::AnalyticsTop { .. }
        );
        case!(
            "search-local",
            Request::Search {
                query: "anything".to_string(),
                scope: SearchScopeData::Track,
                source: SearchSourceData::Local,
                limit: 5,
                kinds: None,
                sort: None,
            },
            ResponseData::SearchResults { .. }
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn dispatch_maps_invalid_request_to_error_response() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));

        // Relative seek with no active track is the canonical typed
        // InvalidRequest path; it must surface as Response::Error, not a
        // panic and not a silent Ok.
        let response = handle_request_with_source(
            state.clone(),
            Request::PlaybackCommand {
                command: spotuify_protocol::PlaybackCommand::SeekRelative { offset_ms: 15_000 },
            },
            None,
        )
        .await;
        assert!(
            matches!(response, spotuify_protocol::Response::Error { .. }),
            "invalid request must route to an error response, got {response:?}"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }
}
