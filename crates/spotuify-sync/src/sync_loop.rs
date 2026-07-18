//! Phase 7 — sync engine. Moved out of the binary's `src/sync.rs`
//! once the `SyncContext` trait broke the cycle with `DaemonState`.
//!
//! All public functions are generic over `&impl SyncContext`. The
//! binary's wrapper supplies an `Arc<DaemonState>` (which impls
//! `SyncContext`) and the sync loop runs against the daemon's live
//! Spotify client, store, and event broadcaster -- no longer
//! compile-coupled to the daemon module.

use std::any::Any;
use std::collections::HashMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::Result;
use futures::FutureExt;
use spotuify_core::{
    now_ms, AccessOutcome, AccessUnavailable, CollectionRequest, FreshnessProbe, ItemSource,
    LibraryRequest, MediaItem, MediaKind, MusicProvider, PageContinuation, PageRequest, Playback,
    Playlist, ProviderError, ProviderId, ProviderPage, Queue, RemoteTransport, RequestContext,
    ResourceUri,
};
use spotuify_protocol::{
    CacheSyncSummary, DaemonEvent, ProviderSyncOutcome, SyncCompletionStatus, SyncTargetData,
};
use tokio::task::JoinHandle;

use crate::{should_refetch_playlist_tracks, SyncContext, SyncProvider};

/// Active poll cadence when at least one client is subscribed to
/// daemon events. Matches `spotify_player`'s 3s default — fast enough
/// that cross-device playback changes feel near-live, well under
/// Spotify's rate limits.
const ACTIVE_CADENCE: Duration = Duration::from_secs(3);
/// Devices and queue change rarely, and local mutations update the queue
/// optimistically, so they poll on slower lanes than playback. Polling
/// `/me/player/devices` every `ACTIVE_CADENCE` (3s) is what trips Spotify's
/// rate limiter over a long TUI session, so the devices lane is the slowest.
const QUEUE_CADENCE: Duration = Duration::from_secs(15);
const DEVICES_CADENCE: Duration = Duration::from_secs(60);
/// When our own embedded device is the active player, librespot's player
/// events already feed the daemon's clock live, so the Web API `/me/player`
/// poll is redundant. Downgrade it from every-tick to a slow
/// reconciliation that only exists to catch playback moving to another
/// device or an external shuffle/repeat change. This is the single
/// biggest reduction in Web API spend on the shared first-party token.
const PLAYBACK_RECONCILE_CADENCE: Duration = Duration::from_secs(30);
const SLOW_CADENCE: Duration = Duration::from_secs(15 * 60);
const SLOW_INITIAL_DELAY: Duration = Duration::from_secs(60);
const PER_TARGET_TIMEOUT: Duration = Duration::from_secs(10);
const SLOW_TARGET_TIMEOUT: Duration = Duration::from_secs(9 * 60);
const PROVIDER_DETACH_GRACE: Duration = Duration::from_secs(1);
const BACKOFF_BASE: Duration = Duration::from_secs(15);
const BACKOFF_MAX: Duration = Duration::from_secs(2 * 60);
/// Circuit breaker for the shared global cooldown. On consecutive 429s
/// the daemon escalates its pause well beyond Spotify's per-request
/// `Retry-After` (which stays a flat ~60s while an account is in an
/// escalated throttle). Probing every minute keeps that throttle alive;
/// backing off 1m → 2m → 4m → 8m → 10m lets the account decay and
/// recover on its own. Any successful poll resets the escalation.
const GLOBAL_BACKOFF_BASE: Duration = Duration::from_secs(60);
const GLOBAL_BACKOFF_MAX: Duration = Duration::from_secs(10 * 60);
const PROVIDER_RECONCILE_RETRY: Duration = Duration::from_secs(15);
const SCHEDULER_RESTART_BASE: Duration = Duration::from_secs(1);
const SCHEDULER_RESTART_MAX: Duration = Duration::from_secs(60);
const SCHEDULER_RESTART_RESET_AFTER: Duration = Duration::from_secs(5 * 60);

/// Recently played is useful for "last played" hints, but it is not
/// part of the live transport loop. Keep it on a slower cadence so a
/// slow or rate-limited endpoint cannot pin the hot poll path.
const RECENT_ACTIVE_CADENCE: Duration = Duration::from_secs(5 * 60);

/// Idle poll cadence when no client is subscribed. Keeps the cache
/// somewhat fresh for the next launch without burning API quota.
const IDLE_CADENCE: Duration = Duration::from_secs(30);

/// Tokio detaches a task when its `JoinHandle` is dropped. Sync work owns
/// provider calls and lane locks, so detaching would let cancelled requests or
/// daemon shutdown leak both. This wrapper aborts unless ownership is
/// explicitly transferred to the daemon's bounded drain path.
pub struct AbortOnDropTask<T> {
    handle: Option<JoinHandle<T>>,
}

impl<T> AbortOnDropTask<T> {
    fn new(handle: JoinHandle<T>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    fn abort(&self) {
        if let Some(handle) = self.handle.as_ref() {
            handle.abort();
        }
    }

    pub fn into_join_handle(mut self) -> JoinHandle<T> {
        self.handle
            .take()
            .expect("abort-on-drop task handle already taken")
    }
}

impl<T> Future for AbortOnDropTask<T> {
    type Output = Result<T, tokio::task::JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(
            self.handle
                .as_mut()
                .expect("abort-on-drop task handle already taken"),
        )
        .poll(cx)
    }
}

impl<T> Drop for AbortOnDropTask<T> {
    fn drop(&mut self) {
        self.abort();
    }
}

/// Spawn the background sync loop. Runs until the daemon's shutdown
/// signal fires.
///
/// Sync is the daemon's job, not a client's. Two cadences:
///
/// 1. **Fast cadence (3 s)** — playback / queue / devices. These
///    change as the user listens and need to feel live.
/// 2. **Slow cadence (15 min)** — playlists / library (saved
///    tracks + albums + subscribed shows). These rarely change and
///    paginating them takes seconds; doing them on the same 60 s tick
///    would hammer Spotify.
///
/// Recently played uses its own 5 min cadence inside the fast loop:
/// fresh enough for hints, far away from the transport hot path.
///
/// The daemon does a separate one-shot initial cache warm before the
/// socket starts accepting clients. The background loops wait for
/// their first scheduled tick so startup does not duplicate the same
/// Spotify reads and trip rate limits before the user presses play.
pub fn spawn_background_scheduler<C>(ctx: Arc<C>) -> Vec<AbortOnDropTask<()>>
where
    C: SyncContext + 'static,
{
    // Provider adapters commonly hold reqwest/hyper clients whose connection
    // drivers are pinned to the runtime where the client was first used.
    // Keep provider discovery and every scheduler lane on the caller's main
    // runtime; moving them to the host's auxiliary runtime can strand pooled
    // requests waiting on a driver owned by another runtime.
    let supervisor_future = async move {
        let mut shutdown_rx = ctx.shutdown_receiver();
        let mut revision_rx = ctx.sync_provider_revision_receiver();
        'reconcile: loop {
            let providers = {
                let providers = ctx.sync_providers();
                tokio::pin!(providers);
                tokio::select! {
                    _ = wait_for_shutdown(&mut shutdown_rx) => return,
                    revision = wait_for_provider_revision(&mut revision_rx) => {
                        if revision == ProviderRevisionSignal::Closed {
                            revision_rx = None;
                        }
                        continue 'reconcile;
                    }
                    result = &mut providers => result,
                }
            };
            let providers = match providers {
                Ok(providers) => providers,
                Err(err) => {
                    tracing::warn!(error = %err, "provider sync discovery failed; retrying");
                    tokio::select! {
                        _ = wait_for_shutdown(&mut shutdown_rx) => return,
                        revision = wait_for_provider_revision(&mut revision_rx) => {
                            if revision == ProviderRevisionSignal::Closed {
                                revision_rx = None;
                            }
                        }
                        _ = tokio::time::sleep(PROVIDER_RECONCILE_RETRY) => {}
                    }
                    continue;
                }
            };

            let mut tasks = tokio::task::JoinSet::new();
            let mut task_metadata = HashMap::new();
            for provider in providers {
                let mut persisted_backoff = TargetBackoff::default();
                match ctx
                    .store()
                    .provider_rate_limit_max_cooldown_remaining_ms(provider.id())
                    .await
                {
                    Ok(Some(remaining_ms)) => persisted_backoff.seed_persisted_cooldown(
                        tokio::time::Instant::now(),
                        Duration::from_millis(remaining_ms as u64),
                    ),
                    Ok(None) => {}
                    Err(err) => tracing::warn!(
                        provider = provider.id(),
                        error = %err,
                        "failed to seed provider rate-limit cooldown"
                    ),
                }
                let global_backoff = Arc::new(Mutex::new(persisted_backoff));
                spawn_scheduler_lane(
                    &mut tasks,
                    &mut task_metadata,
                    ctx.clone(),
                    SchedulerLaneState {
                        provider: provider.clone(),
                        lane: SchedulerLane::Fast,
                        global_backoff: global_backoff.clone(),
                        restart_failures: 0,
                    },
                );
                spawn_scheduler_lane(
                    &mut tasks,
                    &mut task_metadata,
                    ctx.clone(),
                    SchedulerLaneState {
                        provider,
                        lane: SchedulerLane::Slow,
                        global_backoff,
                        restart_failures: 0,
                    },
                );
            }

            loop {
                tokio::select! {
                    _ = wait_for_shutdown(&mut shutdown_rx) => {
                        stop_scheduler_lanes(&mut tasks).await;
                        return;
                    }
                    revision = wait_for_provider_revision(&mut revision_rx) => {
                        if revision == ProviderRevisionSignal::Closed {
                            revision_rx = None;
                            continue;
                        }
                        stop_scheduler_lanes(&mut tasks).await;
                        continue 'reconcile;
                    }
                    joined = tasks.join_next_with_id(), if !tasks.is_empty() => {
                        let Some(joined) = joined else {
                            continue;
                        };
                        let (state, error) = match joined {
                            Ok((task_id, (state, result))) => {
                                task_metadata.remove(&task_id);
                                (state, result.err())
                            }
                            Err(err) => {
                                let state = task_metadata.remove(&err.id());
                                let Some(state) = state else {
                                    tracing::error!(error = %err, "scheduler lane lost task metadata");
                                    continue;
                                };
                                (state, Some(format!("scheduler lane task failed: {err}")))
                            }
                        };
                        if scheduler_shutdown_requested(&shutdown_rx) {
                            continue;
                        }
                        let mut state = state;
                        state.restart_failures = state.restart_failures.saturating_add(1);
                        tracing::warn!(
                            provider = state.provider.id(),
                            lane = state.lane.label(),
                            restart_delay_ms = state.restart_delay().as_millis(),
                            error = error.as_deref().unwrap_or("scheduler lane exited unexpectedly"),
                            "provider sync scheduler lane stopped; restarting"
                        );
                        spawn_scheduler_lane(
                            &mut tasks,
                            &mut task_metadata,
                            ctx.clone(),
                            state,
                        );
                    }
                }
            }
        }
    };
    let supervisor = tokio::spawn(supervisor_future);
    vec![AbortOnDropTask::new(supervisor)]
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProviderRevisionSignal {
    Changed,
    Closed,
}

