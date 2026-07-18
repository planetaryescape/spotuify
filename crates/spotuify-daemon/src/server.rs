use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::{FutureExt, SinkExt, StreamExt};
use tokio::sync::Semaphore;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::codec::Framed;

use crate::analytics::AnalyticsStore;
use crate::handler::{handle_request_with_source, handle_request_with_source_and_mutation};
use crate::retention::retention_cutoffs;
use crate::state::DaemonState;
use spotuify_core::ProviderError;
use spotuify_protocol::ipc_stream::{IpcListener, IpcStream};
use spotuify_protocol::{
    DaemonEvent, DaemonStatus, IpcCodec, IpcErrorKind, IpcMessage, IpcPayload, OperationSource,
    Request, Response,
};

// Client-side daemon launcher (ensure/start/restart/status, socket
// probes, build-id compatibility) lives in `spotuify-launcher` so the
// CLI never links the daemon. Re-exported here so the binary's `daemon`
// subcommands and the TUI keep calling `server::…` unchanged.
pub use spotuify_launcher::{
    clear_daemon_pid_file, current_build_id, current_daemon_version, daemon_status,
    ensure_daemon_running, inspect_socket_state, no_daemon_start, remove_stale_socket,
    restart_daemon, stop_daemon, SocketState,
};

/// Background-query and ambient request budget. Sized generously
/// since handlers are cheap (cached reads, doctor scrapes).
const REQUEST_CONCURRENCY_LIMIT: usize = 64;
/// Dedicated fast lane for transport mutations + their immediate
/// query partners. Even when the slow lane is saturated by a sync
/// burst or a doctor sweep, a Pause / Resume / Seek / Toggle / Next
/// / Previous / Volume / Shuffle / Repeat / DeviceTransfer /
/// QueueAdd / PlaybackGet can still be admitted. Transport mutations
/// only hold their permit through the optimistic receipt write +
/// `MutationAccepted` emit (sub-ms); 16 permits is far more than the
/// peak burst we expect from a single TUI + MCP + CLI talking at
/// once.
const TRANSPORT_CONCURRENCY_LIMIT: usize = 16;
const CONNECTION_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const AUTH_HEALTH_INTERVAL: Duration = Duration::from_secs(60);
const PLAYER_HEALTH_INTERVAL: Duration = Duration::from_secs(60);
/// Audio-flow watchdog cadence. Tight (the 60s health loop is far too coarse
/// to catch "shows playing, no sound" promptly).
const AUDIO_WATCHDOG_TICK: Duration = Duration::from_secs(2);
/// Require this much *flat* sample count while `is_playing` before declaring a
/// stall. Comfortably exceeds track-transition gaps (the counter resets per
/// track, read as flowing) and pre-roll buffering, to avoid false positives.
const AUDIO_STALL_THRESHOLD_MS: i64 = 6_000;

pub async fn run_daemon() -> Result<()> {
    // `spotuify daemon start` runs detached with stderr pointed at the
    // null device, so a startup error that escapes here would otherwise
    // vanish. Log the full chain to the daemon log file — the operator's
    // only diagnostic when the process dies before opening the socket —
    // before propagating it to the (silent) caller.
    if let Err(err) = run_daemon_impl().await {
        tracing::error!(error = %err, error_chain = ?err, "daemon exited during startup");
        return Err(err);
    }
    Ok(())
}

