//! `playback` request handlers (split out of the dispatch god-function).

use std::sync::Arc;

use spotuify_core::{
    MediaItem, PlayRequest, PlaySource, ProviderError, RequestContext, ResourceUri,
    TransportCommand, TransportDevice, UriScheme,
};
use spotuify_protocol::{
    DaemonEvent, MutationId, OperationKind, OperationSource, PlaybackCommand, Request, ResponseData,
};

use crate::handler::*;
use crate::state::{player_error_for_display, DaemonState};

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
            let mut command_kind = playback_command_kind(command.clone());
            // Auth latches must win before lazy provider/player construction.
            // In no-embedded builds the configured player cannot be built, but
            // an auth-blocked command still has the stable AuthRequired /
            // AuthRevoked contract. Resolve the configured owner without
            // touching the registry, including a secondary Spotify adapter
            // behind a no-auth default.
            if state.auth_gate_error().is_some() {
                let provider_id = match &command {
                    PlaybackCommand::PlayUri { uri, .. } => {
                        let resource = ResourceUri::parse(uri)?;
                        if resource.scheme() == &UriScheme::Spotify {
                            state.configured_health_auth_target().await?.provider_id
                        } else {
                            spotuify_core::ProviderId::new(resource.scheme().label()).map_err(
                                |error| ProviderError::InvalidInput {
                                    field: "provider".to_string(),
                                    message: error.to_string(),
                                },
                            )?
                        }
                    }
                    _ => match state.active_transport_provider() {
                        Some(provider_id) => provider_id,
                        None => state.configured_auth_target(None).await?.provider_id,
                    },
                };
                reject_if_auth_blocked(&state, Some(&provider_id)).await?;
            }
            // Route from the tapped URI before materialising any collection
            // context. In particular, Liked Songs is provider-scoped; resolving
            // it against the aggregate cache would leak foreign URIs into the
            // selected adapter's ordered play request.
            let (command_provider, command_transport) =
                provider_pair_for_command(&state, &command_kind).await?;
            let command_provider_id = command_provider.id().clone();
            // Resolve an optional collection context daemon-side: an
            // album/playlist URI, or the Liked-Songs sentinel → the full
            // ordered track list from the local cache. This lets the player
            // + Web-API paths start playback INSIDE the collection at the
            // tapped track so "Next" advances through the rest. Resolving
            // from the store is a fast, bounded local read and happens well
            // before any network transport.
            if let (
                CommandKind::PlayUri { context, .. },
                PlaybackCommand::PlayUri {
                    context_uri: Some(requested_context),
                    ..
                },
            ) = (&mut command_kind, &command)
            {
                *context = resolve_play_context(
                    &state,
                    command_provider.as_ref(),
                    Some(requested_context.as_str()),
                )
                .await?;
            }
            let uses_embedded_transport = provider_pair_uses_embedded_transport(
                &state,
                command_provider.as_ref(),
                command_transport.as_ref(),
            )
            .await?;
            let fast_transport = if uses_embedded_transport {
                transport_cmd_for_command_kind(&command_kind, &pre_command_playback)
            } else {
                None
            };
            // The post-command queue rail follows the *effective* context:
            // carry both the tapped track and the resolved context so the
            // synthesized queue can start at the right track.
            let queue_context = match &command_kind {
                CommandKind::PlayUri { uri, context } => {
                    Some((command_provider_id.clone(), uri.clone(), context.clone()))
                }
                CommandKind::PlayItem { item } => {
                    Some((command_provider_id.clone(), item.uri.clone(), None))
                }
                _ => None,
            };
            // Analytics context = the collection when one was requested,
            // else the tapped URI (unchanged). Only set on an explicit play
            // so pause/next keep the existing context.
            let analytics_context = match &command {
                PlaybackCommand::PlayUri { uri, context_uri } => {
                    Some(context_uri.clone().unwrap_or_else(|| uri.clone()))
                }
                _ => queue_context.as_ref().map(|(_, uri, _)| uri.clone()),
            };
            reject_if_auth_blocked(&state, Some(&command_provider_id)).await?;
            // Bump the mutation seq BEFORE the Spotify call so any
            // background poll-in-flight (sync_loop, spawn_*_refresh)
            // sees a newer seq and discards its stale pre-mutation
            // snapshot instead of overwriting the optimistic local
            // cache. Capture the bumped value HERE, not inside the
            // spawned closure: a second transport racing in between
            // the bump and the closure's first poll would otherwise
            // hand both commands the same (latest) seq and let the
            // older command's stale result persist over the newer
            // one's. See `DaemonState::mutation_seq`.
            let captured_seq = state.bump_mutation_seq();
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
                    // Same gate as try_embedded_transport: Spirc drops
                    // transport while our device isn't the active
                    // session, so claiming "applied" here would be a lie
                    // — fall through to the Web API path instead.
                    if embedded_transport_allowed(&state, cmd, &pre_command_playback) {
                        apply_fast_transport(&state, cmd.clone(), effective_command, action).await
                    } else {
                        None
                    }
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
                mutation_id,
                move |_op_id| async move {
                    // `captured_seq` was taken at OUR bump site (moved into
                    // this closure), so `persist_command_result` measures
                    // against exactly the mutation that fired this command.
                    // A second transport bumps past it and drops us; we can
                    // never accidentally adopt the newer command's seq.
                    let acquire = async {
                        if let Some(result) = fast_transport_result {
                            return Ok(result);
                        }
                        // Belt-and-suspenders: catch a latch flip between the
                        // sync pre-check and the body's provider acquisition.
                        reject_if_auth_blocked(&state_for, Some(&command_provider_id)).await?;
                        if uses_embedded_transport {
                            if let Some(result) =
                                try_embedded_transport(&state_for, &background_command_kind).await
                            {
                                return Ok(result);
                            }
                        }
                        execute_provider_pair_with_recovery(
                            &state_for,
                            command_provider,
                            command_transport,
                            background_command_kind,
                        )
                        .await
                    };
                    let result = match acquire.await {
                        Ok(result) => result,
                        Err(err) => {
                            // The optimistic clock apply + predicted queue
                            // were already broadcast as truth. Reconcile so
                            // clients snap back instead of keeping the wrong
                            // now-playing until the next slow poll: bump the
                            // seq (so the refreshes outrank the optimistic
                            // persist) and fetch real state. Mirrors the late-
                            // failure recovery in spawn_fast_transport_ack_watcher.
                            let reconcile_seq = state_for.bump_mutation_seq();
                            spawn_playback_refresh_forced(state_for.clone());
                            spawn_queue_refresh_with_seq(state_for.clone(), reconcile_seq);
                            return Err(err);
                        }
                    };
                    if let Some(context) = analytics_context {
                        state_for.set_playback_context(Some(context));
                    }
                    let ownership_changed =
                        state_for.set_active_transport_provider(command_provider_id.clone());
                    let applied_seq = if ownership_changed {
                        state_for.bump_mutation_seq()
                    } else {
                        captured_seq
                    };
                    // Phase 1: persist BEFORE the event so subscribers
                    // that fetch on the event see fresh state.
                    let outcome = persist_command_result(
                        &state_for,
                        &command_provider_id,
                        applied_seq,
                        &result,
                        action,
                        expected_playback.as_ref(),
                    )
                    .await;
                    tracing::debug!(
                        target: "spotuify_daemon::post_command",
                        action,
                        captured_seq = applied_seq,
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
                    if let Some((provider, start_uri, context)) = queue_context.as_ref() {
                        if let Some(queue) = context_queue_snapshot_for_play(
                            &state_for,
                            provider,
                            start_uri,
                            context.as_ref(),
                        )
                        .await
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
                        spawn_queue_refresh_delayed(state_for.clone(), 1200, applied_seq);
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
            let provider = current_snapshot_provider_id(&state).await?;
            let devices = cached_devices_with_own_device(&state, &provider).await?;
            spawn_devices_refresh(state.clone());
            Ok(ResponseData::Devices { devices })
        }
        Request::DeviceTransfer { device } => {
            let state_for = state.clone();
            let (provider, transport) = current_transport_provider_pair(&state).await?;
            // DeviceTransfer mutates the active device which the
            // playback poll keys off of; bump seq so a polling refresh
            // that started before this call can't repopulate the
            // pre-transfer device. Capture at the bump site (not in
            // the closure) so a racing mutation can't hand us its seq.
            let captured_seq = state.bump_mutation_seq();
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
                mutation_id,
                move |op_id| async move {
                    let transport_caps = provider.capabilities().transport.ok_or_else(|| {
                        ProviderError::unsupported(format!("provider {} transport", provider.id()))
                    })?;
                    require_provider_capability(
                        provider.as_ref(),
                        "device listing",
                        transport_caps.devices,
                    )?;
                    require_provider_capability(
                        provider.as_ref(),
                        "device transfer",
                        transport_caps.transfer,
                    )?;
                    let mut devices = transport.devices(RequestContext::PLAYBACK_CONTROL).await?;
                    let uses_embedded_transport = provider_pair_uses_embedded_transport(
                        &state_for,
                        provider.as_ref(),
                        transport.as_ref(),
                    )
                    .await?;
                    if uses_embedded_transport {
                        if let Some(own_device) = state_for.own_device_entry().await {
                            let own_id = own_device.id.as_deref();
                            if !devices
                                .iter()
                                .any(|candidate| candidate.id.as_deref() == own_id)
                            {
                                devices.push(own_device);
                            }
                        }
                    }
                    cache_devices(&state_for, provider.id(), &devices).await;
                    let target_device = resolve_device(&devices, &device)?;
                    // The embedded device stays listed while the player idles
                    // after a session drop (see `own_device_entry`), but the
                    // Web API can only transfer to a live device — reconnect
                    // it first, then give the cluster registration a moment
                    // to land before the transfer call.
                    let target_is_own =
                        uses_embedded_transport && state_for.device_is_ours(&target_device);
                    if target_is_own && !state_for.player_is_connected().await {
                        tracing::info!(
                            device = %target_device.name,
                            "transfer targets idle embedded device; reconnecting first"
                        );
                        state_for.reconnect_player(&target_device.name).await?;
                        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                    }
                    let playback = state_for.snapshot_playback();
                    let play = playback.is_playing;
                    let prior_device_id = playback.device.as_ref().and_then(|d| d.id.clone());
                    let pre_state = spotuify_protocol::PreState::Transfer {
                        prior_device_id: prior_device_id.clone(),
                    };
                    let plan = match prior_device_id.clone() {
                        Some(id) => {
                            spotuify_protocol::ReversalPlan::TransferToPriorDevice {
                                device_id: id,
                                provider: Some(provider.id().clone()),
                            }
                        }
                        None => spotuify_protocol::ReversalPlan::NotReversible {
                            reason: "no prior active device to restore".to_string(),
                        },
                    };
                    state_for
                        .store()
                        .update_operation_plan(op_id, Some(&pre_state), Some(&plan))
                        .await?;
                    let device_name = target_device.name.clone();
                    let device_id = target_device.id.clone();
                    let target_device_for_heal = target_device.clone();
                    let result = match execute_provider_pair_with_recovery(
                        &state_for,
                        provider.clone(),
                        transport.clone(),
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
                        Err(err) => return Err(err),
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
                    // Heal silent transfers. When the source was our embedded
                    // librespot playing a contextless single track (loaded via
                    // `from_tracks`), the Spotify transfer state has no
                    // resolvable context — the target accepts the transfer but
                    // plays nothing, and Spotify reports no active device.
                    // Detect that the target did not become active and
                    // re-assert the current track on it so audio actually
                    // starts. Real context playback (playlist/album) already
                    // activates the target, so this leaves it untouched.
                    let mut healed_snapshot: Option<spotuify_core::Playback> = None;
                    if play && !to_own_device {
                        let target_active = result.playback.as_ref().is_some_and(|p| {
                            p.device.as_ref().and_then(|d| d.id.as_deref())
                                == device_id.as_deref()
                        });
                        if !target_active {
                            if let (Some(target_id), Some(uri)) = (
                                device_id.as_deref(),
                                playback.item.as_ref().map(|item| item.uri.clone()),
                            ) {
                                let command = TransportCommand::Play(PlayRequest {
                                    start_uri: ResourceUri::parse(&uri)?,
                                    source: PlaySource::Single,
                                    device: TransportDevice::Id(target_id.to_string()),
                                    position_ms: playback.progress_ms,
                                });
                                require_transport_command_capability(
                                    provider.as_ref(),
                                    &command,
                                )?;
                                match transport
                                    .execute(RequestContext::PLAYBACK_CONTROL, command)
                                    .await
                                {
                                    Ok(_) => {
                                        tracing::info!(
                                            device = %device_name, %uri,
                                            "re-asserted playback on target after contextless transfer"
                                        );
                                        healed_snapshot = Some(spotuify_core::Playback {
                                            item: playback.item.clone(),
                                            device: Some(target_device_for_heal.clone()),
                                            is_playing: true,
                                            progress_ms: playback.progress_ms,
                                            shuffle: playback.shuffle,
                                            repeat: playback.repeat,
                                            sampled_at_ms: Some(spotuify_core::now_ms()),
                                            provider_timestamp_ms: None,
                                            source: Some(
                                                spotuify_core::PlaybackStateSource::CommandResult,
                                            ),
                                        });
                                    }
                                    Err(err) => tracing::warn!(
                                        error = %err, device = %device_name,
                                        "post-transfer re-assert failed"
                                    ),
                                }
                            }
                        }
                    }
                    // Phase 1: capture any playback/devices snapshot the
                    // Transfer ACK returned so subscribers don't need a
                    // re-fetch round-trip.
                    let _outcome = persist_command_result(
                        &state_for,
                        provider.id(),
                        captured_seq,
                        &result,
                        "transfer",
                        None,
                    )
                    .await;
                    // Apply the heal snapshot AFTER persist so it wins over the
                    // post-transfer readback (which still shows the source
                    // device): optimistically move the clock to the target so
                    // every client reflects the hand-off immediately, rather
                    // than waiting for a poll to age past the stale local
                    // PlayerEvent.
                    if let Some(healed) = healed_snapshot {
                        state_for
                            .playback_clock()
                            .apply_command_result(&healed, spotuify_core::now_ms());
                    }
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
        Request::RecentlyPlayed { provider } => {
            // Non-blocking: empty list is fine on cold start. Refresh
            // populates the cache and subscribers re-fetch when they
            // see SyncFinished or the next PlaybackChanged.
            let (provider, _) = state.provider_or_default(provider.as_ref()).await?;
            let items = state
                .store()
                .list_provider_recent_items(20, Some(&provider))
                .await?;
            spawn_recent_refresh(state.clone(), provider);
            Ok(ResponseData::MediaItems { items })
        }
        Request::QueueGet => {
            // Non-blocking: return the last known queue immediately,
            // then refresh in the background. Returning default here
            // makes clients briefly clear their visible queue on every
            // fallback read/reseed.
            let provider = current_snapshot_provider_id(&state).await?;
            let queue = state.queue_snapshot_for_clients(
                state
                    .store()
                    .latest_provider_queue(500, &provider)
                    .await?
                    .unwrap_or_default(),
            );
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
                reason: "the remote queue has no remove operation".to_string(),
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
                mutation_id,
                move |_op_id| async move {
                    let resource = ResourceUri::parse(&uri)?;
                    let provider = state_for_event.provider_for_uri(&resource).await?;
                    let transport = state_for_event.provider_transport(provider.id()).await?;
                    let resolved_items =
                        queueable_items_for_selection(&state_for_event, provider.as_ref(), &uri)
                            .await?;
                    // The queue is a set: a track appears at most once.
                    // Dedup against the LIVE queue only — never the
                    // persisted snapshot, which is historical by design
                    // and may describe a dead Spotify session. Spotify
                    // has no queue-move, so an existing entry stays put
                    // rather than moving up; a failed live fetch
                    // degrades to no dedup.
                    let already_queued = live_queue_uris(
                        transport.as_ref(),
                        provider
                            .capabilities()
                            .transport
                            .as_ref()
                            .is_some_and(|caps| caps.queue_read),
                    )
                    .await;
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
                    let selection_kind = spotuify_core::ResourceUri::parse(&uri)?.kind();
                    let idle_context_label = idle_context_start_label(&selection_kind);

                    // "Queue" needs an active provider session. The selected
                    // provider transport is authoritative; for Spotify this
                    // is the Web API `POST /me/player/queue`, which 404s with
                    // NO_ACTIVE_DEVICE when nothing is playing. When the
                    // provider reports no active device for the first item, the
                    // session is idle: start embedded playback. Track/episode
                    // selections play the first item then queue the rest;
                    // album/playlist selections start their context so the
                    // backend owns natural progression. Keying off Spotify's
                    // actual error (not the local clock) means a remote
                    // session the daemon hasn't polled yet is never hijacked.
                    let mut played_first = false;
                    let mut played_context = false;
                    let mut applied_items = Vec::new();
                    let mut applied_uris = Vec::new();
                    // The first item reveals whether a session exists: queue
                    // it, and if Spotify reports no active device, start one by
                    // playing it on the embedded device instead.
                    if let Some(first) = queue_uris.first().cloned() {
                        match queue_one(provider.as_ref(), transport.as_ref(), &first).await? {
                            QueueAttempt::Queued => {
                                applied_items.push(queued_items[0].clone());
                                applied_uris.push(first);
                                state_for_event
                                    .set_active_transport_provider(provider.id().clone());
                                state_for_event.bump_mutation_seq();
                            }
                            QueueAttempt::NoActiveDevice => {
                                let start_uri = if idle_context_label.is_some() {
                                    played_context = true;
                                    uri.clone()
                                } else {
                                    first.clone()
                                };
                                start_embedded_queue_session(
                                    state_for_event.as_ref(),
                                    provider.as_ref(),
                                    transport.as_ref(),
                                    &start_uri,
                                )
                                .await?;
                                played_first = true;
                                state_for_event
                                    .set_active_transport_provider(provider.id().clone());
                                state_for_event.bump_mutation_seq();
                            }
                        }
                    }

                    if !played_context {
                        for (queue_uri, queue_item) in
                            queue_uris.iter().zip(queued_items.iter()).skip(1)
                        {
                            // After an idle auto-play, the new session takes a
                            // beat to register with Spotify, so retry briefly
                            // instead of racing it with another 404.
                            let mut attempt = 0u32;
                            loop {
                                match queue_one(provider.as_ref(), transport.as_ref(), queue_uri)
                                    .await
                                {
                                    Ok(QueueAttempt::Queued) => {
                                        applied_items.push(queue_item.clone());
                                        applied_uris.push(queue_uri.clone());
                                        break;
                                    }
                                    Ok(QueueAttempt::NoActiveDevice)
                                        if played_first && attempt < 6 =>
                                    {
                                        attempt += 1;
                                        tokio::time::sleep(std::time::Duration::from_millis(500))
                                            .await;
                                    }
                                    Ok(QueueAttempt::NoActiveDevice) => {
                                        return Err(report_partial_queue_application(
                                            state_for_event.clone(),
                                            provider.clone(),
                                            transport.clone(),
                                            applied_items,
                                            applied_uris,
                                            &already_queued,
                                            played_first.then(|| queued_items[0].clone()),
                                            state_for_event.current_mutation_seq(),
                                            anyhow::Error::new(ProviderError::NoActiveDevice)
                                                .context(format!(
                                                    "queue add for {queue_uri} failed"
                                                )),
                                        )
                                        .await);
                                    }
                                    Err(error) => {
                                        return Err(report_partial_queue_application(
                                            state_for_event.clone(),
                                            provider.clone(),
                                            transport.clone(),
                                            applied_items,
                                            applied_uris,
                                            &already_queued,
                                            played_first.then(|| queued_items[0].clone()),
                                            state_for_event.current_mutation_seq(),
                                            error.context(format!(
                                                "queue add for {queue_uri} failed"
                                            )),
                                        )
                                        .await);
                                    }
                                }
                            }
                        }
                    }

                    // What actually landed in the queue: the first item too,
                    // unless we auto-played it to start the session.
                    let queue_snapshot = cache_optimistic_queue_with_appends(
                        &state_for_event,
                        provider.id(),
                        applied_items,
                        &already_queued,
                    )
                    .await;
                    let skip_note = if skipped_dupes > 0 {
                        format!(", skipped {skipped_dupes} already queued")
                    } else {
                        String::new()
                    };
                    let message = if played_first && applied_uris.is_empty() {
                        if let Some(label) = idle_context_label {
                            format!("playing {label} now")
                        } else {
                            "playing now".to_string()
                        }
                    } else if played_first {
                        format!(
                            "playing now, queued {} item(s){skip_note}",
                            applied_uris.len()
                        )
                    } else {
                        format!("queued {} item(s){skip_note}", applied_uris.len())
                    };
                    state_for_event.emit_event(DaemonEvent::QueueChanged {
                        action: "queue".to_string(),
                        uris: applied_uris.clone(),
                        queue: queue_snapshot,
                    });
                    state_for_event.warm_queue_uris(applied_uris.clone());
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
            validate_queue_batch_provider(&uris)?;
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
                    reason: "the remote queue has no remove operation".to_string(),
                }),
                mutation_lane,
                mutation_id,
                move |_op_id| async move {
                    // Expand each selection (tracks pass through; album/playlist
                    // URIs expand to their tracks).
                    let mut resolved_items: Vec<MediaItem> = Vec::new();
                    let mut queue_provider = None;
                    let mut transport = None;
                    let mut queue_read = false;
                    for selection in &uris {
                        let resource = ResourceUri::parse(selection)?;
                        let provider = state_for_event.provider_for_uri(&resource).await?;
                        if transport.is_none() {
                            queue_read = provider
                                .capabilities()
                                .transport
                                .as_ref()
                                .is_some_and(|caps| caps.queue_read);
                            transport =
                                Some(state_for_event.provider_transport(provider.id()).await?);
                            queue_provider = Some(provider.clone());
                        }
                        let resolved = queueable_items_for_selection(
                            &state_for_event,
                            provider.as_ref(),
                            selection,
                        )
                        .await?;
                        resolved_items.extend(resolved);
                    }
                    let Some(transport) = transport else {
                        emit_mutation_finished(&state_for_event, "queue", "nothing to queue");
                        return Ok(());
                    };
                    let queue_provider = queue_provider
                        .ok_or_else(|| anyhow::anyhow!("queue provider was not resolved"))?;
                    // Queue set semantics — same rule as Request::QueueAdd
                    // above: dedup against the live queue and within the
                    // batch itself.
                    let already_queued = live_queue_uris(transport.as_ref(), queue_read).await;
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
                    let mut applied_items = Vec::new();
                    let mut applied_uris = Vec::new();
                    if let Some(first) = queue_uris.first().cloned() {
                        match queue_one(queue_provider.as_ref(), transport.as_ref(), &first).await?
                        {
                            QueueAttempt::Queued => {
                                applied_items.push(queued_items[0].clone());
                                applied_uris.push(first);
                                state_for_event
                                    .set_active_transport_provider(queue_provider.id().clone());
                                state_for_event.bump_mutation_seq();
                            }
                            QueueAttempt::NoActiveDevice => {
                                start_embedded_queue_session(
                                    state_for_event.as_ref(),
                                    queue_provider.as_ref(),
                                    transport.as_ref(),
                                    &first,
                                )
                                .await?;
                                played_first = true;
                                state_for_event
                                    .set_active_transport_provider(queue_provider.id().clone());
                                state_for_event.bump_mutation_seq();
                            }
                        }
                    }
                    for (queue_uri, queue_item) in
                        queue_uris.iter().zip(queued_items.iter()).skip(1)
                    {
                        let mut attempt = 0u32;
                        loop {
                            match queue_one(queue_provider.as_ref(), transport.as_ref(), queue_uri)
                                .await
                            {
                                Ok(QueueAttempt::Queued) => {
                                    applied_items.push(queue_item.clone());
                                    applied_uris.push(queue_uri.clone());
                                    break;
                                }
                                Ok(QueueAttempt::NoActiveDevice) if played_first && attempt < 6 => {
                                    attempt += 1;
                                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                                }
                                Ok(QueueAttempt::NoActiveDevice) => {
                                    return Err(report_partial_queue_application(
                                        state_for_event.clone(),
                                        queue_provider.clone(),
                                        transport.clone(),
                                        applied_items,
                                        applied_uris,
                                        &already_queued,
                                        played_first.then(|| queued_items[0].clone()),
                                        state_for_event.current_mutation_seq(),
                                        anyhow::Error::new(ProviderError::NoActiveDevice)
                                            .context(format!("queue add for {queue_uri} failed")),
                                    )
                                    .await);
                                }
                                Err(error) => {
                                    return Err(report_partial_queue_application(
                                        state_for_event.clone(),
                                        queue_provider.clone(),
                                        transport.clone(),
                                        applied_items,
                                        applied_uris,
                                        &already_queued,
                                        played_first.then(|| queued_items[0].clone()),
                                        state_for_event.current_mutation_seq(),
                                        error.context(format!("queue add for {queue_uri} failed")),
                                    )
                                    .await);
                                }
                            }
                        }
                    }
                    let queue_snapshot = cache_optimistic_queue_with_appends(
                        &state_for_event,
                        queue_provider.id(),
                        applied_items,
                        &already_queued,
                    )
                    .await;
                    let skip_note = if skipped_dupes > 0 {
                        format!(", skipped {skipped_dupes} already queued")
                    } else {
                        String::new()
                    };
                    let message = if played_first {
                        format!(
                            "playing now, queued {} item(s){skip_note}",
                            applied_uris.len()
                        )
                    } else {
                        format!("queued {} item(s){skip_note}", applied_uris.len())
                    };
                    state_for_event.emit_event(DaemonEvent::QueueChanged {
                        action: "queue".to_string(),
                        uris: applied_uris.clone(),
                        queue: queue_snapshot,
                    });
                    state_for_event.warm_queue_uris(applied_uris.clone());
                    spawn_queue_refresh(state_for_event.clone());
                    emit_mutation_finished(&state_for_event, "queue", &message);
                    Ok(())
                },
            )
            .await
        }
        Request::Reconnect => {
            tracing::info!("daemon reconnect requested");
            let device_name = state.configured_device_name();
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
            if let Some(requested) = device.as_deref() {
                let outputs = tokio::time::timeout(
                    std::time::Duration::from_secs(3),
                    tokio::task::spawn_blocking(crate::server::list_audio_outputs),
                )
                .await
                .map_err(|_| anyhow::anyhow!("audio output enumeration timed out"))??;
                if !outputs.iter().any(|output| output == requested) {
                    return Err(ProviderError::InvalidInput {
                        field: "device".to_string(),
                        message: format!("audio output `{requested}` is not available"),
                    }
                    .into());
                }
            }
            let snapshot = state.snapshot_playback();
            state.set_player_audio_output(device.clone()).await?;
            let device_name = state.configured_device_name();
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
                                error = %player_error_for_display(&err),
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

fn validate_queue_batch_provider(uris: &[String]) -> anyhow::Result<Option<UriScheme>> {
    let mut expected: Option<UriScheme> = None;
    for raw in uris {
        let uri = ResourceUri::parse(raw)?;
        match expected.as_ref() {
            Some(scheme) if scheme != uri.scheme() => {
                return Err(ProviderError::InvalidInput {
                    field: "uris".to_string(),
                    message: format!(
                        "queue batch mixes provider schemes `{scheme}` and `{}`",
                        uri.scheme()
                    ),
                }
                .into());
            }
            Some(_) => {}
            None => expected = Some(uri.scheme().clone()),
        }
    }
    Ok(expected)
}

pub(crate) async fn start_embedded_queue_session(
    state: &DaemonState,
    provider: &dyn spotuify_core::MusicProvider,
    transport: &dyn spotuify_core::RemoteTransport,
    uri: &str,
) -> anyhow::Result<()> {
    if !provider_pair_uses_embedded_transport(state, provider, transport).await? {
        return Err(ProviderError::NoActiveDevice.into());
    }
    state
        .transport(crate::state::TransportCmd::PlayUri {
            uri: uri.to_string(),
            position_ms: 0,
        })
        .await
        .map_err(|error| {
            anyhow::anyhow!(
                "failed to start playback for {uri}: {}",
                player_error_for_display(&error)
            )
        })
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn report_partial_queue_application(
    state: Arc<DaemonState>,
    provider: Arc<dyn spotuify_core::MusicProvider>,
    transport: Arc<dyn spotuify_core::RemoteTransport>,
    applied_items: Vec<MediaItem>,
    applied_uris: Vec<String>,
    live_uris: &std::collections::HashSet<String>,
    started_item: Option<MediaItem>,
    captured_seq: u64,
    failure: anyhow::Error,
) -> anyhow::Error {
    let applied_count = applied_uris.len() + usize::from(started_item.is_some());
    let queue = cache_optimistic_queue_application(
        state.as_ref(),
        provider.id(),
        started_item,
        applied_items,
        live_uris,
    )
    .await;
    state.emit_event(DaemonEvent::QueueChanged {
        action: "queue-partially-applied".to_string(),
        uris: applied_uris,
        queue,
    });
    spawn_queue_refresh_for_pair(state.clone(), provider, transport, captured_seq);
    let message = format!(
        "queue partially applied ({applied_count} action(s) succeeded): {failure}; inspect the queue before retrying"
    );
    emit_mutation_finished(&state, "queue", &message);
    failure.context(message)
}

/// Fetch the live queue's URIs for add-time dedup. Only a queue Spotify
/// reports as belonging to an ACTIVE session counts; a cached fallback
/// snapshot must not veto adds (it may describe a dead session). A
/// failed fetch degrades to "nothing already queued" so adds still land.
pub(crate) async fn live_queue_uris(
    transport: &dyn spotuify_core::RemoteTransport,
    queue_read_supported: bool,
) -> std::collections::HashSet<String> {
    if !queue_read_supported {
        return std::collections::HashSet::new();
    }
    match transport.queue(RequestContext::PLAYBACK_CONTROL).await {
        Ok(queue) if queue.session_active => {
            record_daemon_action(
                "queue",
                queue
                    .currently_playing
                    .as_ref()
                    .map(|item| item.uri.as_str()),
                serde_json::json!({"upcoming_count": queue.items.len()}),
            )
            .await;
            queue.items.iter().map(|item| item.uri.clone()).collect()
        }
        Ok(queue) => {
            record_daemon_action(
                "queue",
                queue
                    .currently_playing
                    .as_ref()
                    .map(|item| item.uri.as_str()),
                serde_json::json!({"upcoming_count": queue.items.len()}),
            )
            .await;
            std::collections::HashSet::new()
        }
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
pub(crate) fn dedup_queue_items(
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
    #![allow(clippy::unwrap_used)]

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

    #[test]
    fn queue_batch_rejects_mixed_provider_schemes() {
        let error = validate_queue_batch_provider(&[
            "spotify:track:a".to_string(),
            "apple-music:track:b".to_string(),
        ])
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<ProviderError>(),
            Some(ProviderError::InvalidInput { field, .. }) if field == "uris"
        ));
    }

    #[test]
    fn queue_batch_accepts_one_provider_scheme() {
        let scheme = validate_queue_batch_provider(&[
            "spotify:track:a".to_string(),
            "spotify:album:b".to_string(),
        ])
        .unwrap();
        assert_eq!(scheme.as_ref().map(UriScheme::label), Some("spotify"));
    }

    #[tokio::test]
    async fn queue_add_updates_the_selected_provider_transport() {
        use spotuify_core::RemoteTransport as _;

        let provider = spotuify_provider_fake::FakeProvider::new();
        assert!(matches!(
            queue_one(&provider, &provider, "fake:track:track-2")
                .await
                .unwrap(),
            QueueAttempt::Queued
        ));

        let queue = provider
            .queue(RequestContext::PLAYBACK_CONTROL)
            .await
            .unwrap();
        assert_eq!(
            queue.items.last().map(|item| item.uri.as_str()),
            Some("fake:track:track-2")
        );
    }
}