async fn wait_for_provider_revision(
    receiver: &mut Option<tokio::sync::watch::Receiver<u64>>,
) -> ProviderRevisionSignal {
    match receiver {
        Some(receiver) => match receiver.changed().await {
            Ok(()) => ProviderRevisionSignal::Changed,
            Err(_) => ProviderRevisionSignal::Closed,
        },
        None => std::future::pending().await,
    }
}

async fn wait_for_shutdown(receiver: &mut tokio::sync::watch::Receiver<bool>) {
    loop {
        if *receiver.borrow_and_update() {
            return;
        }
        if receiver.changed().await.is_err() {
            return;
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SchedulerLane {
    Fast,
    Slow,
}

impl SchedulerLane {
    const fn label(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Slow => "slow",
        }
    }
}

#[derive(Clone)]
struct SchedulerLaneState {
    provider: SyncProvider,
    lane: SchedulerLane,
    global_backoff: Arc<Mutex<TargetBackoff>>,
    restart_failures: u32,
}

impl SchedulerLaneState {
    fn restart_delay(&self) -> Duration {
        if self.restart_failures == 0 {
            return Duration::ZERO;
        }
        let exponent = self.restart_failures.saturating_sub(1).min(6);
        SCHEDULER_RESTART_BASE
            .saturating_mul(1_u32 << exponent)
            .min(SCHEDULER_RESTART_MAX)
    }
}

fn spawn_scheduler_lane<C>(
    tasks: &mut tokio::task::JoinSet<(SchedulerLaneState, std::result::Result<(), String>)>,
    metadata: &mut HashMap<tokio::task::Id, SchedulerLaneState>,
    ctx: Arc<C>,
    state: SchedulerLaneState,
) where
    C: SyncContext + 'static,
{
    let task_state = state.clone();
    let abort_handle = tasks.spawn(async move {
        let mut task_state = task_state;
        let restart_delay = task_state.restart_delay();
        if !restart_delay.is_zero() {
            tokio::time::sleep(restart_delay).await;
        }
        let run_started = tokio::time::Instant::now();
        let provider = task_state.provider.clone();
        let backoff = task_state.global_backoff.clone();
        let result = AssertUnwindSafe(async {
            match task_state.lane {
                SchedulerLane::Fast => run_fast_scheduler(ctx, provider, backoff).await,
                SchedulerLane::Slow => run_slow_scheduler(ctx, provider, backoff).await,
            }
        })
        .catch_unwind()
        .await
        .map_err(panic_payload_message);
        task_state.restart_failures = scheduler_restart_failures_after_run(
            task_state.restart_failures,
            run_started.elapsed(),
        );
        (task_state, result)
    });
    metadata.insert(abort_handle.id(), state);
}

fn scheduler_restart_failures_after_run(failures: u32, run_duration: Duration) -> u32 {
    if run_duration >= SCHEDULER_RESTART_RESET_AFTER {
        0
    } else {
        failures
    }
}

async fn stop_scheduler_lanes<T>(tasks: &mut tokio::task::JoinSet<T>)
where
    T: 'static,
{
    tasks.abort_all();
    let drain = async { while tasks.join_next().await.is_some() {} };
    if tokio::time::timeout(PROVIDER_DETACH_GRACE, drain)
        .await
        .is_err()
    {
        tracing::warn!("provider scheduler lanes did not detach after abort");
    }
}

fn scheduler_shutdown_requested(receiver: &tokio::sync::watch::Receiver<bool>) -> bool {
    *receiver.borrow() || receiver.has_changed().is_err()
}

async fn run_fast_scheduler<C>(
    ctx: Arc<C>,
    provider: SyncProvider,
    global_backoff: Arc<Mutex<TargetBackoff>>,
) where
    C: SyncContext + 'static,
{
    let mut shutdown_rx = ctx.shutdown_receiver();
    // Tick at the active cadence; skip work between ticks when no
    // client is subscribed (idle keeps API spend low). Initial
    // cache warm owns the boot-time pull, so this loop waits one
    // cadence before its first run.
    let mut interval =
        tokio::time::interval_at(tokio::time::Instant::now() + ACTIVE_CADENCE, ACTIVE_CADENCE);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // `None` until the first successful sync — keeps the
    // first-tick "elapsed" check from gating us at boot when
    // subscribers aren't yet attached. (Tokio's `Instant` starts
    // at process boot, so we can't construct an Instant in the
    // past to satisfy `elapsed >= IDLE_CADENCE` on first tick.)
    let mut last_sync: Option<tokio::time::Instant> = None;
    let mut last_playback_sync: Option<tokio::time::Instant> = None;
    let mut last_queue_sync: Option<tokio::time::Instant> = None;
    let mut last_devices_sync: Option<tokio::time::Instant> = None;
    let mut last_recent_sync: Option<tokio::time::Instant> = None;
    let mut playback_backoff = TargetBackoff::default();
    let mut queue_backoff = TargetBackoff::default();
    let mut devices_backoff = TargetBackoff::default();
    let mut recent_backoff = TargetBackoff::default();
    // Shared cooldown across every fast lane (and the slow loop): a
    // 429 on any of playback/queue/devices/recent/playlists/library
    // pauses ALL of them until the provider's Retry-After elapses,
    // with escalation on consecutive 429s. Per-lane backoff alone
    // leaves the lanes staggered so something hits Spotify every few
    // seconds and its rolling rate-limit window never drains.
    loop {
        tokio::select! {
            _ = interval.tick() => {
                let has_subscribers = ctx.event_subscriber_count() > 0;
                let elapsed = last_sync.map_or(IDLE_CADENCE, |t| t.elapsed());
                if !has_subscribers && elapsed < IDLE_CADENCE {
                    continue;
                }
                let now = tokio::time::Instant::now();
                if !global_backoff.lock().expect("global backoff lock").should_run(now) {
                    tracing::debug!(provider = provider.id(), "fast sync paused: provider rate-limit cooldown");
                    continue;
                }
                // Playback. When our own embedded device is the active
                // player, librespot's player events already drive the
                // clock live, so the `/me/player` Web API poll is
                // redundant — drop it to a slow reconciliation. When
                // playback is LIVE elsewhere (a phone, etc.) keep
                // polling every tick so cross-device changes stay
                // fresh. When NOTHING is playing anywhere, probe at
                // the idle cadence: with a subscribed client this
                // branch used to poll every 3s around the clock —
                // the single biggest share of the ~13k/day calls
                // that earned hour-long 429 penalties. A foreign
                // device starting playback is noticed within one
                // idle interval; our own device reports instantly
                // via player events either way.
                let playback_due = if ctx.embedded_is_active_playback() {
                    last_playback_sync
                        .is_none_or(|t| t.elapsed() >= PLAYBACK_RECONCILE_CADENCE)
                } else if !ctx.snapshot_playback().is_playing {
                    last_playback_sync.is_none_or(|t| t.elapsed() >= IDLE_CADENCE)
                } else {
                    true
                };
                if playback_due && provider_supports_target(&provider, SyncTargetData::Playback) {
                    let epoch = global_backoff.lock().expect("global backoff lock").epoch();
                    let pb = sync_provider_target_with_backoff(
                        ctx.clone(),
                        provider.clone(),
                        SyncTargetData::Playback,
                        &mut playback_backoff,
                    )
                    .await;
                    record_global(&global_backoff, epoch, &pb);
                    log_background_result(SyncTargetData::Playback, &pb);
                    last_playback_sync = Some(now);
                }

                if provider_supports_target(&provider, SyncTargetData::Queue)
                    && global_backoff.lock().expect("global backoff lock").should_run(now)
                    && last_queue_sync.is_none_or(|t| t.elapsed() >= QUEUE_CADENCE)
                {
                    let epoch = global_backoff.lock().expect("global backoff lock").epoch();
                    let q = sync_provider_target_with_backoff(
                        ctx.clone(),
                        provider.clone(),
                        SyncTargetData::Queue,
                        &mut queue_backoff,
                    )
                    .await;
                    record_global(&global_backoff, epoch, &q);
                    log_background_result(SyncTargetData::Queue, &q);
                    last_queue_sync = Some(now);
                }

                if provider_supports_target(&provider, SyncTargetData::Devices)
                    && global_backoff.lock().expect("global backoff lock").should_run(now)
                    && last_devices_sync.is_none_or(|t| t.elapsed() >= DEVICES_CADENCE)
                {
                    let epoch = global_backoff.lock().expect("global backoff lock").epoch();
                    let d = sync_provider_target_with_backoff(
                        ctx.clone(),
                        provider.clone(),
                        SyncTargetData::Devices,
                        &mut devices_backoff,
                    )
                    .await;
                    record_global(&global_backoff, epoch, &d);
                    log_background_result(SyncTargetData::Devices, &d);
                    last_devices_sync = Some(now);
                }

                let recent_due = last_recent_sync
                    .is_none_or(|last| last.elapsed() >= RECENT_ACTIVE_CADENCE);
                if provider_supports_target(&provider, SyncTargetData::Recent)
                    && global_backoff.lock().expect("global backoff lock").should_run(now)
                    && recent_due
                {
                    let epoch = global_backoff.lock().expect("global backoff lock").epoch();
                    let r = sync_provider_target_with_backoff(
                        ctx.clone(),
                        provider.clone(),
                        SyncTargetData::Recent,
                        &mut recent_backoff,
                    )
                    .await;
                    record_global(&global_backoff, epoch, &r);
                    log_background_result(SyncTargetData::Recent, &r);
                    last_recent_sync = Some(now);
                }
                last_sync = Some(now);
            }
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow_and_update() {
                    break;
                }
            }
        }
    }
}

async fn run_slow_scheduler<C>(
    ctx: Arc<C>,
    provider: SyncProvider,
    global_backoff: Arc<Mutex<TargetBackoff>>,
) where
    C: SyncContext + 'static,
{
    let mut shutdown_rx = ctx.shutdown_receiver();
    let mut interval = tokio::time::interval_at(
        tokio::time::Instant::now() + SLOW_INITIAL_DELAY,
        SLOW_CADENCE,
    );
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut playlists_backoff = TargetBackoff::default();
    let mut library_backoff = TargetBackoff::default();
    loop {
        tokio::select! {
            _ = interval.tick() => {
                let now = tokio::time::Instant::now();
                if !global_backoff.lock().expect("global backoff lock").should_run(now) {
                    tracing::debug!(provider = provider.id(), "slow sync paused: provider rate-limit cooldown");
                    continue;
                }
                if provider_supports_target(&provider, SyncTargetData::Playlists) {
                    let playlists_epoch = global_backoff.lock().expect("global backoff lock").epoch();
                    let playlists = sync_provider_target_with_backoff_timeout(
                        ctx.clone(),
                        provider.clone(),
                        SyncTargetData::Playlists,
                        &mut playlists_backoff,
                        SLOW_TARGET_TIMEOUT,
                    ).await;
                    if let Some(Err(err)) = &playlists {
                        tracing::warn!(error = %err, "background playlists sync failed");
                    }
                    // Bail before firing the library sync if playlists hit
                    // a 429; otherwise the slow loop's first tick double-
                    // bursts an already-tight window (4+ requests in
                    // <500ms) before the global cooldown can kick in.
                    record_global(&global_backoff, playlists_epoch, &playlists);
                    if rate_limit_retry_after(&playlists).is_some() {
                        continue;
                    }
                }
                if provider_supports_target(&provider, SyncTargetData::Library) {
                    let library_epoch = global_backoff.lock().expect("global backoff lock").epoch();
                    let library = sync_provider_target_with_backoff_timeout(
                        ctx.clone(),
                        provider.clone(),
                        SyncTargetData::Library,
                        &mut library_backoff,
                        SLOW_TARGET_TIMEOUT,
                    ).await;
                    if let Some(Err(err)) = &library {
                        tracing::warn!(error = %err, "background library sync failed");
                    }
                    record_global(&global_backoff, library_epoch, &library);
                }
            }
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow_and_update() {
                    break;
                }
            }
        }
    }
}