async fn run_daemon_impl() -> Result<()> {
    spotuify_protocol::paths::secure_current_instance_dirs()
        .context("failed to secure spotuify state directories")?;
    let socket_path = DaemonState::socket_path();
    // Unix sockets live in a real directory that must exist before bind.
    // On Windows the socket is a named pipe (`\\.\pipe\…`) with no
    // filesystem parent, so there is nothing to create — and trying would
    // fail attempting to mkdir inside the pipe namespace.
    #[cfg(not(windows))]
    if let Some(parent) = socket_path.parent() {
        if parent == spotuify_protocol::paths::runtime_dir() {
            spotuify_protocol::paths::ensure_private_dir(parent)
                .with_context(|| format!("failed to secure {}", parent.display()))?;
        } else {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
    }

    // Win the instance startup lock BEFORE inspecting/claiming the
    // socket. Two `daemon start`s racing here could otherwise both judge
    // the socket stale/missing and proceed, with the loser either
    // unlinking the winner's fresh socket or failing at bind after all
    // the init work. The advisory flock makes the claim atomic per
    // instance; held for the process lifetime, released on exit/crash.
    let _startup_lock = acquire_startup_lock(&spotuify_protocol::paths::runtime_dir())?;

    // Starting is the explicit opposite of an intentional stop: clear the
    // sentinel a prior `daemon stop` may have left so a supervising client
    // (the macOS menubar app) resumes keeping the daemon alive.
    let _ = std::fs::remove_file(spotuify_protocol::paths::intentional_stop_sentinel());

    match inspect_socket_state(&socket_path).await {
        SocketState::Reachable => anyhow::bail!(
            "daemon already running at {}. Try `spotuify daemon status`.",
            socket_path.display()
        ),
        SocketState::Stale => {
            remove_stale_socket(&socket_path);
            clear_daemon_pid_file();
        }
        SocketState::Missing => {}
    }

    // Preflight: current-instance cleanup. A previous `daemon start`
    // run can crash during init and orphan a process that still holds
    // this instance's Tantivy index lock. Never kill broad
    // `spotuify daemon` matches here: dev and prod daemons can
    // intentionally coexist under different instance paths.
    cleanup_zombie_daemons();
    clear_stale_tantivy_locks();

    // Test-only safety net: when a test harness spawns us it sets
    // `SPOTUIFY_EXIT_WITH_PARENT` to its own PID. Detached daemons
    // (`process_group(0)`) outlive a `cargo test`/`nextest` process that
    // is killed mid-run, so without this they orphan (PPID 1) and pile
    // up across runs. The watchdog exits us once that process is gone.
    // No-op in normal CLI/dev/prod use (env unset), so a real daemon
    // still survives the short-lived CLI that launched it.
    spawn_parent_death_watchdog();

    // Phase 0: backend init errors propagate from DaemonState::new and
    // are logged by the run_daemon wrapper before the process exits.
    let state = Arc::new(DaemonState::new().await?);
    state
        .store()
        .recover_running_provider_reconciliations()
        .await
        .context("failed to recover interrupted provider reconciliations")?;
    // Phase 9.1: bring up the player backend chosen by config.
    // Errors (e.g. spotifyd autostart failure) are logged but don't
    // block the daemon — playback commands return typed errors when
    // attempted.
    match recover_provider_mutation_lifecycle_after_startup(&state).await {
        Ok(count) if count > 0 => tracing::warn!(
            count,
            "recovered in-flight mutation claims as outcome-indeterminate"
        ),
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(%error, "provider mutation lifecycle recovery incomplete; retrying");
            spawn_processing_mutation_recovery_retry(state.clone());
        }
    }
    if state.has_embedded_player_provider() {
        let device_name = state.configured_device_name();
        if let Err(err) = state.ensure_player_ready(&device_name).await {
            tracing::warn!(error = %err, "player backend register_device failed; continuing");
        }
    }
    let media_control_task = spawn_media_control_command_loop(state.clone());
    let sync_tasks = spotuify_sync::spawn_background_scheduler(state.clone());
    let queue_warm_task = state.start_queue_warm_scheduler();
    spawn_auth_health_loop(state.clone());
    if state.has_embedded_player_provider() {
        spawn_player_health_loop(state.clone());
        spawn_audio_flow_watchdog(state.clone());
    }
    // Eager warm: fire a playback + queue + devices + recent pull
    // BEFORE the socket starts accepting connections so the very first
    // TUI launch can reconcile live playback/devices quickly without
    // blocking initial render. Fire-and-forget — failures
    // (no auth, no network) fall back gracefully to the synthetic /
    // empty response in the handlers. The background scheduler waits
    // until its first cadence tick so this warm path is the only
    // boot-time Spotify read burst.
    spawn_initial_cache_warm(state.clone());
    // Phase 12 (P12.7) — operations + analytics retention. Default
    // windows: 90d operations, 90d playback_progress, 365d events.
    // Pass 2 (P11.x) reads windows from config; the foundation default
    // matches blueprint.
    let retention_task = spawn_retention_loop(state.clone());
    // Update-awareness: poll GitHub releases (startup + every 6h) so clients
    // can surface "a newer release exists". Opt out with SPOTUIFY_NO_UPDATE_CHECK.
    let update_task = spawn_update_loop(state.clone());
    // macOS: follow the system default audio output (re-route playback when the
    // user switches their Mac's output device, if no device is pinned).
    #[cfg(target_os = "macos")]
    let audio_follow_task = Some(spawn_audio_follow_loop(state.clone()));
    #[cfg(not(target_os = "macos"))]
    let audio_follow_task: Option<JoinHandle<()>> = None;
    // Listening reminders: fire due/overdue reminders, emit ReminderDue.
    let reminder_task = crate::reminders::spawn_reminder_loop(state.clone());
    let mut listener = IpcListener::bind(&socket_path)
        .with_context(|| format!("failed to bind {}", socket_path.display()))?;
    spotuify_protocol::paths::secure_private_socket(&socket_path)
        .with_context(|| format!("failed to secure {}", socket_path.display()))?;
    write_daemon_pid_file()?;
    tracing::info!(socket = %socket_path.display(), "spotuify daemon listening");

    let request_semaphore = Arc::new(Semaphore::new(REQUEST_CONCURRENCY_LIMIT));
    let transport_semaphore = Arc::new(Semaphore::new(TRANSPORT_CONCURRENCY_LIMIT));
    let mut shutdown_rx = state.shutdown_receiver();
    let mut connections = JoinSet::new();

    loop {
        tokio::select! {
            joined = connections.join_next(), if !connections.is_empty() => {
                if let Some(Err(err)) = joined {
                    tracing::warn!(error = %err, "daemon client task failed");
                }
            }
            changed = shutdown_rx.changed() => {
                if changed.is_ok() && *shutdown_rx.borrow_and_update() {
                    tracing::info!("daemon shutdown requested");
                    break;
                }
            }
            accepted = listener.accept() => {
                let stream = match accepted {
                    Ok(stream) => stream,
                    Err(err) => {
                        // A transient accept error (e.g. EMFILE/ENFILE under
                        // load) must not take down the whole daemon and skip
                        // graceful drain. Log, back off briefly so a
                        // persistent error can't hot-spin the loop, then keep
                        // serving.
                        tracing::warn!(error = %err, "daemon accept failed; continuing");
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        continue;
                    }
                };
                let connection_state = state.clone();
                let request_semaphore = request_semaphore.clone();
                let transport_semaphore = transport_semaphore.clone();
                let event_rx = state.event_tx.subscribe();
                let connection_shutdown_rx = state.shutdown_receiver();
                connections.spawn(async move {
                    serve_client_connection(
                        stream,
                        connection_state,
                        request_semaphore,
                        transport_semaphore,
                        event_rx,
                        connection_shutdown_rx,
                    ).await;
                });
            }
        }
    }

    let _ = state.event_tx.send(IpcMessage {
        id: 0,
        source: None,
        mutation_id: None,
        payload: IpcPayload::Event(DaemonEvent::ShutdownRequested),
    });
    state
        .shutdown_background_tasks(CONNECTION_DRAIN_TIMEOUT)
        .await;
    state.shutdown_search().await;
    state.shutdown_player().await;
    drain_background_tasks(
        sync_tasks
            .into_iter()
            .map(spotuify_sync::AbortOnDropTask::into_join_handle)
            .chain(media_control_task)
            .chain(queue_warm_task)
            .chain(std::iter::once(retention_task))
            .chain(std::iter::once(update_task))
            .chain(audio_follow_task)
            .chain(std::iter::once(reminder_task))
            .collect(),
        CONNECTION_DRAIN_TIMEOUT,
    )
    .await;
    drop(listener);
    drain_connection_tasks(&mut connections, CONNECTION_DRAIN_TIMEOUT).await;
    remove_bound_socket(&socket_path);
    clear_daemon_pid_file();
    Ok(())
}

/// Keep auth alive independent of connected clients. Normal access
/// tokens expire hourly; `refresh_auth_health` refreshes through the
/// shared daemon token cache when the proactive headroom is reached
/// and updates the player token bridge used by embedded playback.
/// Periodically probe the embedded player session and auto-reconnect a
/// zombie (a session that went invalid without emitting
/// `SessionDisconnected` — dropped TCP, host sleep/wake). The
/// event-driven reconnect handles clean disconnects; this catches the
/// silent ones. The probe + reconnect decision + give-up logic lives on
/// `DaemonState::probe_player_health`; this loop just drives the cadence.
fn spawn_player_health_loop(state: Arc<DaemonState>) {
    let task_state = state.clone();
    state.spawn_background("player-health", async move {
        let mut shutdown_rx = task_state.shutdown_receiver();
        let mut interval = tokio::time::interval(PLAYER_HEALTH_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Skip the immediate first tick: the player is still registering
        // at startup, so a probe now would be a spurious failure.
        interval.tick().await;

        loop {
            tokio::select! {
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow_and_update() {
                        break;
                    }
                }
                _ = interval.tick() => {
                    let snapshot = task_state.probe_player_health(spotuify_core::now_ms()).await;
                    tracing::trace!(
                        connected = snapshot.connected,
                        consecutive_failures = snapshot.consecutive_failures,
                        "player health probe"
                    );
                }
            }
        }
    });
}

