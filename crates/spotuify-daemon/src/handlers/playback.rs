//! `playback` request handlers (split out of the dispatch god-function).

use std::sync::Arc;

use spotuify_protocol::{
    DaemonEvent, OperationKind, OperationSource, PlaybackCommand, Request, ResponseData,
};
use spotuify_spotify::actions::{self, CommandKind};
use spotuify_spotify::client::MediaItem;
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
        Request::PlaybackGet => {
            // Phase 2 — sub-millisecond `PlaybackClock` snapshot. No
            // SQLite read on the hot path: the clock is in-memory and
            // extrapolates current progress against a monotonic baseline.
            // The Spotify Web API call NEVER runs inline; it always runs
            // in `spawn_playback_refresh` so auth file IO +
            // token refresh + HTTP round-trip can't make the first
            // PlaybackGet take a full minute the way it used to.
            let playback = state.snapshot_playback();
            state.viz_coordinator().set_playing(playback.is_playing);
            spawn_playback_refresh(state.clone());
            Ok(ResponseData::Playback { playback })
        }
        Request::PlaybackCommand { command } => {
            // Phase 5 — resolve `SeekRelative` to an absolute `Seek` against
            // the daemon `PlaybackClock` BEFORE any other dispatch logic.
            // Doing it here means the rest of the pipeline only ever sees
            // absolute seeks. If there's no active track, return a typed
            // InvalidRequest error so scripts get a clear failure.
            let command = match command {
                PlaybackCommand::SeekRelative { offset_ms } => {
                    let snapshot = state.snapshot_playback();
                    let Some(item) = snapshot.item.clone() else {
                        anyhow::bail!("invalid request: seek requires an active track");
                    };
                    let current = snapshot.progress_ms as i64;
                    let target = current.saturating_add(offset_ms).max(0) as u64;
                    let clamped = if item.duration_ms > 0 {
                        target.min(item.duration_ms)
                    } else {
                        target
                    };
                    tracing::debug!(
                        target: "spotuify_daemon::seek",
                        offset_ms,
                        current_ms = current,
                        target_ms = clamped,
                        track_uri = %item.uri,
                        "resolved relative seek from clock"
                    );
                    PlaybackCommand::Seek {
                        position_ms: clamped,
                    }
                }
                other => other,
            };
            let action = playback_command_action(&command);
            let op_kind = playback_command_operation_kind(&command);
            let viz_playing = playback_command_viz_state(&command);
            let state_for = state.clone();
            let pre_command_playback = state.snapshot_playback();
            let command_kind = playback_command_kind(command.clone());
            let fast_transport =
                transport_cmd_for_command_kind(&command_kind, &pre_command_playback);
            let context_queue_uri = match &command_kind {
                CommandKind::PlayUri { uri } => Some(uri.clone()),
                CommandKind::PlayItem { item } => Some(item.uri.clone()),
                _ => None,
            };
            // Tell the session tracker which context the next started track
            // plays from (for playlist-level analytics). Only set on an
            // explicit play so pause/next keep the existing context.
            if let Some(context) = &context_queue_uri {
                state.set_playback_context(Some(context.clone()));
            }
            reject_if_auth_blocked(&state)?;
            // Bump the mutation seq BEFORE the Spotify call so any
            // background poll-in-flight (sync_loop, spawn_*_refresh)
            // sees a newer seq and discards its stale pre-mutation
            // snapshot instead of overwriting the optimistic local
            // cache. See `DaemonState::mutation_seq`.
            state.bump_mutation_seq();
            // Update viz state optimistically — the user expects the
            // visualiser to react the moment they hit Pause, not after
            // Spotify ACKs.
            if let Some(playing) = viz_playing {
                state.viz_coordinator().set_playing(playing);
            }
            // Optimistic playback emit — the daemon owns the snapshot
            // every subscriber renders. Predict the post-command state,
            // apply it to the clock, and broadcast `PlaybackChanged`
            // BEFORE the spawned closure dispatches to Spotify. Every
            // client (TUI player widget, CLI `--watch`, MCP) reacts at
            // first render tick instead of waiting 300-1500ms for
            // Spotify to round-trip. The clock's source-priority
            // ranking lets the eventual authoritative
            // `CommandResult` event overwrite us cleanly.
            let predicted = compute_optimistic_playback(&state, &command).await;
            let expected_playback = expected_playback_after_command(&command, predicted.as_ref());
            if let Some(predicted) = predicted.as_ref() {
                state
                    .playback_clock()
                    .apply_command_result(predicted, spotuify_core::now_ms());
                state.emit_event(DaemonEvent::PlaybackChanged {
                    action: format!("optimistic-{action}"),
                    playback: Some(state.snapshot_playback()),
                });
                // Optimistic queue emit — same ownership rule as playback:
                // predict the post-`next` queue (cached queue with the
                // predicted track promoted to now-playing) and broadcast
                // `QueueChanged` in the same tick, so the queue rail and
                // lyrics context move with the track title instead of
                // waiting for the post-command refresh. Caching the
                // prediction also lets a rapid second `next` chain off it.
                if matches!(command, PlaybackCommand::Next) {
                    if let Some(next_item) = predicted.item.as_ref() {
                        if let Some(queue) = optimistic_queue_after_next(&state, next_item).await {
                            cache_queue(&state, &queue).await;
                            state.emit_event(DaemonEvent::QueueChanged {
                                action: format!("optimistic-{action}"),
                                uris: Vec::new(),
                                queue: Some(queue),
                            });
                        }
                    }
                }
            }
            let fast_transport_result =
                if let Some((cmd, effective_command)) = fast_transport.as_ref() {
                    apply_fast_transport(&state, cmd.clone(), effective_command, action).await
                } else {
                    None
                };
            let background_command_kind = fast_transport
                .map(|(_, effective_command)| effective_command)
                .unwrap_or(command_kind);
            spawn_optimistic_mutation(
                &state,
                op_kind,
                operation_source,
                vec![],
                action,
                request_json.clone(),
                Some(spotuify_protocol::PreState::Transport),
                Some(spotuify_protocol::ReversalPlan::NotReversible {
                    reason: "transport".to_string(),
                }),
                mutation_lane,
                move |_op_id| async move {
                    // Capture seq INSIDE the closure so we measure against
                    // the mutation that just fired (the bump happened
                    // before `spawn_optimistic_mutation`). A second mutation
                    // that arrives while this awaits will advance the seq
                    // and `persist_command_result` will drop us.
                    let captured_seq = state_for.current_mutation_seq();
                    let result = if let Some(result) = fast_transport_result {
                        result
                    } else {
                        // Belt-and-suspenders: catch a latch flip between the
                        // sync pre-check and the body's spotify_client() call.
                        if let Some(err) = state_for.auth_gate_error() {
                            return Err(anyhow::Error::new(err));
                        }
                        if let Some(result) =
                            try_embedded_transport(&state_for, &background_command_kind).await
                        {
                            result
                        } else {
                            let mut client = state_for.spotify_client().await?;
                            execute_with_device_recovery(
                                &state_for,
                                &mut client,
                                background_command_kind,
                            )
                            .await?
                        }
                    };
                    // Phase 1: persist BEFORE the event so subscribers
                    // that fetch on the event see fresh state.
                    let outcome = persist_command_result(
                        &state_for,
                        captured_seq,
                        &result,
                        action,
                        expected_playback.as_ref(),
                    )
                    .await;
                    tracing::debug!(
                        target: "spotuify_daemon::post_command",
                        action,
                        captured_seq,
                        persisted_playback = outcome.playback.is_some(),
                        persisted_queue = outcome.queue_items.is_some(),
                        persisted_devices = outcome.devices.is_some(),
                        post_uri = outcome
                            .playback
                            .as_ref()
                            .and_then(|p| p.uri.as_deref())
                            .unwrap_or(""),
                        post_is_playing = outcome
                            .playback
                            .as_ref()
                            .map(|p| p.is_playing)
                            .unwrap_or_default(),
                        "post-command persist"
                    );
                    let message = result.message.clone().unwrap_or_else(|| action.to_string());
                    // Phase 3 — embed the freshly-computed clock snapshot
                    // (which has just been updated by `persist_command_result`)
                    // so TUI/MCP subscribers apply the new state without a
                    // follow-up `PlaybackGet` round-trip.
                    state_for.emit_event(DaemonEvent::PlaybackChanged {
                        action: action.to_string(),
                        playback: Some(state_for.snapshot_playback()),
                    });
                    if outcome.queue_items.is_some() {
                        let queue_snapshot = result.queue.clone();
                        state_for.emit_event(DaemonEvent::QueueChanged {
                            action: action.to_string(),
                            uris: Vec::new(),
                            queue: queue_snapshot,
                        });
                    }
                    if let Some(uri) = context_queue_uri.as_deref() {
                        if let Some(queue) =
                            context_queue_snapshot_for_play_uri(&state_for, uri).await
                        {
                            let uris = queue
                                .items
                                .iter()
                                .map(|item| item.uri.clone())
                                .collect::<Vec<_>>();
                            cache_queue(&state_for, &queue).await;
                            state_for.warm_queue_uris(uris.clone());
                            state_for.emit_event(DaemonEvent::QueueChanged {
                                action: "play-context".to_string(),
                                uris,
                                queue: Some(queue),
                            });
                        }
                    }
                    if outcome.devices.is_some() {
                        state_for.emit_event(DaemonEvent::DevicesChanged {
                            action: action.to_string(),
                            devices: result.devices.clone(),
                        });
                    }
                    if result.request_refresh && outcome.playback.is_none() {
                        spawn_playback_refresh(state_for.clone());
                    }
                    // Track-changing transport rarely carries queue items in
                    // its result, so the optimistic queue emit above would
                    // never be reconciled. Fetch the authoritative queue
                    // after a short delay (immediate fetches still see the
                    // pre-skip queue upstream).
                    if outcome.queue_items.is_none() && matches!(action, "next" | "previous") {
                        spawn_queue_refresh_delayed(state_for.clone(), 1200);
                    }
                    emit_mutation_finished(&state_for, action, &message);
                    Ok(())
                },
            )
            .await
        }
        Request::DevicesList => {
            // Never block on Spotify; serve whatever's cached and let
            // the spawned refresh broadcast `DevicesChanged` when fresh
            // data arrives.
            let mut devices = state.store().list_devices().await?;
            if let Some(own_device) = state.connected_own_device().await {
                let own_id = own_device.id.as_deref();
                if !devices.iter().any(|device| device.id.as_deref() == own_id) {
                    devices.push(own_device);
                }
            }
            spawn_devices_refresh(state.clone());
            Ok(ResponseData::Devices { devices })
        }
        Request::DeviceTransfer { device } => {
            let state_for = state.clone();
            // DeviceTransfer mutates the active device which the
            // playback poll keys off of; bump seq so a polling refresh
            // that started before this call can't repopulate the
            // pre-transfer device.
            state.bump_mutation_seq();
            spawn_optimistic_mutation(
                &state,
                OperationKind::Transfer,
                operation_source,
                vec![],
                "transfer",
                request_json.clone(),
                None,
                None,
                mutation_lane,
                move |op_id| async move {
                    let mut client = state_for.spotify_client().await?;
                    let cached_devices = cached_devices_with_own_device(&state_for).await?;
                    let target_device = match selection::resolve_device(&cached_devices, &device) {
                        Ok(device) => device,
                        Err(_) => {
                            let devices = actions::devices(&mut client).await?;
                            selection::resolve_device(&devices, &device)?
                        }
                    };
                    let playback = state_for.snapshot_playback();
                    let play = playback.is_playing;
                    let prior_device_id = playback.device.as_ref().and_then(|d| d.id.clone());
                    let pre_state = spotuify_protocol::PreState::Transfer {
                        prior_device_id: prior_device_id.clone(),
                    };
                    let plan = match prior_device_id.clone() {
                        Some(id) => {
                            spotuify_protocol::ReversalPlan::TransferToPriorDevice { device_id: id }
                        }
                        None => spotuify_protocol::ReversalPlan::NotReversible {
                            reason: "no prior active device to restore".to_string(),
                        },
                    };
                    if let Err(err) = state_for
                        .store()
                        .update_operation_plan(op_id, Some(&pre_state), Some(&plan))
                        .await
                    {
                        tracing::warn!(error = %err, "failed to persist transfer pre-state");
                    }
                    let captured_seq = state_for.current_mutation_seq();
                    let device_name = target_device.name.clone();
                    let device_id = target_device.id.clone();
                    let result = match actions::execute(
                        &mut client,
                        CommandKind::Transfer {
                            device: target_device,
                            play,
                        },
                    )
                    .await
                    {
                        Ok(result) => result,
                        // Spotify lists Alexa/Echo speakers but routinely
                        // 404s Web-API transfers to them — they're
                        // Alexa-controlled and must be started there first.
                        // Surface that instead of the raw "404 Not found".
                        Err(err) if is_no_active_device_error(&err) => {
                            let is_echo =
                                device_id.as_deref().is_some_and(|id| id.contains("_amzn_"));
                            let hint = if is_echo {
                                format!(
                                    "\"{device_name}\" can't be started from here — Alexa/Echo \
                                     speakers must be started in Alexa (or the Spotify app) first, \
                                     then transfer."
                                )
                            } else {
                                format!(
                                    "\"{device_name}\" is offline or unavailable to Spotify \
                                     right now."
                                )
                            };
                            return Err(anyhow::anyhow!(hint));
                        }
                        Err(err) => return Err(err.into()),
                    };
                    state_for.viz_coordinator().set_playing(play);
                    // User-driven hand-off: mark whether this device remains the
                    // intended target so a later session drop doesn't auto-
                    // reconnect and steal playback from the device we just
                    // transferred to.
                    let to_own_device = state_for
                        .own_device_id()
                        .is_some_and(|own| device_id.as_deref() == Some(own.as_str()));
                    state_for.set_we_are_active(to_own_device);
                    // Phase 1: capture any playback/devices snapshot the
                    // Transfer ACK returned so subscribers don't need a
                    // re-fetch round-trip.
                    let _outcome =
                        persist_command_result(&state_for, captured_seq, &result, "transfer", None)
                            .await;
                    let message = result
                        .message
                        .clone()
                        .unwrap_or_else(|| "transfer".to_string());
                    state_for.emit_event(DaemonEvent::DevicesChanged {
                        action: "transfer".to_string(),
                        devices: result.devices.clone(),
                    });
                    state_for.emit_event(DaemonEvent::PlaybackChanged {
                        action: "transfer".to_string(),
                        playback: Some(state_for.snapshot_playback()),
                    });
                    emit_mutation_finished(&state_for, "transfer", &message);
                    Ok(())
                },
            )
            .await
        }
        Request::RecentlyPlayed => {
            // Non-blocking: empty list is fine on cold start. Refresh
            // populates the cache and subscribers re-fetch when they
            // see SyncFinished or the next PlaybackChanged.
            let items = state.store().list_recent_items(20).await?;
            spawn_recent_refresh(state.clone());
            Ok(ResponseData::MediaItems { items })
        }
        Request::QueueGet => {
            // Non-blocking: return the last known queue immediately,
            // then refresh in the background. Returning default here
            // makes clients briefly clear their visible queue on every
            // fallback read/reseed.
            let queue = state.store().latest_queue(500).await?.unwrap_or_default();
            state.warm_queue(&queue);
            spawn_queue_refresh(state.clone());
            Ok(ResponseData::Queue { queue })
        }
        Request::QueueAdd { uri } => {
            let state_for_event = state.clone();
            let pre_state = Some(spotuify_protocol::PreState::QueueAdd { uri: uri.clone() });
            // Queue adds have no executable inverse: neither the Web API
            // nor librespot 0.8 exposes queue-remove. Record that honestly
            // so `ops undo` never selects (or pretends to reverse) this op.
            let plan = Some(spotuify_protocol::ReversalPlan::NotReversible {
                reason: "Spotify has no queue-remove endpoint".to_string(),
            });
            // QueueAdd mutates the upcoming-queue list. Bump the seq
            // so a queue poll already in flight can't repopulate the
            // pre-add ordering.
            state.bump_mutation_seq();
            spawn_optimistic_mutation(
                &state,
                OperationKind::QueueAdd,
                operation_source,
                vec![uri.clone()],
                "queue",
                request_json.clone(),
                pre_state,
                plan,
                mutation_lane,
                move |_op_id| async move {
                    let mut client = state_for_event.spotify_client().await?;
                    let resolved_items =
                        queueable_items_for_selection(&state_for_event, &mut client, &uri).await?;
                    // The queue is a set: a track appears at most once.
                    // Dedup against the LIVE queue only — never the
                    // persisted snapshot, which is historical by design
                    // and may describe a dead Spotify session. Spotify
                    // has no queue-move, so an existing entry stays put
                    // rather than moving up; a failed live fetch
                    // degrades to no dedup.
                    let already_queued = live_queue_uris(&mut client).await;
                    let (queued_items, skipped_dupes) =
                        dedup_queue_items(resolved_items, &already_queued);
                    if queued_items.is_empty() && skipped_dupes > 0 {
                        emit_mutation_finished(
                            &state_for_event,
                            "queue",
                            &format!("already queued, skipped {skipped_dupes} item(s)"),
                        );
                        return Ok(());
                    }
                    let queue_uris: Vec<String> =
                        queued_items.iter().map(|item| item.uri.clone()).collect();
                    let selection_kind = selection::media_kind_from_uri(&uri)?;
                    let idle_context_label = idle_context_start_label(&selection_kind);

                    // "Queue" needs an active Spotify session. librespot
                    // 0.8.0 can't originate add-to-queue (embedded
                    // `queue_add` returns Unsupported), so we fall back to
                    // the Web API `POST /me/player/queue`, which 404s with
                    // NO_ACTIVE_DEVICE when nothing is playing. When Spotify
                    // itself reports no active device for the first item, the
                    // session is idle: start embedded playback. Track/episode
                    // selections play the first item then queue the rest;
                    // album/playlist selections start their context so the
                    // backend owns natural progression. Keying off Spotify's
                    // actual error (not the local clock) means a remote
                    // session the daemon hasn't polled yet is never hijacked.
                    let mut played_first = false;
                    let mut played_context = false;
                    // The first item reveals whether a session exists: queue
                    // it, and if Spotify reports no active device, start one by
                    // playing it on the embedded device instead.
                    if let Some(first) = queue_uris.first().cloned() {
                        match queue_one(state_for_event.as_ref(), &mut client, &first).await? {
                            QueueAttempt::Queued => {}
                            QueueAttempt::NoActiveDevice => {
                                let start_uri = if idle_context_label.is_some() {
                                    played_context = true;
                                    uri.clone()
                                } else {
                                    first.clone()
                                };
                                state_for_event
                                    .transport(crate::state::TransportCmd::PlayUri {
                                        uri: start_uri.clone(),
                                        position_ms: 0,
                                    })
                                    .await
                                    .map_err(|err| {
                                        anyhow::anyhow!(
                                            "failed to start playback for {start_uri}: {err}"
                                        )
                                    })?;
                                played_first = true;
                            }
                        }
                    }

                    if !played_context {
                        for queue_uri in queue_uris.iter().skip(1) {
                            // After an idle auto-play, the new session takes a
                            // beat to register with Spotify, so retry briefly
                            // instead of racing it with another 404.
                            let mut attempt = 0u32;
                            loop {
                                match queue_one(state_for_event.as_ref(), &mut client, queue_uri)
                                    .await?
                                {
                                    QueueAttempt::Queued => break,
                                    QueueAttempt::NoActiveDevice if played_first && attempt < 6 => {
                                        attempt += 1;
                                        tokio::time::sleep(std::time::Duration::from_millis(500))
                                            .await;
                                    }
                                    QueueAttempt::NoActiveDevice => {
                                        return Err(anyhow::anyhow!(
                                            "queue add for {queue_uri} failed: no active device"
                                        ));
                                    }
                                }
                            }
                        }
                    }

                    // What actually landed in the queue: the first item too,
                    // unless we auto-played it to start the session.
                    let queued_uris: Vec<String> = if played_context {
                        Vec::new()
                    } else if played_first {
                        queue_uris.iter().skip(1).cloned().collect()
                    } else {
                        queue_uris.clone()
                    };
                    let appended_items: Vec<MediaItem> = if played_context {
                        Vec::new()
                    } else if played_first {
                        queued_items.iter().skip(1).cloned().collect()
                    } else {
                        queued_items.clone()
                    };
                    let queue_snapshot =
                        optimistic_queue_with_appends(&state_for_event, appended_items).await;
                    if let Some(queue) = queue_snapshot.as_ref() {
                        cache_queue(&state_for_event, queue).await;
                    }
                    let skip_note = if skipped_dupes > 0 {
                        format!(", skipped {skipped_dupes} already queued")
                    } else {
                        String::new()
                    };
                    let message = if played_first && queued_uris.is_empty() {
                        if let Some(label) = idle_context_label {
                            format!("playing {label} now")
                        } else {
                            "playing now".to_string()
                        }
                    } else if played_first {
                        format!(
                            "playing now, queued {} item(s){skip_note}",
                            queued_uris.len()
                        )
                    } else {
                        format!("queued {} item(s){skip_note}", queue_uris.len())
                    };
                    state_for_event.emit_event(DaemonEvent::QueueChanged {
                        action: "queue".to_string(),
                        uris: queued_uris.clone(),
                        queue: queue_snapshot,
                    });
                    state_for_event.warm_queue_uris(queued_uris.clone());
                    spawn_queue_refresh(state_for_event.clone());
                    emit_mutation_finished(&state_for_event, "queue", &message);
                    Ok(())
                },
            )
            .await
        }
        Request::QueueAddMany { uris } => {
            // "Queue all" — append a whole batch (e.g. every liked song).
            // Spotify's queue endpoint is single-URI, so we loop internally
            // and emit one aggregate receipt. Not reversible: Spotify has
            // no queue-remove, so the plan says so explicitly.
            let state_for_event = state.clone();
            let subject = uris.clone();
            state.bump_mutation_seq();
            spawn_optimistic_mutation(
                &state,
                OperationKind::QueueAdd,
                operation_source,
                subject,
                "queue",
                request_json.clone(),
                None,
                Some(spotuify_protocol::ReversalPlan::NotReversible {
                    reason: "Spotify has no queue-remove endpoint".to_string(),
                }),
                mutation_lane,
                move |_op_id| async move {
                    let mut client = state_for_event.spotify_client().await?;
                    // Expand each selection (tracks pass through; album/playlist
                    // URIs expand to their tracks).
                    let mut resolved_items: Vec<MediaItem> = Vec::new();
                    for selection in &uris {
                        let resolved =
                            queueable_items_for_selection(&state_for_event, &mut client, selection)
                                .await?;
                        resolved_items.extend(resolved);
                    }
                    // Queue set semantics — same rule as Request::QueueAdd
                    // above: dedup against the live queue and within the
                    // batch itself.
                    let already_queued = live_queue_uris(&mut client).await;
                    let (queued_items, skipped_dupes) =
                        dedup_queue_items(resolved_items, &already_queued);
                    let queue_uris: Vec<String> =
                        queued_items.iter().map(|item| item.uri.clone()).collect();
                    if queue_uris.is_empty() {
                        let message = if skipped_dupes > 0 {
                            format!("already queued, skipped {skipped_dupes} item(s)")
                        } else {
                            "nothing to queue".to_string()
                        };
                        emit_mutation_finished(&state_for_event, "queue", &message);
                        return Ok(());
                    }
                    let mut played_first = false;
                    if let Some(first) = queue_uris.first().cloned() {
                        match queue_one(state_for_event.as_ref(), &mut client, &first).await? {
                            QueueAttempt::Queued => {}
                            QueueAttempt::NoActiveDevice => {
                                state_for_event
                                    .transport(crate::state::TransportCmd::PlayUri {
                                        uri: first.clone(),
                                        position_ms: 0,
                                    })
                                    .await
                                    .map_err(|err| {
                                        anyhow::anyhow!(
                                            "failed to start playback for {first}: {err}"
                                        )
                                    })?;
                                played_first = true;
                            }
                        }
                    }
                    for queue_uri in queue_uris.iter().skip(1) {
                        let mut attempt = 0u32;
                        loop {
                            match queue_one(state_for_event.as_ref(), &mut client, queue_uri)
                                .await?
                            {
                                QueueAttempt::Queued => break,
                                QueueAttempt::NoActiveDevice if played_first && attempt < 6 => {
                                    attempt += 1;
                                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                                }
                                QueueAttempt::NoActiveDevice => {
                                    return Err(anyhow::anyhow!(
                                        "queue add for {queue_uri} failed: no active device"
                                    ));
                                }
                            }
                        }
                    }
                    let queued_uris: Vec<String> = if played_first {
                        queue_uris.iter().skip(1).cloned().collect()
                    } else {
                        queue_uris.clone()
                    };
                    let appended_items: Vec<MediaItem> = if played_first {
                        queued_items.iter().skip(1).cloned().collect()
                    } else {
                        queued_items.clone()
                    };
                    let queue_snapshot =
                        optimistic_queue_with_appends(&state_for_event, appended_items).await;
                    if let Some(queue) = queue_snapshot.as_ref() {
                        cache_queue(&state_for_event, queue).await;
                    }
                    let skip_note = if skipped_dupes > 0 {
                        format!(", skipped {skipped_dupes} already queued")
                    } else {
                        String::new()
                    };
                    let message = if played_first {
                        format!(
                            "playing now, queued {} item(s){skip_note}",
                            queued_uris.len()
                        )
                    } else {
                        format!("queued {} item(s){skip_note}", queue_uris.len())
                    };
                    state_for_event.emit_event(DaemonEvent::QueueChanged {
                        action: "queue".to_string(),
                        uris: queued_uris.clone(),
                        queue: queue_snapshot,
                    });
                    state_for_event.warm_queue_uris(queued_uris.clone());
                    spawn_queue_refresh(state_for_event.clone());
                    emit_mutation_finished(&state_for_event, "queue", &message);
                    Ok(())
                },
            )
            .await
        }
        Request::Reconnect => {
            tracing::info!("daemon reconnect requested");
            let device_name = DaemonState::configured_device_name();
            state.reconnect_player(&device_name).await?;
            state.emit_event(DaemonEvent::ConfigReloaded);
            Ok(ResponseData::Ack {
                message: "player backend reconnected".to_string(),
            })
        }
        Request::SetAudioOutput { device } => {
            // Live output rebind: snapshot what was playing, swap the
            // backend's device selection, rebuild the Spirc + sink via
            // the normal reconnect path, then put the interrupted track
            // back where it was. No daemon restart.
            tracing::info!(device = ?device, "audio output rebind requested");
            let snapshot = state.snapshot_playback();
            state.set_player_audio_output(device.clone()).await?;
            let device_name = DaemonState::configured_device_name();
            state.reconnect_player(&device_name).await?;
            let mut message = match device.as_deref() {
                Some(name) => format!("audio output set to \"{name}\""),
                None => "audio output reset to the system default".to_string(),
            };
            if snapshot.is_playing {
                if let Some(item) = snapshot.item.as_ref() {
                    let position_ms = u32::try_from(snapshot.progress_ms).unwrap_or(0);
                    match state
                        .transport(crate::state::TransportCmd::PlayUri {
                            uri: item.uri.clone(),
                            position_ms,
                        })
                        .await
                    {
                        Ok(()) => {
                            message.push_str("; playback resumed");
                            spawn_playback_refresh(state.clone());
                        }
                        Err(err) => {
                            tracing::warn!(
                                error = %err,
                                "playback restore after audio output rebind failed"
                            );
                            message.push_str("; playback could not be resumed automatically");
                        }
                    }
                }
            }
            state.emit_event(DaemonEvent::ConfigReloaded);
            Ok(ResponseData::Ack { message })
        }
        _ => unreachable!("non-playback request routed to playback dispatcher"),
    }
}