#[derive(Debug, Default)]
struct TargetBackoff {
    failures: u32,
    next_allowed: Option<tokio::time::Instant>,
    epoch: u64,
}

impl TargetBackoff {
    fn should_run(&self, now: tokio::time::Instant) -> bool {
        self.next_allowed.is_none_or(|next| now >= next)
    }

    fn record_success(&mut self) {
        self.failures = 0;
        self.next_allowed = None;
    }

    fn epoch(&self) -> u64 {
        self.epoch
    }

    fn record_success_if_epoch(&mut self, observed_epoch: u64) {
        if self.epoch == observed_epoch {
            self.record_success();
        }
    }

    fn seed_persisted_cooldown(&mut self, now: tokio::time::Instant, remaining: Duration) {
        self.failures = self.failures.max(1);
        self.epoch = self.epoch.saturating_add(1);
        self.next_allowed = Some(now + remaining);
    }

    /// Trip the shared global cooldown on a 429, escalating the pause on
    /// consecutive hits. Spotify's per-request `Retry-After` is the floor;
    /// each consecutive 429 doubles the daemon's own backoff
    /// (`GLOBAL_BACKOFF_BASE` × 2^(n-1), capped at `GLOBAL_BACKOFF_MAX`)
    /// so a deeply-throttled account isn't kept alive by once-a-minute
    /// probing. [`record_success`] resets the escalation.
    fn note_rate_limit(&mut self, now: tokio::time::Instant, retry_after: Duration) {
        self.failures = self.failures.saturating_add(1);
        self.epoch = self.epoch.saturating_add(1);
        let shift = self.failures.saturating_sub(1).min(4);
        let escalated = GLOBAL_BACKOFF_BASE
            .saturating_mul(1 << shift)
            .min(GLOBAL_BACKOFF_MAX);
        let pause = retry_after.max(escalated).min(GLOBAL_BACKOFF_MAX);
        self.next_allowed = Some(now + pause);
    }

    fn record_failure(&mut self, now: tokio::time::Instant, err: &anyhow::Error) {
        self.failures = self.failures.saturating_add(1);
        if let Some(ProviderError::RateLimited { retry_after, scope }) =
            err.downcast_ref::<ProviderError>()
        {
            let retry_after = retry_after.unwrap_or(BACKOFF_BASE);
            self.next_allowed = Some(now + retry_after);
            tracing::debug!(
                scope = scope.as_deref().unwrap_or("provider"),
                retry_after_secs = retry_after.as_secs(),
                "background sync target honoring provider rate-limit cooldown"
            );
            return;
        }
        let multiplier = 1_u64 << self.failures.saturating_sub(1).min(3);
        let delay =
            Duration::from_secs((BACKOFF_BASE.as_secs() * multiplier).min(BACKOFF_MAX.as_secs()));
        self.next_allowed = Some(now + delay);
    }
}

async fn sync_provider_target_with_backoff<C>(
    ctx: Arc<C>,
    provider: SyncProvider,
    target: SyncTargetData,
    backoff: &mut TargetBackoff,
) -> Option<Result<CacheSyncSummary>>
where
    C: SyncContext + 'static,
{
    sync_provider_target_with_backoff_timeout(ctx, provider, target, backoff, PER_TARGET_TIMEOUT)
        .await
}

async fn sync_provider_target_with_backoff_timeout<C>(
    ctx: Arc<C>,
    provider: SyncProvider,
    target: SyncTargetData,
    backoff: &mut TargetBackoff,
    timeout: Duration,
) -> Option<Result<CacheSyncSummary>>
where
    C: SyncContext + 'static,
{
    let now = tokio::time::Instant::now();
    if !backoff.should_run(now) {
        tracing::debug!(
            provider = provider.id(),
            target = target.label(),
            "background sync target in backoff"
        );
        return None;
    }
    let provider_id = provider.provider_id().clone();
    ctx.emit_event(DaemonEvent::SyncStarted {
        target,
        provider: Some(provider_id.clone()),
    });
    let result =
        sync_provider_target_bounded_with_timeout(ctx.clone(), provider, target, timeout).await;
    ctx.emit_event(DaemonEvent::SyncFinished {
        summary: terminal_summary(target, provider_id, &result),
    });
    match &result {
        Ok(_) => backoff.record_success(),
        Err(err) => backoff.record_failure(tokio::time::Instant::now(), err),
    }
    Some(result)
}

/// Run one provider target through the same lock, abort-on-drop task, and
/// target deadline used by background and isolated sync.
pub async fn sync_provider_target_bounded<C>(
    ctx: Arc<C>,
    provider: SyncProvider,
    target: SyncTargetData,
) -> Result<CacheSyncSummary>
where
    C: SyncContext + 'static,
{
    sync_provider_target_bounded_with_timeout(ctx, provider, target, target_timeout(target)).await
}

#[doc(hidden)]
pub async fn sync_provider_target_bounded_with_timeout<C>(
    ctx: Arc<C>,
    provider: SyncProvider,
    target: SyncTargetData,
    timeout: Duration,
) -> Result<CacheSyncSummary>
where
    C: SyncContext + 'static,
{
    let provider_id = provider.id().to_string();
    let task_future =
        async move { sync_provider_target_locked(ctx.as_ref(), &provider, target).await };
    // See `spawn_background_scheduler`: provider futures stay on the caller's
    // runtime even when the host exposes a separate runtime for pure DB work.
    let mut task = AbortOnDropTask::new(tokio::spawn(task_future));
    match tokio::time::timeout(timeout, &mut task).await {
        Ok(joined) => joined.map_err(|err| anyhow::anyhow!("provider sync task failed: {err}"))?,
        Err(_) => {
            task.abort();
            if tokio::time::timeout(PROVIDER_DETACH_GRACE, &mut task)
                .await
                .is_err()
            {
                tracing::warn!(
                    provider = provider_id,
                    target = target.label(),
                    "provider sync task did not detach after abort"
                );
            }
            Err(anyhow::anyhow!(
                "timed out syncing provider {provider_id} {} after {}s",
                target.label(),
                timeout.as_secs()
            ))
        }
    }
}

async fn acquire_sync_locks<C: SyncContext>(
    ctx: &C,
    provider_id: &str,
    target: SyncTargetData,
) -> Vec<tokio::sync::OwnedMutexGuard<()>> {
    let mut guards = Vec::new();
    for lock in ctx.sync_locks_for(provider_id, target) {
        guards.push(lock.lock_owned().await);
    }
    guards
}

async fn sync_provider_target_locked<C: SyncContext>(
    ctx: &C,
    provider: &SyncProvider,
    target: SyncTargetData,
) -> Result<CacheSyncSummary> {
    let _guards = acquire_sync_locks(ctx, provider.id(), target).await;
    sync_provider_target(ctx, provider, target).await
}

fn log_background_result(target: SyncTargetData, result: &Option<Result<CacheSyncSummary>>) {
    if let Some(Err(err)) = result {
        tracing::debug!(target = target.label(), error = %err, "background sync failed");
    }
}

/// Provider `Retry-After` from a sync result that failed with a 429. Providers
/// may omit it; the per-provider circuit breaker still applies its base delay.
fn rate_limit_retry_after(result: &Option<Result<CacheSyncSummary>>) -> Option<Duration> {
    if let Some(Err(err)) = result {
        if let Some(ProviderError::RateLimited { retry_after, .. }) =
            err.downcast_ref::<ProviderError>()
        {
            return Some(retry_after.unwrap_or(GLOBAL_BACKOFF_BASE));
        }
    }
    None
}

/// Feed a lane's result into one provider's global cooldown. A provider's
/// fast and slow lanes share this state, but unrelated providers do not.
fn record_global(
    global: &Mutex<TargetBackoff>,
    observed_epoch: u64,
    result: &Option<Result<CacheSyncSummary>>,
) {
    record_global_at(global, tokio::time::Instant::now(), observed_epoch, result);
}

fn record_global_at(
    global: &Mutex<TargetBackoff>,
    completed_at: tokio::time::Instant,
    observed_epoch: u64,
    result: &Option<Result<CacheSyncSummary>>,
) {
    let mut g = global.lock().expect("global backoff lock");
    if let Some(retry) = rate_limit_retry_after(result) {
        g.note_rate_limit(completed_at, retry);
    } else if matches!(result, Some(Ok(_))) {
        g.record_success_if_epoch(observed_epoch);
    }
}

/// Run one sync pass for the given target. Used both by the background
/// scheduler and by the `spotuify sync` CLI command.
pub async fn sync_target<C: SyncContext>(
    ctx: &C,
    target: SyncTargetData,
) -> Result<CacheSyncSummary> {
    let providers = ctx.sync_providers().await?;
    let aggregate_provider = (providers.len() == 1).then(|| providers[0].provider_id().clone());
    let mut summary = empty_summary(target, aggregate_provider);
    let timeout = target_timeout(target);
    let mut first_error = None;
    let mut successes = 0_u32;
    for provider in providers {
        let provider_id = provider.provider_id().clone();
        ctx.emit_event(DaemonEvent::SyncStarted {
            target,
            provider: Some(provider_id.clone()),
        });
        let result =
            tokio::time::timeout(timeout, sync_provider_target_locked(ctx, &provider, target))
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "timed out syncing provider {} {} after {}s",
                        provider.id(),
                        target.label(),
                        timeout.as_secs()
                    )
                })
                .and_then(std::convert::identity);
        let terminal = terminal_summary(target, provider_id.clone(), &result);
        ctx.emit_event(DaemonEvent::SyncFinished {
            summary: terminal.clone(),
        });
        summary.provider_outcomes.push(outcome_from(&terminal));
        match result {
            Ok(provider_summary) => {
                successes += 1;
                merge_summary(&mut summary, &provider_summary);
            }
            Err(err) if first_error.is_none() => first_error = Some(err),
            Err(_) => {}
        }
    }
    if let Some(err) = first_error {
        if successes == 0 {
            return Err(err);
        }
        summary.status = SyncCompletionStatus::Partial;
        summary.error = Some(err.to_string());
    }
    Ok(summary)
}

/// Spawn every provider pass concurrently and bound cancellation for each.
/// This is the scheduler/integration surface when the context is owned.
pub async fn sync_target_isolated<C>(
    ctx: Arc<C>,
    target: SyncTargetData,
) -> Result<CacheSyncSummary>
where
    C: SyncContext + 'static,
{
    sync_target_isolated_with_timeout(ctx, target, target_timeout(target)).await
}