/// Audio-flow watchdog — catches "shows playing, no sound". The player-health
/// loop only probes session/TCP liveness, which stays `true` when the player
/// thread dies silently or the sink stalls (e.g. AirPods route failure). This
/// compares the clock's `is_playing` against the sink's PCM sample counter
/// (ground truth) and, on a sustained stall, reconciles the clock so it stops
/// lying and recovers through the shared reconnect throttle.
fn spawn_audio_flow_watchdog(state: Arc<DaemonState>) {
    use crate::state::{classify_audio_flow, AudioFlowVerdict};

    let task_state = state.clone();
    state.spawn_background("audio-watchdog", async move {
        let mut shutdown_rx = task_state.shutdown_receiver();
        let mut interval = tokio::time::interval(AUDIO_WATCHDOG_TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await; // skip the immediate first tick

        let mut last_samples: Option<u64> = None;
        let mut stalled_since_ms: Option<i64> = None;

        loop {
            tokio::select! {
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow_and_update() {
                        break;
                    }
                }
                _ = interval.tick() => {
                    let now_ms = spotuify_core::now_ms();
                    let playback = task_state.snapshot_playback();
                    // Playback on another Connect device (phone, car) also
                    // reads as "clock playing, local sink flat" — that is not
                    // a stall, and recovering would steal the session from
                    // that device (observed 2026-06-29). Watchdog is inert
                    // unless the active device is (or may be) ours.
                    let is_playing = playback.is_playing
                        && task_state.embedded_owns_global_transport()
                        && !task_state.active_device_is_foreign(&playback);
                    let samples = task_state.audio_samples();
                    let stalled_for_ms = stalled_since_ms.map_or(0, |s| now_ms.saturating_sub(s));
                    match classify_audio_flow(
                        is_playing,
                        samples,
                        last_samples,
                        stalled_for_ms,
                        AUDIO_STALL_THRESHOLD_MS,
                    ) {
                        AudioFlowVerdict::Flowing | AudioFlowVerdict::NotPlaying => {
                            stalled_since_ms = None;
                            task_state.record_audio_flow(true, None);
                        }
                        AudioFlowVerdict::Buffering => {
                            // Start/continue the grace timer; take no action yet.
                            stalled_since_ms.get_or_insert(now_ms);
                        }
                        AudioFlowVerdict::Stalled => {
                            tracing::warn!(
                                "audio-flow watchdog: clock playing but sink not advancing; reconciling + recovering"
                            );
                            task_state.record_audio_flow(false, Some(now_ms));
                            if task_state.playback_clock().mark_audio_stalled(now_ms) {
                                task_state.viz_coordinator().set_playing(false);
                                task_state.emit_event(DaemonEvent::PlaybackChanged {
                                    action: "audio_stalled".to_string(),
                                    playback: Some(task_state.snapshot_playback()),
                                });
                            }
                            task_state.trigger_audio_stall_recovery(now_ms);
                            // Fire once per stall, not every tick.
                            stalled_since_ms = None;
                        }
                    }
                    last_samples = samples;
                }
            }
        }
    });
}

fn spawn_auth_health_loop(state: Arc<DaemonState>) {
    let task_state = state.clone();
    state.spawn_background("auth-health", async move {
        let mut shutdown_rx = task_state.shutdown_receiver();
        let mut interval = tokio::time::interval(AUTH_HEALTH_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow_and_update() {
                        break;
                    }
                }
                _ = interval.tick() => {
                    match task_state.refresh_auth_health().await {
                        Ok(()) => tracing::trace!("auth health probe succeeded"),
                        Err(err) => tracing::debug!(error = %err, "auth health probe failed"),
                    }
                }
            }
        }
    });
}

/// Bail out of the initial cache warm on a provider rate-limit error.
/// Used to short-circuit subsequent warm steps after the first 429 —
/// otherwise startup fires the whole burst (4+ requests in <1s) at an
/// already-throttled account and the rolling window can't drain.
fn warm_bail_on_rate_limit(err: &ProviderError) -> bool {
    if matches!(err, ProviderError::RateLimited { .. }) {
        tracing::debug!(
            error = %err,
            "initial cache warm aborted: provider rate-limited; deferring to background sync"
        );
        true
    } else {
        false
    }
}

/// Fire a single round of cache-warming probes against Spotify as
/// soon as the daemon comes up. The handlers themselves never block
/// on Spotify any more, so without this the first PlaybackGet right
/// after `spotuify` launches would always render a synthetic
/// last-played (or an empty Playback on a truly fresh install) and
/// the user would only see real state on the next sync tick. This
/// warm-up makes the common case — daemon already running, TUI just
/// opened — feel instant. Failures (no auth yet, no network) are
/// silent; the regular sync scheduler retries on its 60s cadence.
fn spawn_initial_cache_warm(state: Arc<DaemonState>) {
    let task_state = state.clone();
    state.spawn_background("initial-cache-warm", async move {
        // Run the four fast domains sequentially through the sync engine's
        // bounded, abort-on-drop, provider-locking surface. This keeps warm
        // behavior identical to scheduled sync without a second copy of the
        // provider/store/event logic.
        let Ok(provider) = task_state.default_provider().await else {
            tracing::debug!("initial cache warm skipped: default provider unavailable");
            return;
        };
        let provider_id = provider.id().as_str().to_string();
        match task_state
            .store()
            .provider_rate_limit_max_cooldown_remaining_ms(&provider_id)
            .await
        {
            Ok(Some(remaining_ms)) => {
                tracing::debug!(
                    provider = provider_id,
                    remaining_ms,
                    "initial cache warm skipped: persisted provider cooldown active"
                );
                return;
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(
                    provider = provider_id,
                    error = %err,
                    "failed to inspect persisted cooldown before initial cache warm"
                );
                return;
            }
        }
        let transport = task_state.default_transport().await.ok();
        let Ok(sync_provider) = spotuify_sync::SyncProvider::new(provider, transport) else {
            tracing::warn!(
                provider = provider_id,
                "initial cache warm provider is invalid"
            );
            return;
        };
        for target in [
            spotuify_protocol::SyncTargetData::Playback,
            spotuify_protocol::SyncTargetData::Queue,
            spotuify_protocol::SyncTargetData::Devices,
            spotuify_protocol::SyncTargetData::Recent,
        ] {
            if let Err(err) = spotuify_sync::sync_provider_target_bounded(
                task_state.clone(),
                sync_provider.clone(),
                target,
            )
            .await
            {
                if err
                    .downcast_ref::<ProviderError>()
                    .is_some_and(warm_bail_on_rate_limit)
                {
                    return;
                }
                tracing::debug!(
                    provider = provider_id,
                    target = target.label(),
                    error = %err,
                    "initial cache warm target failed"
                );
            }
        }
    });
}

fn spawn_processing_mutation_recovery_retry(state: Arc<DaemonState>) {
    let task_state = state.clone();
    let mut shutdown_rx = state.shutdown_receiver();
    state.spawn_background("processing-mutation-recovery", async move {
        let mut delay = Duration::from_secs(1);
        loop {
            tokio::select! {
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow() {
                        return;
                    }
                }
                _ = tokio::time::sleep(delay) => {}
            }
            match recover_provider_mutation_lifecycle_after_startup(&task_state).await {
                Ok(count) => {
                    if count > 0 {
                        tracing::warn!(
                            count,
                            "recovered in-flight mutation claims after provider topology retry"
                        );
                    }
                    return;
                }
                Err(error) => {
                    tracing::warn!(%error, "provider mutation lifecycle recovery retry failed");
                    delay = delay.saturating_mul(2).min(Duration::from_secs(60));
                }
            }
        }
    });
}