/// Fetch the live queue's URIs for add-time dedup. Only a queue Spotify
/// reports as belonging to an ACTIVE session counts; a cached fallback
/// snapshot must not veto adds (it may describe a dead session). A
/// failed fetch degrades to "nothing already queued" so adds still land.
async fn live_queue_uris(
    client: &mut spotuify_spotify::SpotifyClient,
) -> std::collections::HashSet<String> {
    match actions::queue(client).await {
        Ok(queue) if queue.session_active => {
            queue.items.iter().map(|item| item.uri.clone()).collect()
        }
        Ok(_) => std::collections::HashSet::new(),
        Err(err) => {
            tracing::debug!(error = %err, "queue dedup skipped: live queue fetch failed");
            std::collections::HashSet::new()
        }
    }
}

/// The queue is a set: keep only items whose URI is not already queued
/// and not duplicated earlier in the same batch. Returns the kept items
/// and the number skipped. Spotify has no queue-move, so an existing
/// entry stays where it is rather than moving up.
fn dedup_queue_items(
    items: Vec<MediaItem>,
    already_queued: &std::collections::HashSet<String>,
) -> (Vec<MediaItem>, usize) {
    let mut seen = already_queued.clone();
    let mut kept = Vec::with_capacity(items.len());
    let mut skipped = 0usize;
    for item in items {
        if seen.insert(item.uri.clone()) {
            kept.push(item);
        } else {
            skipped += 1;
        }
    }
    (kept, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn item(uri: &str) -> MediaItem {
        MediaItem {
            uri: uri.to_string(),
            name: uri.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn dedup_skips_uris_already_in_the_live_queue() {
        let already: HashSet<String> = ["spotify:track:a".to_string()].into_iter().collect();
        let (kept, skipped) = dedup_queue_items(
            vec![item("spotify:track:a"), item("spotify:track:b")],
            &already,
        );
        assert_eq!(
            kept.iter().map(|i| i.uri.as_str()).collect::<Vec<_>>(),
            vec!["spotify:track:b"]
        );
        assert_eq!(skipped, 1);
    }

    #[test]
    fn dedup_collapses_duplicates_within_the_batch() {
        let (kept, skipped) = dedup_queue_items(
            vec![
                item("spotify:track:a"),
                item("spotify:track:a"),
                item("spotify:track:b"),
                item("spotify:track:a"),
            ],
            &HashSet::new(),
        );
        assert_eq!(
            kept.iter().map(|i| i.uri.as_str()).collect::<Vec<_>>(),
            vec!["spotify:track:a", "spotify:track:b"]
        );
        assert_eq!(skipped, 2);
    }

    #[test]
    fn dedup_keeps_everything_when_queue_unknown() {
        let (kept, skipped) = dedup_queue_items(
            vec![item("spotify:track:a"), item("spotify:track:b")],
            &HashSet::new(),
        );
        assert_eq!(kept.len(), 2);
        assert_eq!(skipped, 0);
    }
}