#[doc(hidden)]
pub async fn sync_target_isolated_with_timeout<C>(
    ctx: Arc<C>,
    target: SyncTargetData,
    timeout: Duration,
) -> Result<CacheSyncSummary>
where
    C: SyncContext + 'static,
{
    let providers = ctx.sync_providers().await?;
    let aggregate_provider = (providers.len() == 1).then(|| providers[0].provider_id().clone());
    let mut tasks = tokio::task::JoinSet::new();
    let mut task_providers = HashMap::new();
    for provider in providers {
        let task_ctx = ctx.clone();
        let provider_id = provider.provider_id().clone();
        ctx.emit_event(DaemonEvent::SyncStarted {
            target,
            provider: Some(provider_id.clone()),
        });
        let task_provider_id = provider_id.clone();
        let abort_handle = tasks.spawn(async move {
            let result = AssertUnwindSafe(sync_provider_target_bounded_with_timeout(
                task_ctx, provider, target, timeout,
            ))
            .catch_unwind()
            .await
            .unwrap_or_else(|payload| {
                Err(anyhow::anyhow!(
                    "provider sync task panicked: {}",
                    panic_payload_message(payload)
                ))
            });
            (task_provider_id, result)
        });
        task_providers.insert(abort_handle.id(), provider_id);
    }
    let mut summary = empty_summary(target, aggregate_provider);
    let mut first_error = None;
    while let Some(joined) = tasks.join_next_with_id().await {
        let (provider_id, result) = match joined {
            Ok((task_id, (provider_id, result))) => {
                task_providers.remove(&task_id);
                (provider_id, result)
            }
            Err(err) => {
                let provider_id = task_providers
                    .remove(&err.id())
                    .unwrap_or_else(|| ProviderId::new("unknown").expect("valid provider id"));
                (
                    provider_id,
                    Err(anyhow::anyhow!("provider sync task failed: {err}")),
                )
            }
        };
        let terminal = terminal_summary(target, provider_id.clone(), &result);
        ctx.emit_event(DaemonEvent::SyncFinished {
            summary: terminal.clone(),
        });
        summary.provider_outcomes.push(outcome_from(&terminal));
        match result {
            Ok(provider_summary) => merge_summary(&mut summary, &provider_summary),
            Err(err) => {
                if first_error.is_none() {
                    first_error = Some(err);
                }
            }
        }
    }
    summary
        .provider_outcomes
        .sort_by(|left, right| left.provider.cmp(&right.provider));
    if let Some(err) = first_error {
        let successes = summary
            .provider_outcomes
            .iter()
            .filter(|outcome| outcome.status == SyncCompletionStatus::Succeeded)
            .count();
        if successes == 0 {
            return Err(err);
        }
        summary.status = SyncCompletionStatus::Partial;
        summary.error = Some(err.to_string());
    }
    Ok(summary)
}

fn panic_payload_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

fn target_timeout(target: SyncTargetData) -> Duration {
    match target {
        SyncTargetData::All | SyncTargetData::Playlists | SyncTargetData::Library => {
            SLOW_TARGET_TIMEOUT
        }
        _ => PER_TARGET_TIMEOUT,
    }
}

fn empty_summary(target: SyncTargetData, provider: Option<ProviderId>) -> CacheSyncSummary {
    CacheSyncSummary {
        target,
        provider,
        playback_snapshots: 0,
        queue_snapshots: 0,
        queue_items: 0,
        devices: 0,
        playlists: 0,
        playlist_items: 0,
        recent_items: 0,
        library_items: 0,
        media_items: 0,
        status: SyncCompletionStatus::Succeeded,
        error: None,
        provider_outcomes: Vec::new(),
    }
}

fn failed_summary(target: SyncTargetData, provider: ProviderId, error: String) -> CacheSyncSummary {
    let mut summary = empty_summary(target, Some(provider));
    summary.status = SyncCompletionStatus::Failed;
    summary.error = Some(error);
    summary
}

fn terminal_summary(
    target: SyncTargetData,
    provider: ProviderId,
    result: &Result<CacheSyncSummary>,
) -> CacheSyncSummary {
    match result {
        Ok(summary) => summary.clone(),
        Err(err) => failed_summary(target, provider, err.to_string()),
    }
}

fn outcome_from(summary: &CacheSyncSummary) -> ProviderSyncOutcome {
    ProviderSyncOutcome {
        provider: summary
            .provider
            .clone()
            .unwrap_or_else(|| ProviderId::new("unknown").expect("valid provider id")),
        status: summary.status,
        error: summary.error.clone(),
    }
}

fn merge_summary(total: &mut CacheSyncSummary, next: &CacheSyncSummary) {
    total.playback_snapshots += next.playback_snapshots;
    total.queue_snapshots += next.queue_snapshots;
    total.queue_items += next.queue_items;
    total.devices += next.devices;
    total.playlists += next.playlists;
    total.playlist_items += next.playlist_items;
    total.recent_items += next.recent_items;
    total.library_items += next.library_items;
    total.media_items += next.media_items;
}

async fn sync_provider_target<C: SyncContext>(
    ctx: &C,
    provider: &SyncProvider,
    target: SyncTargetData,
) -> Result<CacheSyncSummary> {
    let mut summary = empty_summary(target, Some(provider.provider_id().clone()));
    if target != SyncTargetData::All && !provider_supports_target(provider, target) {
        return Err(ProviderError::unsupported(format!(
            "provider {} {} sync",
            provider.id(),
            target.label()
        ))
        .into());
    }
    let targets: &[SyncTargetData] = match target {
        SyncTargetData::All => &[
            SyncTargetData::Playback,
            SyncTargetData::Queue,
            SyncTargetData::Devices,
            SyncTargetData::Playlists,
            SyncTargetData::Recent,
            SyncTargetData::Library,
        ],
        _ => std::slice::from_ref(&target),
    };
    for domain in targets {
        if provider_supports_target(provider, *domain) {
            sync_domain_with_recovery(ctx, provider, *domain, &mut summary).await?;
        } else {
            tracing::debug!(
                provider = provider.id(),
                target = domain.label(),
                "skipping unsupported sync target"
            );
        }
    }
    Ok(summary)
}

fn provider_supports_target(provider: &SyncProvider, target: SyncTargetData) -> bool {
    let caps = provider.music.capabilities();
    match target {
        SyncTargetData::All => true,
        SyncTargetData::Playback => {
            caps.transport
                .as_ref()
                .is_some_and(|caps| caps.playback_state)
                && provider.transport.is_some()
        }
        SyncTargetData::Queue => {
            caps.transport.as_ref().is_some_and(|caps| caps.queue_read)
                && provider.transport.is_some()
        }
        SyncTargetData::Devices => {
            caps.transport.as_ref().is_some_and(|caps| caps.devices) && provider.transport.is_some()
        }
        SyncTargetData::Playlists => caps.playlists.list,
        SyncTargetData::Recent => caps.catalog.recently_played,
        SyncTargetData::Library => !caps.library.read_kinds.is_empty(),
    }
}

async fn sync_domain_with_recovery<C: SyncContext>(
    ctx: &C,
    provider: &SyncProvider,
    target: SyncTargetData,
    summary: &mut CacheSyncSummary,
) -> Result<()> {
    let mut attempt = empty_summary(summary.target, summary.provider.clone());
    let first = sync_domain_once(ctx, provider, target, &mut attempt, false).await;
    let Err(err) = first else {
        merge_summary(summary, &attempt);
        return Ok(());
    };
    if !is_sync_token_expired(&err) {
        return Err(err);
    }
    clear_target_cursor(ctx, provider, target).await?;
    let mut retry = empty_summary(summary.target, summary.provider.clone());
    sync_domain_once(ctx, provider, target, &mut retry, true).await?;
    merge_summary(summary, &retry);
    Ok(())
}

async fn clear_target_cursor<C: SyncContext>(
    ctx: &C,
    provider: &SyncProvider,
    target: SyncTargetData,
) -> Result<()> {
    if target == SyncTargetData::Library {
        ctx.store()
            .clear_sync_cursors_with_prefix(provider.id(), "library/")
            .await
    } else {
        ctx.store()
            .clear_sync_cursor(provider.id(), target.label())
            .await
    }
}

async fn sync_domain_once<C: SyncContext>(
    ctx: &C,
    provider: &SyncProvider,
    target: SyncTargetData,
    summary: &mut CacheSyncSummary,
    force_full: bool,
) -> Result<()> {
    match target {
        SyncTargetData::Playback => sync_playback(ctx, provider, summary).await,
        SyncTargetData::Queue => sync_queue(ctx, provider, summary).await,
        SyncTargetData::Devices => sync_devices(ctx, provider, summary).await,
        SyncTargetData::Playlists => sync_playlists(ctx, provider, summary, force_full).await,
        SyncTargetData::Recent => sync_recent(ctx, provider, summary).await,
        SyncTargetData::Library => sync_library(ctx, provider, summary, force_full).await,
        SyncTargetData::All => Ok(()),
    }
}

fn is_sync_token_expired(err: &anyhow::Error) -> bool {
    matches!(
        err.downcast_ref::<ProviderError>(),
        Some(ProviderError::SyncTokenExpired { .. })
    )
}