async fn recover_provider_mutation_lifecycle_after_startup(
    state: &Arc<DaemonState>,
) -> Result<u64> {
    state
        .providers()
        .await
        .context("provider topology unavailable for mutation recovery")?;
    let (recovered, failed) = crate::handler::recover_processing_mutations(state)
        .await
        .context("failed to recover in-flight mutation claims")?;
    state
        .recover_pending_receipts_after_startup()
        .await
        .context("failed to recover pending mutation receipts")?;
    crate::handler::resume_provider_reconciliations(state)
        .await
        .context("failed to resume provider reconciliations")?;
    if failed > 0 {
        anyhow::bail!("{failed} in-flight mutation claim(s) still require recovery");
    }
    Ok(recovered)
}

fn spawn_media_control_command_loop(state: Arc<DaemonState>) -> Option<JoinHandle<()>> {
    if !state.system_integration.has_media_controls() {
        return None;
    }
    let system = state.system_integration.clone();
    let mut shutdown_rx = state.shutdown_receiver();
    Some(tokio::spawn(async move {
        loop {
            tokio::select! {
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow_and_update() {
                        break;
                    }
                }
                command = system.recv_media_control_command() => {
                    let Some(command) = command else {
                        break;
                    };
                    let response = handle_request_with_source(
                        state.clone(),
                        Request::PlaybackCommand { command },
                        Some(OperationSource::DaemonInternal),
                    )
                    .await;
                    if let Response::Error { message, .. } = response {
                        tracing::warn!(error = %message, "media-control playback command failed");
                    }
                }
            }
        }
    }))
}