async fn sync_queue<C: SyncContext>(
    ctx: &C,
    provider: &SyncProvider,
    summary: &mut CacheSyncSummary,
) -> Result<()> {
    fail_if_rate_limited_domain(ctx, provider, "queue").await?;
    let started_at_ms = now_ms();
    // Capture seq pre-call so a QueueAdd / Next / Previous racing
    // with this poll can be detected; see sync_playback for the same
    // pattern.
    let pre_seq = ctx.observe_mutation_seq();
    let transport = required_transport(provider)?;
    match transport.queue(RequestContext::BACKGROUND_SYNC).await {
        Ok(queue) => {
            validate_provider_queue(provider, &queue)?;
            if !ctx.may_apply_transport_update(provider.provider_id(), pre_seq) {
                tracing::debug!("dropping stale queue poll: mutation in flight");
            } else if !queue.session_active {
                // Spotify has no active playback session right now.
                // Don't clobber the cache with this empty response —
                // the user's last-known queue (yesterday's listening,
                // tracks they explicitly queued before going idle)
                // would be lost. Don't broadcast either: every
                // subscriber already saw the cached snapshot via
                // build_subscribe_snapshot or the eager warm at boot,
                // and re-emitting "no-session" every 3s during steady-
                // state idle would just be noise. The mid-session →
                // session-ended transition is rare enough (and self-
                // heals on next QueueGet / subscribe) that we let
                // those paths repaint it.
                tracing::debug!("queue sync: no active session, preserving cache");
            } else {
                // Capture the pre-poll snapshot BEFORE persisting so
                // the diff sees the old state. Snapshot is read from
                // `store().latest_queue(...)` in the default impl;
                // taking it after `persist_provider_queue_bulk` would observe
                // the just-written queue and the diff would always
                // collapse to "no change".
                let before_queue = ctx.snapshot_queue(provider.provider_id()).await;
                // Same merge as the daemon's refresh apply path —
                // Spotify caps the queue endpoint at ~20 items and
                // embedded librespot often reports zero upcoming, so
                // persisting the raw fetch here wiped the cached
                // context tail within one queue cadence (live bug).
                let anchor = spotuify_core::queue_merge::queue_tail_anchor(&queue);
                let now = now_ms();
                let queue = ctx.overlay_pending_queue_appends(provider.provider_id(), queue, now);
                let snapshots_complete = provider
                    .music
                    .capabilities()
                    .transport
                    .as_ref()
                    .is_some_and(|transport| transport.queue_snapshots_complete);
                let queue = spotuify_core::queue_merge::reconcile_provider_queue(
                    queue,
                    anchor.as_deref(),
                    &before_queue,
                    ctx.snapshot_playback().shuffle,
                    now,
                    snapshots_complete,
                );
                validate_provider_queue(provider, &queue)?;
                if let Some(written) = ctx
                    .persist_queue_poll_if_current(provider.provider_id(), &queue, pre_seq)
                    .await?
                {
                    summary.queue_snapshots += written;
                    summary.queue_items += queue.items.len() as u32;
                    let mut items = Vec::with_capacity(queue.items.len() + 1);
                    if let Some(item) = queue.currently_playing.as_ref() {
                        items.push(item.clone());
                    }
                    items.extend(queue.items.iter().cloned());
                    if !items.is_empty() {
                        summary.media_items += items.len() as u32;
                        if let Err(err) = ctx.index_media_items(provider.id(), &items, false).await
                        {
                            tracing::debug!(error = %err, "queue index update failed");
                        }
                    }
                    ctx.warm_queue(&queue);
                    // Diff-then-broadcast only after the gated durable commit.
                    let mut after_queue = ctx.snapshot_queue(provider.provider_id()).await;
                    after_queue.session_active = true;
                    if queue_diff_is_meaningful(&before_queue, &after_queue) {
                        ctx.emit_event(DaemonEvent::QueueChanged {
                            action: "synced".to_string(),
                            uris: queue.items.iter().map(|i| i.uri.clone()).collect(),
                            queue: Some(after_queue),
                        });
                    }
                } else {
                    tracing::debug!(
                        "dropping queue poll before persist: mutation raced the store write"
                    );
                }
            }
            ctx.store()
                .record_provider_sync_event_bulk(
                    provider.id(),
                    "queue",
                    started_at_ms,
                    "ok",
                    summary.queue_items + summary.queue_snapshots,
                    None,
                )
                .await?;
            Ok(())
        }
        Err(err) => {
            record_sync_error(ctx, provider, "queue", started_at_ms, &err).await?;
            Err(err.into())
        }
    }
}

async fn sync_playback<C: SyncContext>(
    ctx: &C,
    provider: &SyncProvider,
    summary: &mut CacheSyncSummary,
) -> Result<()> {
    fail_if_rate_limited_domain(ctx, provider, "playback").await?;
    let started_at_ms = now_ms();
    // Capture the mutation seq BEFORE issuing the Spotify call so a
    // PlaybackCommand that races us is seen as "newer than this
    // poll" and we drop our (stale) result instead of clobbering the
    // optimistic local cache. Spotify's playback state is eventually
    // consistent on mutation; without this gate a poll started 200ms
    // before the user's Pause typically returns is_playing=true and
    // would overwrite the optimistic Paused snapshot. Linear's
    // lastSyncId solves the same race.
    let pre_seq = ctx.observe_mutation_seq();
    let transport = required_transport(provider)?;
    match transport.playback(RequestContext::BACKGROUND_SYNC).await {
        Ok(playback) => {
            if let Some(item) = playback.item.as_ref() {
                validate_provider_collection_items(
                    provider,
                    "playback",
                    &[MediaKind::Track, MediaKind::Episode],
                    std::slice::from_ref(item),
                )?;
            }
            if !ctx.may_apply_transport_update(provider.provider_id(), pre_seq) {
                tracing::debug!("dropping stale playback poll: mutation in flight");
            } else {
                // Feed the host's playback clock and broadcast
                // `PlaybackChanged` so subscribed clients (TUI/MCP)
                // re-render. Without this hop the background poll
                // would only land in SQLite — the TUI's player widget
                // never sees the update.
                //
                // Diff-then-broadcast: snapshot the clock BEFORE
                // applying the poll, then compare with AFTER. Steady-
                // state same-track playback re-anchors the clock
                // every 3s for drift correction but doesn't actually
                // change anything the user can perceive (clients
                // extrapolate progress locally). Skip the emit in
                // that case — subscribers were getting ~20 events/min
                // during normal listening, now they get ~1 when
                // something real happens (track change, pause/play,
                // device move, big seek elsewhere).
                // Persistence is the commit point. A failed SQLite write must
                // leave the daemon clock and subscriber stream untouched.
                let sampled_at_ms = now_ms();
                let persisted = ctx
                    .prepare_and_persist_playback_poll_if_current(
                        provider.provider_id(),
                        &playback,
                        pre_seq,
                        sampled_at_ms,
                        playback.provider_timestamp_ms,
                    )
                    .await?;
                if let Some((written, _candidate)) = persisted {
                    summary.playback_snapshots += written;
                    // Close the mutation race again before changing canonical
                    // in-memory state. The daemon implementation serialized the
                    // durable write with transport mutations, so if this check now
                    // fails the newer command is guaranteed to persist afterward.
                    let may_apply_after_persist =
                        ctx.may_apply_transport_update(provider.provider_id(), pre_seq);
                    if !may_apply_after_persist {
                        tracing::debug!(
                            "dropping playback poll after persist: mutation raced the store write"
                        );
                    }
                    let before = ctx.snapshot_playback();
                    let state_seq = ctx.observe_mutation_seq();
                    let applied = may_apply_after_persist
                        && ctx.apply_playback_poll(
                            provider.provider_id(),
                            &playback,
                            pre_seq,
                            state_seq,
                            sampled_at_ms,
                            playback.provider_timestamp_ms,
                        );
                    let after = applied.then(|| ctx.snapshot_playback());
                    if let Some(item) = playback.item.as_ref() {
                        summary.media_items += 1;
                        if let Err(err) = ctx
                            .index_media_items(provider.id(), std::slice::from_ref(item), false)
                            .await
                        {
                            tracing::debug!(error = %err, "playback item index update failed");
                        }
                    }
                    if applied {
                        let after = after.expect("applied playback poll should have snapshot");
                        if playback_diff_is_meaningful(&before, &after) {
                            ctx.emit_event(DaemonEvent::PlaybackChanged {
                                action: "synced".to_string(),
                                playback: Some(after),
                            });
                        }
                    }
                } else {
                    tracing::debug!(
                        "dropping playback poll before persist: stale mutation or unconfirmed empty sample"
                    );
                }
            }
            ctx.store()
                .record_provider_sync_event_bulk(
                    provider.id(),
                    "playback",
                    started_at_ms,
                    "ok",
                    summary.playback_snapshots,
                    None,
                )
                .await?;
            Ok(())
        }
        Err(err) => {
            record_sync_error(ctx, provider, "playback", started_at_ms, &err).await?;
            Err(err.into())
        }
    }
}

async fn sync_devices<C: SyncContext>(
    ctx: &C,
    provider: &SyncProvider,
    summary: &mut CacheSyncSummary,
) -> Result<()> {
    fail_if_rate_limited_domain(ctx, provider, "devices").await?;
    let started_at_ms = now_ms();
    let pre_seq = ctx.observe_mutation_seq();
    let transport = required_transport(provider)?;
    match transport.devices(RequestContext::BACKGROUND_SYNC).await {
        Ok(devices) => {
            if !ctx.may_apply_transport_update(provider.provider_id(), pre_seq) {
                tracing::debug!("dropping stale devices poll: mutation in flight");
            } else {
                // Capture pre-poll snapshot BEFORE persisting so the
                // diff sees the old device set.
                let before_devices = ctx.snapshot_devices(provider.provider_id()).await;
                if let Some(written) = ctx
                    .persist_devices_poll_if_current(provider.provider_id(), &devices, pre_seq)
                    .await?
                {
                    summary.devices += written;
                    let after_devices = ctx.snapshot_devices(provider.provider_id()).await;
                    // Diff-then-broadcast only after the gated durable commit.
                    if devices_diff_is_meaningful(&before_devices, &after_devices) {
                        ctx.emit_event(DaemonEvent::DevicesChanged {
                            action: "synced".to_string(),
                            devices: Some(after_devices),
                        });
                    }
                } else {
                    tracing::debug!(
                        "dropping devices poll before persist: mutation raced the store write"
                    );
                }
            }
            ctx.store()
                .record_provider_sync_event_bulk(
                    provider.id(),
                    "devices",
                    started_at_ms,
                    "ok",
                    devices.len() as u32,
                    None,
                )
                .await?;
            Ok(())
        }
        Err(err) => {
            record_sync_error(ctx, provider, "devices", started_at_ms, &err).await?;
            Err(err.into())
        }
    }
}

async fn sync_playlists<C: SyncContext>(
    ctx: &C,
    provider: &SyncProvider,
    summary: &mut CacheSyncSummary,
    force_full: bool,
) -> Result<()> {
    fail_if_rate_limited_domain(ctx, provider, "playlists").await?;
    let started_at_ms = now_ms();
    let caps = provider.music.capabilities().playlists;
    if !caps.list {
        return Err(ProviderError::unsupported("playlists").into());
    }
    match fetch_all_playlists(provider.music.as_ref(), page_limit(caps.list_max_page_size)).await {
        Ok(playlists) => {
            let playlists = normalize_provider_playlists(provider, playlists)?;
            let mut local_version_tokens = std::collections::HashMap::new();
            for playlist in &playlists {
                let local_version_token = ctx
                    .store()
                    .playlist_version_token(&playlist.id)
                    .await
                    .ok()
                    .flatten();
                local_version_tokens.insert(playlist.id.clone(), local_version_token);
            }
            let playlist_outcome = ctx
                .store()
                .replace_provider_playlists_bulk(provider.id(), provider.id(), &playlists)
                .await?;
            summary.playlists += playlist_outcome.written;
            if !playlist_outcome.removed_uris.is_empty() {
                ctx.remove_indexed_media_items(&playlist_outcome.removed_uris)
                    .await?;
            }
            summary.media_items += playlists.len() as u32;
            let playlist_media = playlists
                .iter()
                .map(|playlist| playlist_as_media_item(provider.id(), playlist))
                .collect::<Result<Vec<_>>>()?;
            if let Err(err) = ctx
                .index_media_items(provider.id(), &playlist_media, false)
                .await
            {
                tracing::debug!(error = %err, "playlist index update failed");
            }
            if !caps.item_read {
                ctx.store()
                    .record_provider_sync_event_bulk(
                        provider.id(),
                        "playlists",
                        started_at_ms,
                        "ok",
                        summary.playlists,
                        None,
                    )
                    .await?;
                return Ok(());
            }
            // Provider version-token refetch gate, with a self-heal override.
            // If the local token matches the remote token but
            // the cached playlist has zero items while Spotify
            // reports a non-empty playlist, we treat it as a cache
            // corruption (a previous sync's persist crashed mid-write
            // and left the playlist empty under an up-to-date
            // snapshot). Force-refetch in that case so a single bad
            // sync run can't strand the playlist empty forever.
            for playlist in &playlists {
                let local_version_token = local_version_tokens
                    .get(&playlist.id)
                    .and_then(|token| token.as_deref());
                let remote_version_token = playlist.version_token.as_deref();
                let version_changed = should_refetch_playlist_tracks_for_provider(
                    provider.music.as_ref(),
                    local_version_token,
                    remote_version_token,
                );
                let tracks_accessible = ctx
                    .store()
                    .playlist_tracks_accessible(&playlist.id)
                    .await
                    .unwrap_or(true);
                if !force_full && !tracks_accessible && local_version_token == remote_version_token
                {
                    tracing::debug!(
                        playlist = %playlist.id,
                        version_token = %remote_version_token.unwrap_or(""),
                        "playlist tracks inaccessible at this version; skipping tracks refetch"
                    );
                    continue;
                }
                if !tracks_accessible {
                    tracing::info!(
                        playlist = %playlist.id,
                        old_version_token = %local_version_token.unwrap_or(""),
                        new_version_token = %remote_version_token.unwrap_or(""),
                        "inaccessible playlist version changed; retrying tracks"
                    );
                }
                let cache_empty_but_remote_has_tracks = !version_changed
                    && playlist.tracks_total > 0
                    && ctx
                        .store()
                        .playlist_items_count(&playlist.id)
                        .await
                        .unwrap_or(0)
                        == 0;
                let needs_refetch =
                    force_full || version_changed || cache_empty_but_remote_has_tracks;
                if !needs_refetch {
                    tracing::debug!(
                        playlist = %playlist.id,
                        version_token = %playlist.version_token.as_deref().unwrap_or(""),
                        "playlist unchanged; skipping tracks refetch"
                    );
                    continue;
                }
                if cache_empty_but_remote_has_tracks {
                    tracing::info!(
                        playlist = %playlist.id,
                        tracks_total = playlist.tracks_total,
                        "playlist cache empty but remote has tracks; force-refetching"
                    );
                }
                let playlist_uri = match ResourceUri::parse(&playlist.id) {
                    Ok(uri) => uri,
                    Err(err) => {
                        tracing::warn!(playlist = %playlist.id, error = %err, "playlist adapter returned invalid URI");
                        continue;
                    }
                };
                match fetch_all_playlist_items(
                    provider.music.as_ref(),
                    playlist_uri,
                    page_limit(caps.items_max_page_size),
                )
                .await
                {
                    Ok(AccessOutcome::Available(items)) => {
                        validate_provider_collection_items(
                            provider,
                            "playlist_items",
                            &[MediaKind::Track, MediaKind::Episode],
                            &items,
                        )?;
                        summary.playlist_items += ctx
                            .store()
                            .persist_provider_playlist_items_with_version_bulk(
                                provider.provider_id(),
                                &playlist.id,
                                &items,
                                playlist.version_token.as_deref(),
                            )
                            .await?;
                        summary.media_items += items.len() as u32;
                        if let Err(err) = ctx.index_media_items(provider.id(), &items, false).await
                        {
                            tracing::debug!(playlist = %playlist.id, error = %err, "playlist item index update failed");
                        }
                    }
                    Ok(AccessOutcome::Unavailable(AccessUnavailable::TemporarilyUnavailable)) => {
                        // Transient failure: do not advance the version or latch the
                        // playlist inaccessible, or the skip gate would never refetch it
                        // until the remote version changed. Leave it eligible next cycle.
                        tracing::debug!(
                            playlist = %playlist.id,
                            "playlist tracks temporarily unavailable; retrying next cycle"
                        );
                    }
                    Ok(AccessOutcome::Unavailable(reason)) => {
                        if let Err(mark_err) = ctx
                            .store()
                            .mark_playlist_tracks_inaccessible_at_version(
                                &playlist.id,
                                playlist.version_token.as_deref(),
                            )
                            .await
                        {
                            tracing::debug!(playlist = %playlist.id, error = %mark_err, "failed to mark playlist tracks inaccessible");
                        }
                        ctx.emit_event(DaemonEvent::PlaylistsChanged {
                            action: "tracks-inaccessible".to_string(),
                            playlist: Some(playlist.id.clone()),
                            provider: Some(provider.provider_id().clone()),
                        });
                        tracing::debug!(playlist = %playlist.id, ?reason, "playlist tracks unavailable");
                    }
                    Err(err) => {
                        if is_control_provider_error(&err) {
                            record_sync_error(ctx, provider, "playlists", started_at_ms, &err)
                                .await?;
                            return Err(err.into());
                        }
                        if playlist_tracks_forbidden(&err) {
                            if let Err(mark_err) = ctx
                                .store()
                                .mark_playlist_tracks_inaccessible_at_version(
                                    &playlist.id,
                                    playlist.version_token.as_deref(),
                                )
                                .await
                            {
                                tracing::debug!(
                                    playlist = %playlist.id,
                                    error = %mark_err,
                                    "failed to mark playlist tracks inaccessible"
                                );
                            }
                            ctx.emit_event(DaemonEvent::PlaylistsChanged {
                                action: "tracks-inaccessible".to_string(),
                                playlist: Some(playlist.id.clone()),
                                provider: Some(provider.provider_id().clone()),
                            });
                            tracing::debug!(playlist = %playlist.id, error = %err, "playlist tracks unavailable");
                            continue;
                        }
                        record_sync_error(ctx, provider, "playlists", started_at_ms, &err).await?;
                        return Err(err.into());
                    }
                }
            }
            ctx.store()
                .record_provider_sync_event_bulk(
                    provider.id(),
                    "playlists",
                    started_at_ms,
                    "ok",
                    summary.playlists,
                    None,
                )
                .await?;
            Ok(())
        }
        Err(err) => {
            record_sync_error(ctx, provider, "playlists", started_at_ms, &err).await?;
            Err(err.into())
        }
    }
}

fn normalize_provider_playlists(
    provider: &SyncProvider,
    playlists: Vec<Playlist>,
) -> Result<Vec<Playlist>> {
    playlists
        .into_iter()
        .map(|mut playlist| {
            let uri = match ResourceUri::parse(&playlist.id) {
                Ok(uri) => uri,
                Err(_) => ResourceUri::new(
                    provider.music.uri_scheme().clone(),
                    MediaKind::Playlist,
                    &playlist.id,
                )?,
            };
            if uri.kind() != MediaKind::Playlist || uri.scheme() != provider.music.uri_scheme() {
                return Err(ProviderError::InvalidInput {
                    field: "playlist".to_string(),
                    message: format!(
                        "provider {} returned foreign playlist `{}`",
                        provider.id(),
                        playlist.id
                    ),
                }
                .into());
            }
            playlist.id = uri.as_uri();
            Ok(playlist)
        })
        .collect()
}

fn playlist_tracks_forbidden(error: &ProviderError) -> bool {
    matches!(error, ProviderError::Forbidden { .. })
}

async fn sync_recent<C: SyncContext>(
    ctx: &C,
    provider: &SyncProvider,
    summary: &mut CacheSyncSummary,
) -> Result<()> {
    fail_if_rate_limited_domain(ctx, provider, "recent").await?;
    let started_at_ms = now_ms();
    let caps = provider.music.capabilities().catalog;
    if !caps.recently_played {
        return Err(ProviderError::unsupported("recently_played").into());
    }
    match fetch_all_recent(
        provider.music.as_ref(),
        page_limit(caps.recently_played_max_page_size),
    )
    .await
    {
        Ok(items) => {
            validate_provider_collection_items(
                provider,
                "recently_played",
                &[MediaKind::Track, MediaKind::Episode],
                &items,
            )?;
            summary.recent_items += ctx
                .store()
                .persist_provider_recent_items_bulk(provider.provider_id(), &items)
                .await?;
            summary.media_items += items.len() as u32;
            if let Err(err) = ctx.index_media_items(provider.id(), &items, false).await {
                tracing::debug!(error = %err, "recent item index update failed");
            }
            ctx.store()
                .record_provider_sync_event_bulk(
                    provider.id(),
                    "recent",
                    started_at_ms,
                    "ok",
                    items.len() as u32,
                    None,
                )
                .await?;
            Ok(())
        }
        Err(err) => {
            record_sync_error(ctx, provider, "recent", started_at_ms, &err).await?;
            Err(err.into())
        }
    }
}

async fn sync_library<C: SyncContext>(
    ctx: &C,
    provider: &SyncProvider,
    summary: &mut CacheSyncSummary,
    force_full: bool,
) -> Result<()> {
    fail_if_rate_limited_domain(ctx, provider, "library").await?;
    let started_at_ms = now_ms();
    let caps = provider.music.capabilities().library;
    let limit = page_limit(caps.max_page_size);
    for kind in caps.read_kinds {
        let cursor_domain = format!("library/{}", kind.label());
        let current_probe = if caps.freshness_probe {
            match provider
                .music
                .library_freshness_probe(RequestContext::BACKGROUND_SYNC, kind.clone())
                .await
            {
                Ok(probe) => Some(probe),
                Err(err) if is_control_provider_error(&err) => {
                    record_sync_error(ctx, provider, "library", started_at_ms, &err).await?;
                    return Err(err.into());
                }
                Err(err) => {
                    tracing::warn!(provider = provider.id(), %kind, error = %err, "library freshness probe failed; fetching full kind");
                    None
                }
            }
        } else {
            None
        };

        if !force_full {
            if let (Some(previous), Some(current)) = (
                ctx.store()
                    .sync_cursor(provider.id(), &cursor_domain)
                    .await?,
                current_probe.as_ref(),
            ) {
                if !provider
                    .music
                    .library_freshness_changed(&FreshnessProbe(previous), current)
                {
                    tracing::debug!(provider = provider.id(), %kind, "library kind unchanged; skipping full fetch");
                    continue;
                }
            }
        }

        let kind_items = match fetch_all_library(provider.music.as_ref(), kind.clone(), limit).await
        {
            Ok(items) => items,
            Err(err) => {
                record_sync_error(ctx, provider, "library", started_at_ms, &err).await?;
                return Err(err.into());
            }
        };
        validate_provider_collection_items(
            provider,
            "library_items",
            std::slice::from_ref(&kind),
            &kind_items,
        )?;

        let library_outcome = ctx
            .store()
            .replace_provider_library_kind_bulk(provider.id(), &kind, &kind_items)
            .await?;
        summary.library_items += library_outcome.written;
        summary.media_items += kind_items.len() as u32;
        if let Err(err) = ctx
            .index_media_items(provider.id(), &kind_items, true)
            .await
        {
            tracing::debug!(provider = provider.id(), %kind, error = %err, "library item index update failed");
        }
        if !library_outcome.removed_uris.is_empty() {
            let removed_items = ctx
                .store()
                .media_items_by_uris(&library_outcome.removed_uris)
                .await?;
            ctx.index_media_items(provider.id(), &removed_items, false)
                .await?;
        }
        if let Some(probe) = current_probe.as_ref() {
            ctx.store()
                .record_provider_sync_success_with_cursor_bulk(
                    provider.id(),
                    &cursor_domain,
                    started_at_ms,
                    library_outcome.written,
                    &probe.0,
                )
                .await?;
        }
    }
    ctx.store()
        .record_provider_sync_event_bulk(
            provider.id(),
            "library",
            started_at_ms,
            "ok",
            summary.library_items,
            None,
        )
        .await?;
    Ok(())
}

#[derive(Clone)]
enum MediaPageSource {
    Recent,
    Library(MediaKind),
}