async fn serve_client_connection(
    stream: IpcStream,
    state: Arc<DaemonState>,
    request_semaphore: Arc<Semaphore>,
    transport_semaphore: Arc<Semaphore>,
    mut event_rx: tokio::sync::broadcast::Receiver<IpcMessage>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    let (mut sink, mut stream) = Framed::new(stream, IpcCodec::new()).split();
    let mut request_tasks: JoinSet<IpcMessage> = JoinSet::new();
    let mut accept_requests = true;
    let mut can_send = true;
    let mut events_subscribed = false;
    let mut provider_policy_capable = false;
    let mut shutdown_requested = false;

    loop {
        let mut enable_event_subscription = None;
        tokio::select! {
            biased;
            joined = request_tasks.join_next(), if !request_tasks.is_empty() => {
                match joined {
                    Some(Ok(response)) if can_send => {
                        let id = response.id;
                        if let Err(err) = sink.send(response).await {
                            // The send failed. Most often this is the encoder
                            // rejecting an un-encodable response payload (a
                            // serde error mapped to io::Error), not a dead
                            // socket — and this site historically swallowed
                            // the error and silently tore the connection down,
                            // which is how the GetDoctorReport response failure
                            // stayed invisible. Log it, then try to inform the
                            // client with a typed error for this id so a single
                            // bad response cannot kill a long-lived session.
                            tracing::error!(
                                request_id = id,
                                error = %err,
                                "failed to send IPC response; replying with a typed error",
                            );
                            let fallback = IpcMessage {
                                id,
                                source: None,
                                mutation_id: None,
                                payload: IpcPayload::Response(Response::error_with_kind(
                                    "daemon failed to encode the response",
                                    IpcErrorKind::Internal,
                                )),
                            };
                            if sink.send(fallback).await.is_err() {
                                // The fallback failed too — the socket itself
                                // is gone, so tear the connection down.
                                can_send = false;
                                accept_requests = false;
                            }
                        }
                    }
                    Some(Ok(_response)) => {}
                    Some(Err(err)) => tracing::warn!(error = %err, "IPC request task failed"),
                    _ => {}
                }
            }
            changed = shutdown_rx.changed(), if !shutdown_requested => {
                match changed {
                    Ok(()) if *shutdown_rx.borrow_and_update() => {
                        shutdown_requested = true;
                        accept_requests = false;
                    }
                    Ok(()) => {}
                    Err(_) => {
                        shutdown_requested = true;
                        accept_requests = false;
                    }
                }
            }
            event = event_rx.recv(), if events_subscribed && can_send => {
                match event {
                    Ok(event) => {
                        let Some(event) = event_for_subscriber(event, provider_policy_capable) else {
                            continue;
                        };
                        if let Err(err) = sink.send(event).await {
                            tracing::error!(
                                error = %err,
                                "failed to send IPC event; closing connection",
                            );
                            can_send = false;
                            accept_requests = false;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        // Broadcast buffer overflowed for this subscriber.
                        // Push-state clients (TUI) need to re-seed because
                        // they just missed `skipped` events. Forward as a
                        // synthetic event so the client can react.
                        let lagged_msg = spotuify_protocol::IpcMessage {
                            id: 0,
                            source: None,
                            mutation_id: None,
                            payload: spotuify_protocol::IpcPayload::Event(
                                spotuify_protocol::DaemonEvent::EventStreamLagged { skipped },
                            ),
                        };
                        if let Err(err) = sink.send(lagged_msg).await {
                            tracing::error!(
                                error = %err,
                                "failed to send lagged-event marker; closing connection",
                            );
                            can_send = false;
                            accept_requests = false;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        // The daemon's broadcast sender is gone (shutdown).
                        // We can no longer push events, and leaving the
                        // connection half-open would silently drop the
                        // responses to any further requests — the
                        // `join_next` arm only sends while `can_send`, so
                        // completed responses fall through to the no-op arm
                        // and the client hangs. Tear the connection down so
                        // a push-state client reconnects and re-seeds.
                        can_send = false;
                        accept_requests = false;
                    }
                }
            }
            message = stream.next(), if accept_requests => {
                match message {
                    Some(Ok(message)) => {
                        if !events_subscribed {
                            if let IpcPayload::Request(Request::SubscribeEvents {
                                provider_policy,
                            }) = &message.payload
                            {
                                enable_event_subscription = Some(*provider_policy);
                            }
                        }
                        // Pick the fast lane for transport-style work
                        // so a saturated background lane (sync burst,
                        // doctor sweep, mass library refresh) can't
                        // delay a Pause / Resume / Seek.
                        let semaphore = if is_transport_request(&message.payload) {
                            transport_semaphore.clone()
                        } else {
                            request_semaphore.clone()
                        };
                        let Ok(permit) = semaphore.acquire_owned().await else {
                            accept_requests = false;
                            continue;
                        };
                        let state = state.clone();
                        request_tasks.spawn(async move {
                            let _permit = permit;
                            guard_ipc_response(
                                message.id,
                                state,
                                message.payload,
                                message.source,
                                message.mutation_id,
                            )
                            .await
                        });
                    }
                    Some(Err(err)) => {
                        tracing::warn!(error = %err, "failed to read IPC frame");
                        accept_requests = false;
                    }
                    None => {
                        accept_requests = false;
                    }
                }
            }
            else => break,
        }

        if let Some(supports_provider_policy) = enable_event_subscription {
            // Start a fresh receiver so events broadcast before opt-in are not replayed.
            event_rx = state.event_tx.subscribe();
            events_subscribed = true;
            provider_policy_capable = supports_provider_policy;
            // Push current state directly to this subscriber BEFORE
            // it sees any broadcast events. Eliminates the seed-race
            // window where `spawn_initial_cache_warm` emitted
            // `PlaybackChanged` before the client subscribed and the
            // client then sat blank until the next state change.
            if can_send {
                let snapshot = build_subscribe_snapshot(&state, provider_policy_capable).await;
                for msg in snapshot {
                    if let Err(err) = sink.send(msg).await {
                        tracing::error!(
                            error = %err,
                            "failed to send subscribe snapshot; closing connection",
                        );
                        can_send = false;
                        accept_requests = false;
                        break;
                    }
                }
            }
        }

        if !accept_requests && request_tasks.is_empty() {
            break;
        }
    }
}

/// Build the current-state events to push to a freshly-subscribed client,
/// including active provider-policy notices. Action is tagged `"snapshot"` so handlers can
/// distinguish a re-render-after-subscribe from a real change. Errors
/// from the underlying store reads degrade to defaults rather than
/// stalling the subscribe handshake.
fn event_for_subscriber(
    mut message: IpcMessage,
    provider_policy_capable: bool,
) -> Option<IpcMessage> {
    message.payload = match message.payload {
        IpcPayload::Event(event) => IpcPayload::Event(
            spotuify_protocol::daemon_event_for_subscriber(event, provider_policy_capable)?,
        ),
        payload => payload,
    };
    Some(message)
}

async fn build_subscribe_snapshot(
    state: &Arc<DaemonState>,
    provider_policy_capable: bool,
) -> Vec<IpcMessage> {
    use spotuify_sync::SyncContext;
    let provider = match state.active_transport_provider() {
        Some(provider) => Some(provider),
        None => state
            .providers()
            .await
            .ok()
            .map(|providers| providers.default_id().clone()),
    };
    // Provider construction validates any durable playback owner before the
    // clock is seeded, so removed adapters cannot leak into this snapshot.
    let playback = state.snapshot_playback();
    let queue = match provider.as_ref() {
        Some(provider) => SyncContext::snapshot_queue(state.as_ref(), provider).await,
        None => Default::default(),
    };
    let devices = match provider.as_ref() {
        Some(provider) => SyncContext::snapshot_devices(state.as_ref(), provider).await,
        None => Vec::new(),
    };
    let mk = |event: spotuify_protocol::DaemonEvent| IpcMessage {
        id: 0,
        source: None,
        mutation_id: None,
        payload: IpcPayload::Event(event),
    };
    let mut snapshot = vec![
        mk(spotuify_protocol::DaemonEvent::PlaybackChanged {
            action: "snapshot".to_string(),
            playback: Some(playback),
        }),
        mk(spotuify_protocol::DaemonEvent::QueueChanged {
            action: "snapshot".to_string(),
            uris: queue.items.iter().map(|i| i.uri.clone()).collect(),
            queue: Some(queue),
        }),
        mk(spotuify_protocol::DaemonEvent::DevicesChanged {
            action: "snapshot".to_string(),
            devices: Some(devices),
        }),
    ];
    // Redeliver the first-party-only migration advisory to THIS subscriber.
    // The broadcast emit is latched once-per-run, so a TUI / macOS app that
    // connects after daemon startup would otherwise miss it. Per-subscribe
    // (not per-event), so an already-subscribed client is never re-spammed.
    let auth_provider = state
        .configured_health_auth_target()
        .await
        .ok()
        .filter(|target| {
            target.strategy == crate::provider_factory::ProviderAuthStrategy::SpotifyOauth
        })
        .map(|target| target.provider_id);
    let auth_advisory = match auth_provider.as_ref() {
        Some(provider) => state.auth_migration_advisory(provider).await,
        None => None,
    };
    if let Some(can_login_dev_app) = auth_advisory {
        snapshot.push(mk(
            spotuify_protocol::DaemonEvent::AuthMigrationRecommended { can_login_dev_app },
        ));
    }
    snapshot.extend(state.active_provider_policies().into_iter().map(|policy| {
        mk(spotuify_protocol::DaemonEvent::ProviderPolicy {
            provider: policy.provider,
            reason: policy.reason,
        })
    }));
    snapshot
        .into_iter()
        .filter_map(|event| event_for_subscriber(event, provider_policy_capable))
        .collect()
}

/// Returns `true` when the inbound IPC payload should be routed to
/// the fast-lane transport semaphore. The taxonomy is "anything the
/// user perceives latency on": transport mutations, the now-playing
/// snapshot read that drives the TUI's frame, device list / queue
/// reads that mid-mutation reconciliation depends on, and the
/// subscribe handshake itself. Background queries (search,
/// analytics, ops log, cache status, doctor) intentionally fall back
/// to the slow lane.
fn is_transport_request(payload: &IpcPayload) -> bool {
    let IpcPayload::Request(req) = payload else {
        return false;
    };
    matches!(
        req,
        Request::PlaybackCommand { .. }
            | Request::PlaybackGet
            | Request::DeviceTransfer { .. }
            | Request::DevicesList
            | Request::QueueAdd { .. }
            | Request::QueueGet
            | Request::LibrarySave { .. }
            | Request::LibraryUnsave { .. }
            | Request::PlaylistAddItems { .. }
            | Request::PlaylistRemoveItems { .. }
            | Request::SubscribeEvents { .. }
            | Request::Ping
    )
}

async fn guard_ipc_response(
    message_id: u64,
    state: Arc<DaemonState>,
    payload: IpcPayload,
    source: Option<spotuify_protocol::OperationSource>,
    mutation_id: Option<spotuify_protocol::MutationId>,
) -> IpcMessage {
    use tracing::Instrument;

    let (request_kind, command_label, request_category) = match &payload {
        IpcPayload::Request(req) => (
            req.kind_label(),
            match req {
                Request::PlaybackCommand { command } => Some(command.label()),
                _ => None,
            },
            Some(req.category().label()),
        ),
        IpcPayload::Response(_) => ("response", None, None),
        IpcPayload::Event(_) => ("event", None, None),
    };

    // Bound every request at the daemon layer so a wedged handler
    // (stuck Spotify call, contended lock, hung provider) returns a
    // typed Timeout instead of pinning the connection task forever.
    let deadline = match &payload {
        IpcPayload::Request(req) => request_deadline(req),
        _ => DEFAULT_REQUEST_DEADLINE,
    };

    let span = tracing::info_span!(
        target: "spotuify_daemon::ipc",
        "ipc.request",
        request_id = message_id,
        request_kind = request_kind,
        command = command_label,
        category = request_category,
        source = source.as_ref().map_or("client", |s| s.label()),
        duration_ms = tracing::field::Empty,
        outcome = tracing::field::Empty,
        error_kind = tracing::field::Empty,
    );

    let started = std::time::Instant::now();
    let response = async move {
        match payload {
            IpcPayload::Request(request) => {
                let protected_mutation_id = request
                    .requires_mutation_id()
                    .then_some(mutation_id)
                    .flatten();
                let handler = AssertUnwindSafe(handle_request_with_source_and_mutation(
                    state.clone(),
                    request,
                    source,
                    protected_mutation_id,
                ))
                .catch_unwind();
                match tokio::time::timeout(deadline, handler).await {
                    Ok(Ok(response)) => response,
                    Ok(Err(_)) => match protected_mutation_id {
                        Some(id) => match crate::handler::recover_processing_mutation(&state, id)
                            .await
                        {
                            Ok(Some(response)) => response,
                            Ok(None) => Response::error_with_kind(
                                "IPC handler panicked before the mutation was claimed",
                                IpcErrorKind::Internal,
                            ),
                            Err(err) => Response::error_with_retryable(
                                format!("failed to persist indeterminate mutation outcome: {err}"),
                                IpcErrorKind::Internal,
                                false,
                            ),
                        },
                        None => Response::error_with_kind(
                            "IPC handler panicked",
                            IpcErrorKind::Internal,
                        ),
                    },
                    Err(_) => match protected_mutation_id {
                        Some(id) => match crate::handler::recover_processing_mutation(&state, id)
                            .await
                        {
                            Ok(Some(response)) => response,
                            Ok(None) => Response::error_with_kind(
                                "request timed out before the mutation was claimed",
                                IpcErrorKind::Timeout,
                            ),
                            Err(err) => Response::error_with_retryable(
                                format!("failed to persist indeterminate mutation outcome: {err}"),
                                IpcErrorKind::Internal,
                                false,
                            ),
                        },
                        None => Response::error_with_kind(
                            "request timed out in the daemon",
                            IpcErrorKind::Timeout,
                        ),
                    },
                }
            }
            _ => Response::error_with_kind(
                "IPC frame was not a request",
                IpcErrorKind::InvalidRequest,
            ),
        }
    }
    .instrument(span.clone())
    .await;

    let elapsed_ms = started.elapsed().as_millis() as u64;
    span.record("duration_ms", elapsed_ms);
    match &response {
        Response::Ok { .. } => {
            span.record("outcome", "ok");
        }
        Response::Error { kind, .. } => {
            span.record("outcome", "error");
            span.record("error_kind", kind.as_code());
        }
    }
    // Threshold-based escalation: slow IPCs become a warn-level event so they
    // surface in default-level log tails without burning a span on every request.
    if elapsed_ms >= SLOW_IPC_WARN_MS {
        tracing::warn!(
            target: "spotuify_daemon::ipc",
            request_id = message_id,
            request_kind = request_kind,
            duration_ms = elapsed_ms,
            "slow IPC request"
        );
    }

    IpcMessage {
        id: message_id,
        source: None,
        mutation_id: None,
        payload: IpcPayload::Response(response),
    }
}

/// Threshold above which an IPC handler is considered slow and warrants
/// a warn-level log event in addition to the per-request info span.
/// The daemon's hot-path target is sub-10ms (cache read + clock snapshot);
/// 250ms is a generous slop for warm caches + a single Spotify call.
const SLOW_IPC_WARN_MS: u64 = 250;

/// Wall-clock ceiling for a normal request. Well above the warm-cache +
/// one-Spotify-call path (seconds), so it only ever trips on a genuinely
/// wedged handler.
const DEFAULT_REQUEST_DEADLINE: Duration = Duration::from_secs(30);
/// Maintenance requests that legitimately run for minutes (full library
/// reindex, sync sweep, analytics rebuild). Still bounded so a hung
/// indexer can't pin a connection forever.
const MAINTENANCE_REQUEST_DEADLINE: Duration = Duration::from_secs(600);

/// Per-request wall-clock ceiling. Long-running maintenance gets a
/// generous cap; everything else is held to the tight default.
fn request_deadline(req: &Request) -> Duration {
    match req {
        Request::Reindex | Request::Sync { .. } | Request::AnalyticsRebuild { .. } => {
            MAINTENANCE_REQUEST_DEADLINE
        }
        _ => DEFAULT_REQUEST_DEADLINE,
    }
}

async fn drain_connection_tasks(connections: &mut JoinSet<()>, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    while !connections.is_empty() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        if tokio::time::timeout(remaining, connections.join_next())
            .await
            .is_err()
        {
            break;
        }
    }
    connections.abort_all();
}

async fn drain_background_tasks(mut tasks: Vec<JoinHandle<()>>, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    for mut task in tasks.drain(..) {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            task.abort();
            continue;
        }
        tokio::select! {
            result = &mut task => {
                if let Err(err) = result {
                    tracing::debug!(error = %err, "daemon background task ended during shutdown");
                }
            }
            _ = tokio::time::sleep(remaining) => {
                tracing::warn!("daemon background task shutdown timed out; aborting task");
                task.abort();
                let _ = task.await;
            }
        }
    }
}

/// Start the daemon. `foreground` runs the serve loop in-process (the
/// `daemon start --foreground` subcommand, and the child the launcher
/// spawns). Otherwise the client-side launcher spawns + supervises a
/// detached daemon process.
pub async fn start_daemon(foreground: bool) -> Result<Option<DaemonStatus>> {
    if foreground {
        run_daemon().await?;
        return Ok(None);
    }
    spotuify_launcher::start_daemon_background().await
}

/// Local audio output device names the embedded player can render to,
/// for the TUI/CLI output picker. Enumerated in-process via the same
/// cpal host librespot matches against. Empty when embedded playback
/// isn't compiled in.
pub fn list_audio_outputs() -> Vec<String> {
    #[cfg(feature = "embedded-playback")]
    {
        spotuify_player::list_audio_outputs()
    }
    #[cfg(not(feature = "embedded-playback"))]
    {
        Vec::new()
    }
}

/// Acquire the per-instance daemon startup lock. The lock file sits in
/// the instance's 0700 runtime dir so dev and prod instances never
/// contend. A non-blocking exclusive `flock` means a second `daemon
/// start` for the same instance fails fast instead of racing the socket
/// claim. The returned `File` must outlive the daemon: dropping it
/// (process exit or crash) releases the lock.
fn acquire_startup_lock(runtime_dir: &Path) -> Result<std::fs::File> {
    use fs2::FileExt;

    // The lock lives in the instance's runtime dir — a real directory on
    // every platform. Deriving it from the socket path breaks on Windows,
    // where the socket is a named pipe (`\\.\pipe\…`) and `with_file_name`
    // would yield `\\.\pipe\daemon.lock`, which cannot be opened as a
    // regular file.
    let lock_path = runtime_dir.join("daemon.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("failed to open daemon lock {}", lock_path.display()))?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(file),
        Err(_) => anyhow::bail!(
            "another spotuify daemon is starting or running for this instance \
             (startup lock {} is held). Try `spotuify daemon status`.",
            lock_path.display()
        ),
    }
}

#[cfg(unix)]
fn remove_bound_socket(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(not(unix))]
fn remove_bound_socket(_path: &Path) {}

fn write_daemon_pid_file() -> Result<()> {
    let pid_path = DaemonState::pid_path();
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(pid_path, std::process::id().to_string())?;
    Ok(())
}