fn page_limit(max: Option<usize>) -> u32 {
    max.unwrap_or(50).clamp(1, u32::MAX as usize) as u32
}

fn required_transport(provider: &SyncProvider) -> Result<Arc<dyn RemoteTransport>> {
    provider
        .transport
        .clone()
        .ok_or_else(|| ProviderError::unsupported("remote_transport").into())
}

fn validate_provider_queue(provider: &SyncProvider, queue: &Queue) -> Result<()> {
    if let Some(item) = queue.currently_playing.as_ref() {
        validate_provider_collection_items(
            provider,
            "queue",
            &[MediaKind::Track, MediaKind::Episode],
            std::slice::from_ref(item),
        )?;
    }
    validate_provider_collection_items(
        provider,
        "queue",
        &[MediaKind::Track, MediaKind::Episode],
        &queue.items,
    )
}

fn validate_provider_media_items(provider: &SyncProvider, items: &[MediaItem]) -> Result<()> {
    for item in items {
        validate_provider_media_item(provider, item)?;
    }
    Ok(())
}

fn validate_provider_collection_items(
    provider: &SyncProvider,
    operation: &str,
    allowed_kinds: &[MediaKind],
    items: &[MediaItem],
) -> Result<()> {
    validate_provider_media_items(provider, items)?;
    if let Some(item) = items
        .iter()
        .find(|item| !allowed_kinds.contains(&item.kind))
    {
        let expected = allowed_kinds
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" or ");
        return Err(ProviderError::InvalidInput {
            field: format!("{operation}.kind"),
            message: format!(
                "provider {} returned {} item `{}` for {operation}; expected {expected}",
                provider.id(),
                item.kind,
                item.uri
            ),
        }
        .into());
    }
    Ok(())
}

fn validate_provider_media_item(provider: &SyncProvider, item: &MediaItem) -> Result<()> {
    let uri = ResourceUri::parse(&item.uri).map_err(|error| ProviderError::InvalidInput {
        field: "media_item.uri".to_string(),
        message: format!(
            "provider {} returned `{}`: {error}",
            provider.id(),
            item.uri
        ),
    })?;
    if uri.scheme() != provider.music.uri_scheme() || uri.kind() != item.kind {
        return Err(ProviderError::InvalidInput {
            field: "media_item.uri".to_string(),
            message: format!(
                "provider {} returned foreign or mismatched media item `{}` ({})",
                provider.id(),
                item.uri,
                item.kind
            ),
        }
        .into());
    }
    Ok(())
}

fn should_refetch_playlist_tracks_for_provider(
    provider: &dyn MusicProvider,
    local: Option<&str>,
    remote: Option<&str>,
) -> bool {
    match (local, remote) {
        (Some(local), Some(remote)) => provider.playlist_version_changed(Some(local), Some(remote)),
        _ => should_refetch_playlist_tracks(local, remote),
    }
}

async fn fetch_all_playlists(
    provider: &dyn MusicProvider,
    limit: u32,
) -> Result<Vec<Playlist>, ProviderError> {
    let first_request = PageRequest::new(limit, 0);
    let first = provider
        .playlists(RequestContext::BACKGROUND_SYNC, first_request.clone())
        .await?;
    validate_provider_page_offset(&first_request, &first, "playlists")?;
    let mut requested_offset = first_request.offset;
    let mut items = first.items;
    let mut next = first.next;
    let mut logical_offset = items.len() as u64;
    let mut seen_cursors = std::collections::HashSet::new();
    for _ in 0..1_000 {
        let Some(continuation) = next else {
            return Ok(items);
        };
        let request = continuation_request(
            limit,
            requested_offset,
            logical_offset,
            continuation,
            &mut seen_cursors,
        )?;
        let page = provider
            .playlists(RequestContext::BACKGROUND_SYNC, request.clone())
            .await?;
        validate_provider_page_offset(&request, &page, "playlists")?;
        requested_offset = request.offset;
        logical_offset = logical_offset.saturating_add(page.items.len() as u64);
        items.extend(page.items);
        next = page.next;
    }
    Err(ProviderError::Provider(
        "playlist pagination exceeded 1000 pages".to_string(),
    ))
}

async fn fetch_all_recent(
    provider: &dyn MusicProvider,
    limit: u32,
) -> Result<Vec<MediaItem>, ProviderError> {
    let first_request = PageRequest::new(limit, 0);
    let first = provider
        .recently_played(RequestContext::BACKGROUND_SYNC, first_request.clone())
        .await?;
    validate_provider_page_offset(&first_request, &first, "recently_played")?;
    collect_media_pages(provider, MediaPageSource::Recent, limit, first).await
}

async fn fetch_all_library(
    provider: &dyn MusicProvider,
    kind: MediaKind,
    limit: u32,
) -> Result<Vec<MediaItem>, ProviderError> {
    let first_request = PageRequest::new(limit, 0);
    let first = provider
        .library_items(
            RequestContext::BACKGROUND_SYNC,
            LibraryRequest {
                kind: kind.clone(),
                page: first_request.clone(),
            },
        )
        .await?;
    validate_provider_page_offset(&first_request, &first, "library_items")?;
    collect_media_pages(provider, MediaPageSource::Library(kind), limit, first).await
}

async fn collect_media_pages(
    provider: &dyn MusicProvider,
    source: MediaPageSource,
    limit: u32,
    first: ProviderPage<MediaItem>,
) -> Result<Vec<MediaItem>, ProviderError> {
    let mut requested_offset = 0;
    let mut items = first.items;
    let mut next = first.next;
    let mut logical_offset = items.len() as u64;
    let mut seen_cursors = std::collections::HashSet::new();
    for _ in 0..1_000 {
        let Some(continuation) = next else {
            return Ok(items);
        };
        let request = continuation_request(
            limit,
            requested_offset,
            logical_offset,
            continuation,
            &mut seen_cursors,
        )?;
        let page = match &source {
            MediaPageSource::Recent => {
                provider
                    .recently_played(RequestContext::BACKGROUND_SYNC, request.clone())
                    .await?
            }
            MediaPageSource::Library(kind) => {
                provider
                    .library_items(
                        RequestContext::BACKGROUND_SYNC,
                        LibraryRequest {
                            kind: kind.clone(),
                            page: request.clone(),
                        },
                    )
                    .await?
            }
        };
        let operation = match &source {
            MediaPageSource::Recent => "recently_played",
            MediaPageSource::Library(_) => "library_items",
        };
        validate_provider_page_offset(&request, &page, operation)?;
        requested_offset = request.offset;
        logical_offset = logical_offset.saturating_add(page.items.len() as u64);
        items.extend(page.items);
        next = page.next;
    }
    Err(ProviderError::Provider(
        "media pagination exceeded 1000 pages".to_string(),
    ))
}

async fn fetch_all_playlist_items(
    provider: &dyn MusicProvider,
    uri: ResourceUri,
    limit: u32,
) -> Result<AccessOutcome<Vec<MediaItem>>, ProviderError> {
    let first_request = PageRequest::new(limit, 0);
    let first = provider
        .playlist_items(
            RequestContext::BACKGROUND_SYNC,
            CollectionRequest {
                uri: uri.clone(),
                page: first_request.clone(),
            },
        )
        .await?;
    let first = match first {
        AccessOutcome::Available(page) => page,
        AccessOutcome::Unavailable(reason) => {
            return Ok(AccessOutcome::Unavailable(reason));
        }
    };
    validate_provider_page_offset(&first_request, &first, "playlist_items")?;
    let mut requested_offset = first_request.offset;
    let mut items = first.items;
    let mut next = first.next;
    let mut logical_offset = items.len() as u64;
    let mut seen_cursors = std::collections::HashSet::new();
    for _ in 0..1_000 {
        let Some(continuation) = next else {
            return Ok(AccessOutcome::Available(items));
        };
        let request = continuation_request(
            limit,
            requested_offset,
            logical_offset,
            continuation,
            &mut seen_cursors,
        )?;
        match provider
            .playlist_items(
                RequestContext::BACKGROUND_SYNC,
                CollectionRequest {
                    uri: uri.clone(),
                    page: request.clone(),
                },
            )
            .await?
        {
            AccessOutcome::Available(page) => {
                validate_provider_page_offset(&request, &page, "playlist_items")?;
                requested_offset = request.offset;
                logical_offset = logical_offset.saturating_add(page.items.len() as u64);
                items.extend(page.items);
                next = page.next;
            }
            AccessOutcome::Unavailable(reason) => {
                return Ok(AccessOutcome::Unavailable(reason));
            }
        }
    }
    Err(ProviderError::Provider(
        "playlist item pagination exceeded 1000 pages".to_string(),
    ))
}

fn validate_provider_page_offset<T>(
    request: &PageRequest,
    page: &ProviderPage<T>,
    operation: &str,
) -> Result<(), ProviderError> {
    if page.requested_offset != request.offset {
        return Err(ProviderError::InvalidInput {
            field: format!("{operation}.requested_offset"),
            message: format!(
                "provider echoed offset {} for request offset {}",
                page.requested_offset, request.offset
            ),
        });
    }
    Ok(())
}

fn continuation_request(
    limit: u32,
    requested_offset: u64,
    logical_offset: u64,
    continuation: PageContinuation,
    seen_cursors: &mut std::collections::HashSet<String>,
) -> Result<PageRequest, ProviderError> {
    match continuation {
        PageContinuation::Offset(offset) if offset > requested_offset => {
            Ok(PageRequest::new(limit, offset))
        }
        PageContinuation::Offset(offset) => Err(ProviderError::Provider(format!(
            "provider pagination did not advance: {offset} <= {requested_offset}"
        ))),
        PageContinuation::Cursor(cursor) if seen_cursors.insert(cursor.clone()) => {
            Ok(PageRequest::with_cursor(limit, logical_offset, cursor))
        }
        PageContinuation::Cursor(cursor) => Err(ProviderError::Provider(format!(
            "provider pagination repeated cursor `{cursor}`"
        ))),
    }
}

fn playlist_as_media_item(provider_id: &str, playlist: &Playlist) -> Result<MediaItem> {
    let uri = ResourceUri::parse(&playlist.id)?;
    Ok(MediaItem {
        id: Some(playlist.id.clone()),
        uri: uri.as_uri(),
        name: playlist.name.clone(),
        subtitle: playlist.owner.clone(),
        context: format!("{} tracks", playlist.tracks_total),
        duration_ms: 0,
        image_url: playlist.image_url.clone(),
        kind: MediaKind::Playlist,
        source: Some(ItemSource::Provider(provider_id.to_string())),
        freshness: Some("fresh".to_string()),
        explicit: None,
        is_playable: None,
        ..Default::default()
    })
}