/// Test-only watchdog: exit the daemon when the process named by
/// `SPOTUIFY_EXIT_WITH_PARENT` dies. Test harnesses set it to their own
/// PID so a daemon they auto-started can't outlive a killed
/// `cargo test`/`nextest` run and orphan itself. Unset in real use, so
/// this is inert for dev/prod daemons (which must survive the launching
/// CLI). Uses a platform process probe to avoid unsafe FFI under the
/// workspace's `deny(unsafe_code)`.
fn spawn_parent_death_watchdog() {
    let Some(parent_pid) = std::env::var("SPOTUIFY_EXIT_WITH_PARENT")
        .ok()
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        .filter(|pid| *pid > 1)
    else {
        return;
    };
    tracing::info!(parent_pid, "parent-death watchdog armed (test mode)");
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(3));
        if !process_is_alive(parent_pid) {
            tracing::warn!(
                parent_pid,
                "parent process gone — exiting to avoid orphaning the daemon"
            );
            std::process::exit(0);
        }
    });
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(not(unix))]
fn process_is_alive(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    system.process(pid).is_some()
}

/// Kill an orphaned daemon only when it is named by this instance's
/// pidfile. A broad `ps | grep spotuify daemon` cleanup can kill a
/// separately installed prod daemon when a local dev build starts.
fn cleanup_zombie_daemons() {
    let me = std::process::id();
    let Some(pid) = read_daemon_pid_file(&DaemonState::pid_path()) else {
        return;
    };
    if pid == me || !is_orphaned_spotuify_daemon(pid) {
        return;
    }
    tracing::warn!(
        pid,
        pid_file = %DaemonState::pid_path().display(),
        "preflight: killing orphaned daemon for current spotuify instance"
    );
    let _ = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let _ = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status();
}

fn read_daemon_pid_file(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| raw.trim().parse::<u32>().ok())
}

fn is_orphaned_spotuify_daemon(target_pid: u32) -> bool {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-ax", "-o", "pid=,ppid=,command="])
        .output()
    else {
        return false;
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let mut parts = line.trim_start().splitn(3, char::is_whitespace);
        let Some(pid_str) = parts.next() else {
            continue;
        };
        let Some(ppid_str) = parts.next() else {
            continue;
        };
        let Some(cmd) = parts.next() else { continue };
        let Ok(pid) = pid_str.trim().parse::<u32>() else {
            continue;
        };
        let Ok(ppid) = ppid_str.trim().parse::<u32>() else {
            continue;
        };
        if pid != target_pid {
            continue;
        }
        return ppid == 1 && cmd.contains("spotuify") && cmd.contains("daemon");
    }
    false
}

/// Tantivy uses two `.tantivy-*.lock` files in the index directory.
/// Filesystem locks (fcntl/flock) are released when a process dies,
/// but the lock files persist on disk and can confuse the next
/// IndexWriter into reporting `LockBusy`. After we've killed any
/// stray daemons above, removing the files is safe — no live
/// process holds them.
///
/// The removal is deliberately not fsynced: if a hard crash resurrects
/// the file, this preflight runs again on the next start and removes
/// it then, so durability buys nothing here.
fn clear_stale_tantivy_locks() {
    let Ok(index_dir) = spotuify_store::search_index_path() else {
        return;
    };
    for name in [".tantivy-writer.lock", ".tantivy-meta.lock"] {
        let path = index_dir.join(name);
        if path.exists() {
            tracing::warn!(path = %path.display(), "preflight: removing stale tantivy lock");
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Phase 12 (P12.7) — background retention loop.
///
/// Wakes once a day and prunes:
/// - `operations` older than 90d
/// - `playback_progress` older than 90d
/// - `analytics_events` older than 365d
///
/// First tick is delayed one period to keep daemon startup fast; a
/// one-shot prune fires immediately so a freshly-started daemon
/// catches up on retention as soon as the socket is listening.
fn spawn_retention_loop(state: Arc<DaemonState>) -> JoinHandle<()> {
    // Retention is the canonical bulk-background job — it deletes
    // hundreds-to-thousands of rows from operations / events /
    // playback_progress at a daily cadence. Running it on the
    // dedicated bg runtime means even when retention is mid-DELETE
    // the main runtime's workers are free for IPC + handler dispatch.
    let bg_handle = state.bg_runtime_handle();
    let state_for_task = state;
    bg_handle.spawn(async move {
        let state = state_for_task;
        let mut shutdown_rx = state.shutdown_receiver();
        // One-shot startup pass — keeps long-idle databases bounded
        // without waiting 24h after the user reopens spotuify.
        run_retention_once(&state).await;

        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(86_400));
        // First tick fires immediately (the one-shot already ran);
        // skip it so the next real tick happens 24h later.
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = ticker.tick() => run_retention_once(&state).await,
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow_and_update() {
                        break;
                    }
                }
            }
        }
    })
}

async fn run_retention_once(state: &DaemonState) {
    let now = spotuify_core::now_ms();
    let analytics = spotuify_config::load()
        .ok()
        .map(|loaded| loaded.config.analytics);
    let cutoffs = retention_cutoffs(now, analytics.as_ref());
    match state.store().prune_expired_mutations(now).await {
        Ok(n) if n > 0 => tracing::info!(rows = n, "pruned expired mutation dedup rows"),
        Ok(_) => {}
        Err(err) => tracing::warn!(error = %err, "mutation dedup retention prune failed"),
    }
    match state
        .store()
        .prune_operations_older_than(cutoffs.operations_ms)
        .await
    {
        Ok(n) if n > 0 => tracing::info!(rows = n, "pruned operations rows past retention"),
        Ok(_) => {}
        Err(err) => tracing::warn!(error = %err, "ops retention prune failed"),
    }
    match state
        .store()
        .prune_playback_progress(cutoffs.progress_ms)
        .await
    {
        Ok(n) if n > 0 => {
            tracing::info!(rows = n, "pruned playback_progress rows past retention")
        }
        Ok(_) => {}
        Err(err) => tracing::warn!(error = %err, "progress retention prune failed"),
    }
    let events_prune = match AnalyticsStore::open_default().await {
        Ok(store) => store.prune_events_older_than(cutoffs.events_ms).await,
        Err(err) => Err(err),
    };
    match events_prune {
        Ok(n) if n > 0 => {
            tracing::info!(rows = n, "pruned analytics_events rows past retention")
        }
        Ok(_) => {}
        Err(err) => tracing::warn!(error = %err, "events retention prune failed"),
    }
}

/// Update-awareness loop. Checks the GitHub releases API ~10s after startup
/// (keeps startup snappy) and then every 6h. Runs on the dedicated bg runtime
/// so a slow network call never starves IPC/handler workers. No-op when
/// `SPOTUIFY_NO_UPDATE_CHECK` is set.
fn spawn_update_loop(state: Arc<DaemonState>) -> JoinHandle<()> {
    let bg_handle = state.bg_runtime_handle();
    bg_handle.spawn(async move {
        if crate::update::update_check_disabled() {
            tracing::debug!("update check disabled via SPOTUIFY_NO_UPDATE_CHECK");
            return;
        }
        let mut shutdown_rx = state.shutdown_receiver();
        // Brief startup delay so the first check doesn't compete with sync.
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {}
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow_and_update() {
                    return;
                }
            }
        }
        run_update_check_once(&state).await;

        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(6 * 3600));
        ticker.tick().await; // consume the immediate first tick
        loop {
            tokio::select! {
                _ = ticker.tick() => run_update_check_once(&state).await,
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow_and_update() {
                        break;
                    }
                }
            }
        }
    })
}