/// `true` when the post-poll playback diverges from the pre-poll
/// snapshot in a way the user would actually perceive: track URI
/// changed, play/pause toggled, device moved, shuffle/repeat flipped,
/// or progress jumped (someone seeked on another device). Steady-
/// state same-track playback returns `false` so the daemon doesn't
/// re-emit `PlaybackChanged` every 3-second poll cycle while nothing
/// is actually changing.
///
/// Clients extrapolate progress locally from the daemon's clock —
/// they don't need a periodic re-anchor event. The 5-second progress
/// tolerance catches real seeks on other devices without false-firing
/// on the natural 3s drift between polls.
fn playback_diff_is_meaningful(before: &Playback, after: &Playback) -> bool {
    let before_uri = before.item.as_ref().map(|i| i.uri.as_str());
    let after_uri = after.item.as_ref().map(|i| i.uri.as_str());
    if before_uri != after_uri {
        return true;
    }
    if let (Some(before_item), Some(after_item)) = (&before.item, &after.item) {
        if before_item.name != after_item.name
            || before_item.subtitle != after_item.subtitle
            || before_item.context != after_item.context
            || before_item.duration_ms != after_item.duration_ms
            || before_item.image_url != after_item.image_url
        {
            return true;
        }
    }
    if before.is_playing != after.is_playing {
        return true;
    }
    let before_device = before.device.as_ref().and_then(|d| d.id.as_deref());
    let after_device = after.device.as_ref().and_then(|d| d.id.as_deref());
    if before_device != after_device {
        return true;
    }
    if before.shuffle != after.shuffle {
        return true;
    }
    if before.repeat != after.repeat {
        return true;
    }
    (after.progress_ms as i64 - before.progress_ms as i64).abs() > 5_000
}

/// `true` when the queue's currently-playing URI or upcoming-item
/// URIs differ between the pre- and post-poll snapshot. Same idea as
/// `playback_diff_is_meaningful` — the daemon polls every 3s for
/// freshness but the queue rarely actually changes; subscribers don't
/// need a re-render when it didn't.
fn queue_diff_is_meaningful(before: &Queue, after: &Queue) -> bool {
    let before_now = before.currently_playing.as_ref().map(|i| i.uri.as_str());
    let after_now = after.currently_playing.as_ref().map(|i| i.uri.as_str());
    if before_now != after_now {
        return true;
    }
    if before.items.len() != after.items.len() {
        return true;
    }
    before
        .items
        .iter()
        .zip(after.items.iter())
        .any(|(b, a)| b.uri != a.uri)
}

/// `true` when the device list changed in a user-visible way:
/// devices added/removed, or the active flag flipped on any device.
/// Volume-only changes are user-perceived but they round-trip through
/// the dedicated `Volume` command path; periodic device polls don't
/// need to re-broadcast volume noise.
fn devices_diff_is_meaningful(
    before: &[spotuify_core::Device],
    after: &[spotuify_core::Device],
) -> bool {
    if before.len() != after.len() {
        return true;
    }
    let key = |d: &spotuify_core::Device| (d.id.clone(), d.is_active);
    let mut before_keys: Vec<_> = before.iter().map(key).collect();
    let mut after_keys: Vec<_> = after.iter().map(key).collect();
    before_keys.sort();
    after_keys.sort();
    before_keys != after_keys
}

async fn fail_if_rate_limited_domain<C: SyncContext>(
    ctx: &C,
    provider: &SyncProvider,
    domain: &str,
) -> Result<()> {
    if let Some(remaining_ms) = ctx
        .store()
        .provider_rate_limit_max_cooldown_remaining_ms(provider.id())
        .await?
    {
        tracing::debug!(
            provider = provider.id(),
            domain,
            remaining_ms,
            "skipping sync while provider rate limit cooldown is active"
        );
        return Err(ProviderError::RateLimited {
            retry_after: Some(Duration::from_millis(remaining_ms as u64)),
            scope: Some(domain.to_string()),
        }
        .into());
    }
    Ok(())
}

async fn record_sync_error<C: SyncContext>(
    ctx: &C,
    provider: &SyncProvider,
    domain: &str,
    started_at_ms: i64,
    err: &ProviderError,
) -> Result<()> {
    ctx.store()
        .record_provider_sync_event_bulk_with_retry_after(
            provider.id(),
            domain,
            started_at_ms,
            spotuify_store::ProviderSyncEventOutcome {
                status: "error",
                row_count: 0,
                error: Some(&err.to_string()),
                retry_after_secs: retry_after_secs(err),
            },
        )
        .await
}

fn is_control_provider_error(err: &ProviderError) -> bool {
    err.is_auth_error()
        || matches!(
            err,
            ProviderError::RateLimited { .. } | ProviderError::SyncTokenExpired { .. }
        )
}

fn retry_after_secs(err: &ProviderError) -> Option<u64> {
    let ProviderError::RateLimited {
        retry_after: Some(retry_after),
        ..
    } = err
    else {
        return None;
    };
    let millis = retry_after.as_millis();
    Some(millis.div_ceil(1000).max(1).min(u128::from(u64::MAX)) as u64)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn devices_and_queue_poll_slower_than_playback() {
        // Regression guard: polling `/me/player/devices` at the fast playback
        // cadence is what rate-limited the account over a long TUI session.
        assert!(DEVICES_CADENCE > ACTIVE_CADENCE);
        assert!(QUEUE_CADENCE > ACTIVE_CADENCE);
        assert!(DEVICES_CADENCE >= QUEUE_CADENCE);
    }

    #[test]
    fn every_provider_timeout_is_bounded_below_ten_minutes() {
        assert!(SLOW_TARGET_TIMEOUT > PER_TARGET_TIMEOUT);
        assert!(SLOW_TARGET_TIMEOUT < Duration::from_secs(600));
        assert_eq!(SLOW_TARGET_TIMEOUT, Duration::from_secs(9 * 60));
    }

    #[test]
    fn scheduler_restart_failures_reset_only_after_a_stable_run() {
        assert_eq!(
            scheduler_restart_failures_after_run(
                3,
                SCHEDULER_RESTART_RESET_AFTER - Duration::from_secs(1)
            ),
            3
        );
        assert_eq!(
            scheduler_restart_failures_after_run(3, SCHEDULER_RESTART_RESET_AFTER),
            0
        );
    }

    #[test]
    fn target_backoff_skips_until_delay_then_resets_on_success() {
        let now = tokio::time::Instant::now();
        let mut backoff = TargetBackoff::default();
        assert!(backoff.should_run(now));

        let err = anyhow::anyhow!("transient failure");
        backoff.record_failure(now, &err);
        assert!(!backoff.should_run(now + Duration::from_secs(1)));
        assert!(backoff.should_run(now + BACKOFF_BASE));

        backoff.record_success();
        assert!(backoff.should_run(now + Duration::from_secs(1)));
    }

    #[test]
    fn global_cooldown_escalates_on_consecutive_429s_and_resets_on_success() {
        // The circuit breaker: each consecutive 429 doubles the daemon's
        // own backoff (floored at the 60s base) so it can't keep an
        // escalated account alive by probing every minute. A success
        // resets it.
        let now = tokio::time::Instant::now();
        let mut global = TargetBackoff::default();
        // Server retry-after 30s, but the base floor is 60s.
        global.note_rate_limit(now, Duration::from_secs(30));
        assert!(!global.should_run(now + Duration::from_secs(59)));
        assert!(global.should_run(now + Duration::from_secs(60)));
        // Second consecutive 429 → 120s.
        global.note_rate_limit(now, Duration::from_secs(30));
        assert!(!global.should_run(now + Duration::from_secs(119)));
        assert!(global.should_run(now + Duration::from_secs(120)));
        // Third → 240s.
        global.note_rate_limit(now, Duration::from_secs(30));
        assert!(!global.should_run(now + Duration::from_secs(239)));
        assert!(global.should_run(now + Duration::from_secs(240)));
        // A success drops the escalation back to the 60s base.
        global.record_success();
        global.note_rate_limit(now, Duration::from_secs(30));
        assert!(!global.should_run(now + Duration::from_secs(59)));
        assert!(global.should_run(now + Duration::from_secs(60)));
    }

    #[test]
    fn global_cooldown_caps_at_max() {
        let now = tokio::time::Instant::now();
        let mut global = TargetBackoff::default();
        for _ in 0..10 {
            global.note_rate_limit(now, Duration::from_secs(30));
        }
        assert!(!global.should_run(now + GLOBAL_BACKOFF_MAX - Duration::from_secs(1)));
        assert!(global.should_run(now + GLOBAL_BACKOFF_MAX));
    }

    #[test]
    fn global_cooldown_honors_server_retry_after_as_floor() {
        // When Spotify asks for longer than our escalated base, honor it.
        let now = tokio::time::Instant::now();
        let mut global = TargetBackoff::default();
        global.note_rate_limit(now, Duration::from_secs(300));
        assert!(!global.should_run(now + Duration::from_secs(299)));
        assert!(global.should_run(now + Duration::from_secs(300)));
    }

    #[test]
    fn stale_success_cannot_clear_a_newer_rate_limit_epoch() {
        let now = tokio::time::Instant::now();
        let mut global = TargetBackoff::default();
        let stale_epoch = global.epoch();
        global.note_rate_limit(now, Duration::from_secs(60));

        global.record_success_if_epoch(stale_epoch);

        assert!(!global.should_run(now + Duration::from_secs(59)));
        let current_epoch = global.epoch();
        global.record_success_if_epoch(current_epoch);
        assert!(global.should_run(now));
    }

    #[test]
    fn retry_after_starts_when_delayed_rate_limit_response_completes() {
        let started = tokio::time::Instant::now();
        let completed = started + Duration::from_secs(30);
        let global = Mutex::new(TargetBackoff::default());
        let result: Option<Result<CacheSyncSummary>> =
            Some(Err(anyhow::Error::new(ProviderError::RateLimited {
                retry_after: Some(Duration::from_secs(60)),
                scope: Some("library".to_string()),
            })));

        record_global_at(&global, completed, 0, &result);

        let global = global.lock().unwrap();
        assert!(!global.should_run(started + Duration::from_secs(89)));
        assert!(global.should_run(started + Duration::from_secs(90)));
    }

    #[test]
    fn rate_limit_retry_after_extracts_429_only() {
        let rate_limited: Option<Result<CacheSyncSummary>> =
            Some(Err(anyhow::Error::new(ProviderError::RateLimited {
                retry_after: Some(Duration::from_secs(30)),
                scope: Some("playback".to_string()),
            })));
        assert_eq!(
            rate_limit_retry_after(&rate_limited),
            Some(Duration::from_secs(30))
        );

        let without_hint: Option<Result<CacheSyncSummary>> =
            Some(Err(anyhow::Error::new(ProviderError::RateLimited {
                retry_after: None,
                scope: Some("library".to_string()),
            })));
        assert_eq!(
            rate_limit_retry_after(&without_hint),
            Some(GLOBAL_BACKOFF_BASE)
        );

        let other: Option<Result<CacheSyncSummary>> = Some(Err(anyhow::anyhow!("network blip")));
        assert_eq!(rate_limit_retry_after(&other), None);
        assert_eq!(rate_limit_retry_after(&None), None);
    }

    #[test]
    fn target_backoff_honors_provider_retry_after() {
        let now = tokio::time::Instant::now();
        let mut backoff = TargetBackoff::default();
        let retry_after = Duration::from_secs(60 * 60);
        let err = anyhow::Error::new(ProviderError::RateLimited {
            retry_after: Some(retry_after),
            scope: Some("playback".to_string()),
        });

        backoff.record_failure(now, &err);

        assert!(!backoff.should_run(now + BACKOFF_MAX + Duration::from_secs(1)));
        assert!(backoff.should_run(now + retry_after));
    }
}