/// macOS only — "follow the system default audio output" watcher.
///
/// When the user has NOT pinned a specific output device
/// (`player.audio_output_device` unset = follow the system default) and changes
/// their Mac's default output (Sound settings, Control Center, plugging in
/// headphones), rebuild the embedded player so audio re-routes to the new
/// device. Polls every 2s — cheap, no extra deps, ~2s latency. Gated on
/// `we_are_active` so it never disrupts playback that's on another device, and
/// it only reconnects on an actual default-device change (an intentional user
/// action), keeping playback reliability intact.
#[cfg(target_os = "macos")]
fn spawn_audio_follow_loop(state: Arc<DaemonState>) -> JoinHandle<()> {
    let bg_handle = state.bg_runtime_handle();
    bg_handle.spawn(async move {
        let mut shutdown_rx = state.shutdown_receiver();
        let mut last = spotuify_player::current_default_output_name();
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(2));
        ticker.tick().await; // consume the immediate first tick
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let following = state
                        .accepted_player_settings()
                        .audio_output_device
                        .is_none();
                    let current = spotuify_player::current_default_output_name();
                    if !following {
                        // Pinned to a specific device — don't follow; keep the
                        // baseline current so a later un-pin doesn't false-fire.
                        last = current;
                        continue;
                    }
                    if current != last {
                        let new_default = current.clone();
                        last = current;
                        if new_default.is_some() && state.is_we_are_active() {
                            tracing::info!(
                                device = ?new_default,
                                "system default output changed; re-routing embedded player"
                            );
                            let name = state.configured_device_name();
                            if let Err(err) = state.reconnect_player(&name).await {
                                tracing::warn!(error = %err, "audio-follow reconnect failed");
                            }
                        }
                    }
                }
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow_and_update() {
                        break;
                    }
                }
            }
        }
    })
}

/// One update check: fetch the latest release, cache it, and emit
/// `UpdateAvailable` the first time a newer version is seen. Called by the
/// background loop and (with `force`) by `Request::CheckUpdate`.
pub(crate) async fn run_update_check_once(state: &DaemonState) {
    let (latest, url) = match crate::update::fetch_latest_release().await {
        Ok(pair) => pair,
        // Offline / rate-limited / API hiccup: keep the previous cache, no nag.
        Err(err) => {
            tracing::debug!(error = %err, "update check failed (ignored)");
            return;
        }
    };
    let current = current_daemon_version();
    let newer = crate::update::is_newer(current, &latest);
    let first_sighting = state
        .cached_release()
        .map(|prev| prev.latest_version != latest)
        .unwrap_or(true);
    state.set_cached_release(crate::update::CachedRelease {
        latest_version: latest.clone(),
        release_url: url.clone(),
        checked_at_ms: spotuify_core::now_ms(),
    });
    if newer && first_sighting {
        let method = crate::update::detect_upgrade_method(&crate::update::current_exe_path());
        let upgrade = crate::update::upgrade_hint(method, &latest, url.as_deref());
        tracing::info!(latest = %latest, "newer spotuify release available");
        state.emit_event(DaemonEvent::UpdateAvailable {
            latest_version: latest,
            release_url: url,
            upgrade,
        });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::Arc;

    use spotuify_core::{Device, MediaItem, MediaKind, MusicProvider as _, Queue};
    use spotuify_provider_fake::FakeProvider;

    use crate::provider_registry::{ProviderRegistry, ProviderRuntime};

    use super::*;

    struct TestEnv {
        _temp: tempfile::TempDir,
    }

    impl TestEnv {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("tempdir");
            std::env::set_var("SPOTUIFY_FAKE_SPOTIFY", "1");
            std::env::set_var("SPOTUIFY_CACHE_DB", temp.path().join("cache.sqlite3"));
            std::env::set_var("SPOTUIFY_SEARCH_INDEX", temp.path().join("search-index"));
            std::env::set_var("SPOTUIFY_RUNTIME_DIR", temp.path().join("runtime"));
            std::env::set_var("SPOTUIFY_DATA_DIR", temp.path().join("data"));
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
            ] {
                std::env::remove_var(key);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn startup_lock_is_exclusive_then_reacquirable() {
        let dir = tempfile::tempdir().expect("tempdir");

        let first = acquire_startup_lock(dir.path()).expect("first daemon wins the startup lock");
        // A second concurrent `daemon start` for the same instance must
        // fail fast instead of racing the socket claim.
        assert!(
            acquire_startup_lock(dir.path()).is_err(),
            "second concurrent start must not also win the lock"
        );

        // Once the holder exits (lock released), a fresh start reacquires.
        drop(first);
        assert!(
            acquire_startup_lock(dir.path()).is_ok(),
            "lock must be reacquirable after the previous holder releases it"
        );
    }

    #[tokio::test]
    async fn subscribe_snapshot_uses_secondary_active_transport_partition() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let default = Arc::new(FakeProvider::isolated("snapshot-default").unwrap());
        let secondary = Arc::new(FakeProvider::isolated("snapshot-secondary").unwrap());
        let registry = ProviderRegistry::new(
            default.id().clone(),
            [
                ProviderRuntime::with_transport(default).unwrap(),
                ProviderRuntime::with_transport(secondary.clone()).unwrap(),
            ],
        )
        .unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let provider = secondary.id().clone();
        let queue_uri = "snapshot-secondary:track:queued";
        state
            .store()
            .persist_provider_queue(
                &provider,
                &Queue {
                    items: vec![MediaItem {
                        uri: queue_uri.to_string(),
                        name: "Secondary queue item".to_string(),
                        kind: MediaKind::Track,
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        state
            .store()
            .replace_provider_devices(
                &provider,
                &[Device {
                    id: Some("secondary-device".to_string()),
                    name: "Secondary device".to_string(),
                    kind: "Computer".to_string(),
                    is_active: true,
                    is_restricted: false,
                    volume_percent: Some(50),
                    supports_volume: true,
                }],
            )
            .await
            .unwrap();
        state.set_active_transport_provider(provider);

        let snapshot = build_subscribe_snapshot(&state, true).await;
        assert!(snapshot.iter().any(|message| matches!(
            &message.payload,
            IpcPayload::Event(DaemonEvent::QueueChanged { queue: Some(queue), .. })
                if queue.items.iter().any(|item| item.uri == queue_uri)
        )));
        assert!(snapshot.iter().any(|message| matches!(
            &message.payload,
            IpcPayload::Event(DaemonEvent::DevicesChanged { devices: Some(devices), .. })
                if devices.iter().any(|device| device.id.as_deref() == Some("secondary-device"))
        )));

        state.shutdown_search().await;
        state.shutdown_player().await;
    }
}
