//! Phase 7 — sync engine. Moved out of the binary's `src/sync.rs`
//! once the `SyncContext` trait broke the cycle with `DaemonState`.
//!
//! All public functions are generic over `&impl SyncContext`. The
//! binary's wrapper supplies an `Arc<DaemonState>` (which impls
//! `SyncContext`) and the sync loop runs against the daemon's live
//! Spotify client, store, and event broadcaster -- no longer
//! compile-coupled to the daemon module.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use spotuify_core::{now_ms, MediaItem, MediaKind, Playback, Playlist};
use spotuify_protocol::{CacheSyncSummary, DaemonEvent, SyncTargetData};
use spotuify_spotify::SpotifyError;
use tokio::task::JoinHandle;

use crate::{should_refetch_playlist_tracks, should_refetch_saved_tracks, SyncContext};

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
const SLOW_TARGET_TIMEOUT: Duration = Duration::from_secs(30 * 60);
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

/// Recently played is useful for "last played" hints, but it is not
/// part of the live transport loop. Keep it on a slower cadence so a
/// slow or rate-limited endpoint cannot pin the hot poll path.
const RECENT_ACTIVE_CADENCE: Duration = Duration::from_secs(5 * 60);

/// Idle poll cadence when no client is subscribed. Keeps the cache
/// somewhat fresh for the next launch without burning API quota.
const IDLE_CADENCE: Duration = Duration::from_secs(30);

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
pub fn spawn_background_scheduler<C>(ctx: Arc<C>) -> Vec<JoinHandle<()>>
where
    C: SyncContext + 'static,
{
    // Intentionally NOT routing onto the host's dedicated bg runtime
    // here, even though `ctx.background_runtime()` is now exposed.
    // The Spotify HTTP client (reqwest + hyper) memoised by the
    // daemon is lazily built on whichever runtime calls
    // `spotify_client` first; hyper's connection-driver tasks are
    // pinned to that runtime via `tokio::spawn`. Cross-runtime use
    // can leave the request future awaiting on a driver task that's
    // owned by a different runtime, which under HTTP/2 connection
    // pooling has been observed to hang indefinitely. Keep
    // long-running sync loops on the same (main) runtime as the
    // request handlers so the reqwest pool's tasks always live where
    // their futures are awaited. The bg runtime is still useful for
    // pure-DB background work (see daemon retention loop).
    // Shared across the fast and slow loops so a 429 on either pauses the
    // other. Startup is the worst case: the fast warm (4 requests in
    // ~400ms) and the slow loop's first tick (4 more 60s later) used to
    // both burst into an already-tight rolling window; now the second
    // burst is gated by the first's cooldown.
    let global_backoff = Arc::new(Mutex::new(TargetBackoff::default()));
    let fast_global = global_backoff.clone();
    let slow_global = global_backoff;

    let fast_ctx = ctx.clone();
    let fast_future = async move {
        let mut shutdown_rx = fast_ctx.shutdown_receiver();
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
        let global_backoff = fast_global;
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let has_subscribers = fast_ctx.event_subscriber_count() > 0;
                    let elapsed = last_sync.map_or(IDLE_CADENCE, |t| t.elapsed());
                    if !has_subscribers && elapsed < IDLE_CADENCE {
                        continue;
                    }
                    let now = tokio::time::Instant::now();
                    if !global_backoff.lock().expect("global backoff lock").should_run(now) {
                        tracing::debug!("fast sync paused: global Spotify rate-limit cooldown");
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
                    let playback_due = if fast_ctx.embedded_is_active_playback() {
                        last_playback_sync
                            .is_none_or(|t| t.elapsed() >= PLAYBACK_RECONCILE_CADENCE)
                    } else if !fast_ctx.snapshot_playback().is_playing {
                        last_playback_sync.is_none_or(|t| t.elapsed() >= IDLE_CADENCE)
                    } else {
                        true
                    };
                    if playback_due {
                        let pb = sync_target_with_backoff(
                            fast_ctx.as_ref(),
                            SyncTargetData::Playback,
                            &mut playback_backoff,
                        )
                        .await;
                        record_global(&global_backoff, now, &pb);
                        log_background_result(SyncTargetData::Playback, &pb);
                        last_playback_sync = Some(now);
                    }

                    if global_backoff.lock().expect("global backoff lock").should_run(now)
                        && last_queue_sync.is_none_or(|t| t.elapsed() >= QUEUE_CADENCE)
                    {
                        let q = sync_target_with_backoff(
                            fast_ctx.as_ref(),
                            SyncTargetData::Queue,
                            &mut queue_backoff,
                        )
                        .await;
                        record_global(&global_backoff, now, &q);
                        log_background_result(SyncTargetData::Queue, &q);
                        last_queue_sync = Some(now);
                    }

                    if global_backoff.lock().expect("global backoff lock").should_run(now)
                        && last_devices_sync.is_none_or(|t| t.elapsed() >= DEVICES_CADENCE)
                    {
                        let d = sync_target_with_backoff(
                            fast_ctx.as_ref(),
                            SyncTargetData::Devices,
                            &mut devices_backoff,
                        )
                        .await;
                        record_global(&global_backoff, now, &d);
                        log_background_result(SyncTargetData::Devices, &d);
                        last_devices_sync = Some(now);
                    }

                    let recent_due = last_recent_sync
                        .is_none_or(|last| last.elapsed() >= RECENT_ACTIVE_CADENCE);
                    if global_backoff.lock().expect("global backoff lock").should_run(now) && recent_due {
                        let r = sync_target_with_backoff(
                            fast_ctx.as_ref(),
                            SyncTargetData::Recent,
                            &mut recent_backoff,
                        )
                        .await;
                        record_global(&global_backoff, now, &r);
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
    };

    let slow_ctx = ctx;
    let slow_future = async move {
        let mut shutdown_rx = slow_ctx.shutdown_receiver();
        let mut interval = tokio::time::interval_at(
            tokio::time::Instant::now() + SLOW_INITIAL_DELAY,
            SLOW_CADENCE,
        );
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let global_backoff = slow_global;
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let now = tokio::time::Instant::now();
                    if !global_backoff.lock().expect("global backoff lock").should_run(now) {
                        tracing::debug!("slow sync paused: global Spotify rate-limit cooldown");
                        continue;
                    }
                    let playlists = sync_target_with_timeout(
                        slow_ctx.as_ref(),
                        SyncTargetData::Playlists,
                        SLOW_TARGET_TIMEOUT,
                    ).await;
                    if let Err(err) = &playlists {
                        tracing::warn!(error = %err, "background playlists sync failed");
                    }
                    // Bail before firing the library sync if playlists hit
                    // a 429; otherwise the slow loop's first tick double-
                    // bursts an already-tight window (4+ requests in
                    // <500ms) before the global cooldown can kick in.
                    if record_global_slow(&global_backoff, now, &playlists) {
                        continue;
                    }
                    let library = sync_target_with_timeout(
                        slow_ctx.as_ref(),
                        SyncTargetData::Library,
                        SLOW_TARGET_TIMEOUT,
                    ).await;
                    if let Err(err) = &library {
                        tracing::warn!(error = %err, "background library sync failed");
                    }
                    record_global_slow(&global_backoff, now, &library);
                }
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow_and_update() {
                        break;
                    }
                }
            }
        }
    };

    let fast = tokio::spawn(fast_future);
    let slow = tokio::spawn(slow_future);
    vec![fast, slow]
}

#[derive(Debug, Default)]
struct TargetBackoff {
    failures: u32,
    next_allowed: Option<tokio::time::Instant>,
}

impl TargetBackoff {
    fn should_run(&self, now: tokio::time::Instant) -> bool {
        self.next_allowed.is_none_or(|next| now >= next)
    }

    fn record_success(&mut self) {
        self.failures = 0;
        self.next_allowed = None;
    }

    /// Trip the shared global cooldown on a 429, escalating the pause on
    /// consecutive hits. Spotify's per-request `Retry-After` is the floor;
    /// each consecutive 429 doubles the daemon's own backoff
    /// (`GLOBAL_BACKOFF_BASE` × 2^(n-1), capped at `GLOBAL_BACKOFF_MAX`)
    /// so a deeply-throttled account isn't kept alive by once-a-minute
    /// probing. [`record_success`] resets the escalation.
    fn note_rate_limit(&mut self, now: tokio::time::Instant, retry_after: Duration) {
        self.failures = self.failures.saturating_add(1);
        let shift = self.failures.saturating_sub(1).min(4);
        let escalated = GLOBAL_BACKOFF_BASE
            .saturating_mul(1 << shift)
            .min(GLOBAL_BACKOFF_MAX);
        let pause = retry_after.max(escalated).min(GLOBAL_BACKOFF_MAX);
        self.next_allowed = Some(now + pause);
    }

    fn record_failure(&mut self, now: tokio::time::Instant, err: &anyhow::Error) {
        self.failures = self.failures.saturating_add(1);
        if let Some(SpotifyError::RateLimited { retry_after, scope }) =
            err.downcast_ref::<SpotifyError>()
        {
            self.next_allowed = Some(now + *retry_after);
            tracing::debug!(
                scope,
                retry_after_secs = retry_after.as_secs(),
                "background sync target honoring Spotify rate-limit cooldown"
            );
            return;
        }
        let multiplier = 1_u64 << self.failures.saturating_sub(1).min(3);
        let delay =
            Duration::from_secs((BACKOFF_BASE.as_secs() * multiplier).min(BACKOFF_MAX.as_secs()));
        self.next_allowed = Some(now + delay);
    }
}

async fn sync_target_with_backoff<C: SyncContext>(
    ctx: &C,
    target: SyncTargetData,
    backoff: &mut TargetBackoff,
) -> Option<Result<CacheSyncSummary>> {
    let now = tokio::time::Instant::now();
    if !backoff.should_run(now) {
        tracing::debug!(target = target.label(), "background sync target in backoff");
        return None;
    }
    let result = sync_target_with_timeout(ctx, target, PER_TARGET_TIMEOUT).await;
    match &result {
        Ok(_) => backoff.record_success(),
        Err(err) => backoff.record_failure(tokio::time::Instant::now(), err),
    }
    Some(result)
}

async fn sync_target_with_timeout<C: SyncContext>(
    ctx: &C,
    target: SyncTargetData,
    timeout: Duration,
) -> Result<CacheSyncSummary> {
    tokio::time::timeout(timeout, sync_target(ctx, target))
        .await
        .unwrap_or_else(|_| {
            Err(anyhow::anyhow!(
                "timed out syncing {} after {}s",
                target.label(),
                timeout.as_secs()
            ))
        })
}

fn log_background_result(target: SyncTargetData, result: &Option<Result<CacheSyncSummary>>) {
    if let Some(Err(err)) = result {
        tracing::debug!(target = target.label(), error = %err, "background sync failed");
    }
}

/// Spotify's `Retry-After` from a sync result that failed with a 429,
/// if any. Fed into the shared global cooldown so one rate-limited lane
/// pauses every lane.
fn rate_limit_retry_after(result: &Option<Result<CacheSyncSummary>>) -> Option<Duration> {
    if let Some(Err(err)) = result {
        if let Some(SpotifyError::RateLimited { retry_after, .. }) =
            err.downcast_ref::<SpotifyError>()
        {
            return Some(*retry_after);
        }
    }
    None
}

/// Feed a lane's result into the shared global cooldown: a 429 escalates
/// the daemon-wide pause; a success resets the escalation (the account
/// is healthy again). A skipped lane (`None`) is neither — no signal.
/// The cooldown is shared across the fast and slow loops via `Arc<Mutex>`
/// so the slow loop honors the fast loop's backoff and vice versa.
fn record_global(
    global: &Mutex<TargetBackoff>,
    now: tokio::time::Instant,
    result: &Option<Result<CacheSyncSummary>>,
) {
    let mut g = global.lock().expect("global backoff lock");
    if let Some(retry) = rate_limit_retry_after(result) {
        g.note_rate_limit(now, retry);
    } else if matches!(result, Some(Ok(_))) {
        g.record_success();
    }
}

/// Feed a slow-loop result (which returns `Result<CacheSyncSummary>`
/// directly, no `Option`) into the shared global cooldown. Returns
/// `true` when the caller should bail the rest of this tick — a 429
/// just escalated the global pause.
fn record_global_slow(
    global: &Mutex<TargetBackoff>,
    now: tokio::time::Instant,
    result: &Result<CacheSyncSummary>,
) -> bool {
    let mut g = global.lock().expect("global backoff lock");
    match result {
        Ok(_) => {
            g.record_success();
            false
        }
        Err(err) => {
            if let Some(SpotifyError::RateLimited { retry_after, .. }) =
                err.downcast_ref::<SpotifyError>()
            {
                g.note_rate_limit(now, *retry_after);
                true
            } else {
                false
            }
        }
    }
}

/// Run one sync pass for the given target. Used both by the background
/// scheduler and by the `spotuify sync` CLI command.
pub async fn sync_target<C: SyncContext>(
    ctx: &C,
    target: SyncTargetData,
) -> Result<CacheSyncSummary> {
    let _sync_guard = match ctx.sync_lock_for(target) {
        Some(lock) => Some(lock.lock_owned().await),
        None => None,
    };
    ctx.emit_event(DaemonEvent::SyncStarted { target });
    let mut summary = CacheSyncSummary {
        target,
        playback_snapshots: 0,
        queue_snapshots: 0,
        queue_items: 0,
        devices: 0,
        playlists: 0,
        playlist_items: 0,
        recent_items: 0,
        library_items: 0,
        media_items: 0,
    };

    match target {
        SyncTargetData::All => {
            sync_playback(ctx, &mut summary).await?;
            sync_queue(ctx, &mut summary).await?;
            sync_devices(ctx, &mut summary).await?;
            sync_playlists(ctx, &mut summary).await?;
            sync_recent(ctx, &mut summary).await?;
            sync_library(ctx, &mut summary).await?;
        }
        SyncTargetData::Playback => sync_playback(ctx, &mut summary).await?,
        SyncTargetData::Queue => sync_queue(ctx, &mut summary).await?,
        SyncTargetData::Devices => sync_devices(ctx, &mut summary).await?,
        SyncTargetData::Playlists => sync_playlists(ctx, &mut summary).await?,
        SyncTargetData::Recent => sync_recent(ctx, &mut summary).await?,
        SyncTargetData::Library => sync_library(ctx, &mut summary).await?,
    }

    ctx.emit_event(DaemonEvent::SyncFinished {
        summary: summary.clone(),
    });
    Ok(summary)
}

async fn sync_queue<C: SyncContext>(ctx: &C, summary: &mut CacheSyncSummary) -> Result<()> {
    fail_if_rate_limited_domain(ctx, "queue").await?;
    let started_at_ms = now_ms();
    // Capture seq pre-call so a QueueAdd / Next / Previous racing
    // with this poll can be detected; see sync_playback for the same
    // pattern.
    let pre_seq = ctx.observe_mutation_seq();
    let mut client = ctx.spotify_client().await?;
    match client.queue().await {
        Ok(queue) => {
            if !ctx.may_apply_playback_update(pre_seq) {
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
                // taking it after `persist_queue_bulk` would observe
                // the just-written queue and the diff would always
                // collapse to "no change".
                let before_queue = ctx.snapshot_queue().await;
                // Same merge as the daemon's refresh apply path —
                // Spotify caps the queue endpoint at ~20 items and
                // embedded librespot often reports zero upcoming, so
                // persisting the raw fetch here wiped the cached
                // context tail within one queue cadence (live bug).
                let anchor = spotuify_core::queue_merge::queue_tail_anchor(&queue);
                let now = now_ms();
                let queue = ctx.overlay_pending_queue_appends(queue, now);
                let queue = spotuify_core::queue_merge::reattach_cached_queue_tail(
                    queue,
                    anchor.as_deref(),
                    &before_queue,
                    ctx.snapshot_playback().shuffle,
                    now,
                );
                summary.queue_snapshots += ctx.store().persist_queue_bulk(&queue).await?;
                summary.queue_items += queue.items.len() as u32;
                let mut items = Vec::with_capacity(queue.items.len() + 1);
                if let Some(item) = queue.currently_playing.as_ref() {
                    items.push(item.clone());
                }
                items.extend(queue.items.iter().cloned());
                if !items.is_empty() {
                    summary.media_items += items.len() as u32;
                    if let Err(err) = ctx.index_media_items(&items, false).await {
                        tracing::debug!(error = %err, "queue index update failed");
                    }
                }
                ctx.warm_queue(&queue);
                // Diff-then-broadcast: only emit when the queue
                // actually changed (currently-playing URI or item
                // list). Periodic polls during steady-state playback
                // re-fetch the same queue every 3s — clients don't
                // need to know.
                let mut after_queue = ctx.snapshot_queue().await;
                // Cache-backed snapshot implementations may return
                // session_active=false because storage alone cannot know
                // whether the live session still holds. This fresh probe can,
                // so force the broadcast copy active.
                after_queue.session_active = true;
                if queue_diff_is_meaningful(&before_queue, &after_queue) {
                    ctx.emit_event(DaemonEvent::QueueChanged {
                        action: "synced".to_string(),
                        uris: queue.items.iter().map(|i| i.uri.clone()).collect(),
                        queue: Some(after_queue),
                    });
                }
            }
            ctx.store()
                .record_sync_event_bulk(
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
            record_sync_error(ctx, "queue", started_at_ms, &err).await?;
            Err(err.into())
        }
    }
}

async fn sync_playback<C: SyncContext>(ctx: &C, summary: &mut CacheSyncSummary) -> Result<()> {
    fail_if_rate_limited_domain(ctx, "playback").await?;
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
    let mut client = ctx.spotify_client().await?;
    match client.playback().await {
        Ok(playback) => {
            if !ctx.may_apply_playback_update(pre_seq) {
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
                let before = ctx.snapshot_playback();
                let state_seq = ctx.observe_mutation_seq();
                let upstream_has_live_signal = playback_has_live_signal(&playback);
                let applied = ctx.apply_playback_poll(
                    &playback,
                    pre_seq,
                    state_seq,
                    now_ms(),
                    playback.provider_timestamp_ms,
                );
                let after = applied.then(|| ctx.snapshot_playback());
                let playback_to_persist = if upstream_has_live_signal {
                    Some(playback.clone())
                } else {
                    after.clone()
                };
                if let Some(playback_to_persist) = playback_to_persist.as_ref() {
                    summary.playback_snapshots += ctx
                        .store()
                        .persist_playback_bulk(playback_to_persist)
                        .await?;
                    if let Some(item) = playback_to_persist.item.as_ref() {
                        summary.media_items += 1;
                        if let Err(err) = ctx
                            .index_media_items(std::slice::from_ref(item), false)
                            .await
                        {
                            tracing::debug!(error = %err, "playback item index update failed");
                        }
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
            }
            ctx.store()
                .record_sync_event_bulk(
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
            record_sync_error(ctx, "playback", started_at_ms, &err).await?;
            Err(err.into())
        }
    }
}

async fn sync_devices<C: SyncContext>(ctx: &C, summary: &mut CacheSyncSummary) -> Result<()> {
    fail_if_rate_limited_domain(ctx, "devices").await?;
    let started_at_ms = now_ms();
    let pre_seq = ctx.observe_mutation_seq();
    let mut client = ctx.spotify_client().await?;
    match client.devices().await {
        Ok(devices) => {
            if !ctx.may_apply_playback_update(pre_seq) {
                tracing::debug!("dropping stale devices poll: mutation in flight");
            } else {
                // Capture pre-poll snapshot BEFORE persisting so the
                // diff sees the old device set.
                let before_devices = ctx.snapshot_devices().await;
                summary.devices += ctx.store().replace_devices(&devices).await?;
                let after_devices = ctx.snapshot_devices().await;
                // Diff-then-broadcast: skip when the device list +
                // active-flag set hasn't changed. Echo dots and the
                // local librespot session re-announce on every poll
                // but their identity is stable; only really emit on
                // add/remove/active-flip.
                if devices_diff_is_meaningful(&before_devices, &after_devices) {
                    ctx.emit_event(DaemonEvent::DevicesChanged {
                        action: "synced".to_string(),
                        devices: Some(after_devices),
                    });
                }
            }
            ctx.store()
                .record_sync_event_bulk("devices", started_at_ms, "ok", devices.len() as u32, None)
                .await?;
            Ok(())
        }
        Err(err) => {
            record_sync_error(ctx, "devices", started_at_ms, &err).await?;
            Err(err.into())
        }
    }
}

async fn sync_playlists<C: SyncContext>(ctx: &C, summary: &mut CacheSyncSummary) -> Result<()> {
    fail_if_rate_limited_domain(ctx, "playlists").await?;
    let started_at_ms = now_ms();
    let mut client = ctx.spotify_client().await?;
    match client.playlists().await {
        Ok(playlists) => {
            let mut local_snapshots = std::collections::HashMap::new();
            for playlist in &playlists {
                let local_snapshot = ctx
                    .store()
                    .playlist_snapshot_id(&playlist.id)
                    .await
                    .ok()
                    .flatten();
                local_snapshots.insert(playlist.id.clone(), local_snapshot);
            }
            summary.playlists += ctx.store().persist_playlists_bulk(&playlists).await?;
            summary.media_items += playlists.len() as u32;
            let playlist_media = playlists
                .iter()
                .map(playlist_as_media_item)
                .collect::<Vec<_>>();
            if let Err(err) = ctx.index_media_items(&playlist_media, false).await {
                tracing::debug!(error = %err, "playlist index update failed");
            }
            // Phase 6.5: snapshot_id refetch gate, with a self-heal
            // override. If the local snapshot matches Spotify's but
            // the cached playlist has zero items while Spotify
            // reports a non-empty playlist, we treat it as a cache
            // corruption (a previous sync's persist crashed mid-write
            // and left the playlist empty under an up-to-date
            // snapshot). Force-refetch in that case so a single bad
            // sync run can't strand the playlist empty forever.
            for playlist in &playlists {
                if !ctx
                    .store()
                    .playlist_tracks_accessible(&playlist.id)
                    .await
                    .unwrap_or(true)
                {
                    tracing::debug!(
                        playlist = %playlist.id,
                        "playlist tracks inaccessible; skipping tracks refetch"
                    );
                    continue;
                }
                let local_snapshot = local_snapshots
                    .get(&playlist.id)
                    .and_then(|snapshot| snapshot.as_deref());
                let snapshot_changed =
                    should_refetch_playlist_tracks(local_snapshot, playlist.snapshot_id.as_deref());
                let cache_empty_but_remote_has_tracks = !snapshot_changed
                    && playlist.tracks_total > 0
                    && ctx
                        .store()
                        .playlist_items_count(&playlist.id)
                        .await
                        .unwrap_or(0)
                        == 0;
                let needs_refetch = snapshot_changed || cache_empty_but_remote_has_tracks;
                if !needs_refetch {
                    tracing::debug!(
                        playlist = %playlist.id,
                        snapshot = %playlist.snapshot_id.as_deref().unwrap_or(""),
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
                match client.playlist_tracks(&playlist.id).await {
                    Ok(items) => {
                        summary.playlist_items += ctx
                            .store()
                            .persist_playlist_items_bulk(&playlist.id, &items)
                            .await?;
                        summary.media_items += items.len() as u32;
                        if let Err(err) = ctx.index_media_items(&items, false).await {
                            tracing::debug!(playlist = %playlist.id, error = %err, "playlist item index update failed");
                        }
                    }
                    Err(err) => {
                        if playlist_tracks_forbidden(&err) {
                            if let Err(mark_err) = ctx
                                .store()
                                .mark_playlist_tracks_inaccessible(&playlist.id)
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
                            });
                        }
                        tracing::warn!(playlist = %playlist.id, error = %err, "playlist item sync failed")
                    }
                }
            }
            ctx.store()
                .record_sync_event_bulk("playlists", started_at_ms, "ok", summary.playlists, None)
                .await?;
            Ok(())
        }
        Err(err) => {
            record_sync_error(ctx, "playlists", started_at_ms, &err).await?;
            Err(err.into())
        }
    }
}

fn playlist_tracks_forbidden(error: &spotuify_spotify::SpotifyError) -> bool {
    matches!(
        error,
        spotuify_spotify::SpotifyError::Api {
            status: 403,
            endpoint,
            ..
        } if endpoint.starts_with("GET /playlists/") && endpoint.contains("/items")
    )
}

async fn sync_recent<C: SyncContext>(ctx: &C, summary: &mut CacheSyncSummary) -> Result<()> {
    fail_if_rate_limited_domain(ctx, "recent").await?;
    let started_at_ms = now_ms();
    let mut client = ctx.spotify_client().await?;
    match client.recently_played().await {
        Ok(items) => {
            summary.recent_items += ctx.store().persist_recent_items_bulk(&items).await?;
            summary.media_items += items.len() as u32;
            if let Err(err) = ctx.index_media_items(&items, false).await {
                tracing::debug!(error = %err, "recent item index update failed");
            }
            ctx.store()
                .record_sync_event_bulk("recent", started_at_ms, "ok", items.len() as u32, None)
                .await?;
            Ok(())
        }
        Err(err) => {
            record_sync_error(ctx, "recent", started_at_ms, &err).await?;
            Err(err.into())
        }
    }
}

async fn sync_library<C: SyncContext>(ctx: &C, summary: &mut CacheSyncSummary) -> Result<()> {
    fail_if_rate_limited_domain(ctx, "library").await?;
    let started_at_ms = now_ms();
    let mut client = ctx.spotify_client().await?;
    let mut items = Vec::new();
    match client.saved_tracks_page(50, 0).await {
        Ok(page) => {
            let (local_total, local_first_ids) = ctx.store().saved_tracks_fingerprint(50).await?;
            let remote_ids = page
                .items
                .iter()
                .map(|item| item.id.as_deref().unwrap_or(item.uri.as_str()))
                .collect::<Vec<_>>();
            let local_ids = local_first_ids
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            if should_refetch_saved_tracks(local_total, &local_ids, page.total, &remote_ids) {
                match client.saved_tracks().await {
                    Ok(saved_tracks) => items.extend(saved_tracks),
                    Err(err) => tracing::warn!(error = %err, "saved tracks sync failed"),
                }
            } else {
                tracing::debug!(
                    total = page.total,
                    "saved tracks unchanged; skipping full refetch"
                );
            }
        }
        Err(err) => tracing::warn!(error = %err, "saved tracks sync failed"),
    }
    match client.saved_albums().await {
        Ok(saved_albums) => items.extend(saved_albums),
        Err(err) => tracing::warn!(error = %err, "saved albums sync failed"),
    }
    // Subscribed podcasts — these are MediaKind::Show items so the
    // TUI can split Library into Music vs Podcasts via kind filter.
    match client.saved_shows().await {
        Ok(shows) => items.extend(shows),
        Err(err) => tracing::warn!(error = %err, "saved shows sync failed"),
    }
    // Followed artists are persisted separately: they are `followed`, not
    // `saved`, so they must not ride the saved=1 bulk path above. They power
    // the discography browser's entry point (Library → Artists → Enter).
    match client.followed_artists().await {
        Ok(artists) => {
            summary.library_items += ctx.store().persist_followed_artists(&artists).await?;
            summary.media_items += artists.len() as u32;
            if let Err(err) = ctx.index_media_items(&artists, true).await {
                tracing::debug!(error = %err, "followed artists index update failed");
            }
        }
        Err(err) => tracing::warn!(error = %err, "followed artists sync failed"),
    }
    summary.library_items += ctx.store().persist_library_items_bulk(&items).await?;
    summary.media_items += items.len() as u32;
    if let Err(err) = ctx.index_media_items(&items, true).await {
        tracing::debug!(error = %err, "library item index update failed");
    }
    ctx.store()
        .record_sync_event_bulk("library", started_at_ms, "ok", items.len() as u32, None)
        .await?;
    Ok(())
}

fn playlist_as_media_item(playlist: &Playlist) -> MediaItem {
    let uri = if playlist.id.starts_with("spotify:playlist:") {
        playlist.id.clone()
    } else {
        format!("spotify:playlist:{}", playlist.id)
    };
    MediaItem {
        id: Some(playlist.id.clone()),
        uri,
        name: playlist.name.clone(),
        subtitle: playlist.owner.clone(),
        context: format!("{} tracks", playlist.tracks_total),
        duration_ms: 0,
        image_url: playlist.image_url.clone(),
        kind: MediaKind::Playlist,
        source: Some("spotify".to_string()),
        freshness: Some("fresh".to_string()),
        explicit: None,
        is_playable: None,
        ..Default::default()
    }
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

fn playback_has_live_signal(playback: &Playback) -> bool {
    playback.item.is_some() || playback.device.is_some() || playback.is_playing
}

/// `true` when the queue's currently-playing URI or upcoming-item
/// URIs differ between the pre- and post-poll snapshot. Same idea as
/// `playback_diff_is_meaningful` — the daemon polls every 3s for
/// freshness but the queue rarely actually changes; subscribers don't
/// need a re-render when it didn't.
fn queue_diff_is_meaningful(
    before: &spotuify_spotify::client::Queue,
    after: &spotuify_spotify::client::Queue,
) -> bool {
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

async fn fail_if_rate_limited_domain<C: SyncContext>(ctx: &C, domain: &str) -> Result<()> {
    if let Some(remaining_ms) = ctx.store().rate_limit_cooldown_remaining_ms(domain).await? {
        tracing::debug!(
            domain,
            remaining_ms,
            "skipping sync while Spotify rate limit cooldown is active"
        );
        return Err(SpotifyError::RateLimited {
            retry_after: Duration::from_millis(remaining_ms as u64),
            scope: domain.to_string(),
        }
        .into());
    }
    Ok(())
}

async fn record_sync_error<C: SyncContext>(
    ctx: &C,
    domain: &str,
    started_at_ms: i64,
    err: &SpotifyError,
) -> Result<()> {
    ctx.store()
        .record_sync_event_bulk_with_retry_after(
            domain,
            started_at_ms,
            "error",
            0,
            Some(&err.to_string()),
            retry_after_secs(err),
        )
        .await
}

fn retry_after_secs(err: &SpotifyError) -> Option<u64> {
    let SpotifyError::RateLimited { retry_after, .. } = err else {
        return None;
    };
    let millis = retry_after.as_millis();
    Some(millis.div_ceil(1000).max(1).min(u128::from(u64::MAX)) as u64)
}

#[cfg(test)]
mod tests {
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
    fn slow_target_timeout_is_generous_for_paginated_syncs() {
        assert!(SLOW_TARGET_TIMEOUT > PER_TARGET_TIMEOUT);
        assert_eq!(SLOW_TARGET_TIMEOUT, Duration::from_secs(30 * 60));
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
    fn rate_limit_retry_after_extracts_429_only() {
        let rate_limited: Option<Result<CacheSyncSummary>> =
            Some(Err(anyhow::Error::new(SpotifyError::RateLimited {
                retry_after: Duration::from_secs(30),
                scope: "GET /me/player".to_string(),
            })));
        assert_eq!(
            rate_limit_retry_after(&rate_limited),
            Some(Duration::from_secs(30))
        );

        let other: Option<Result<CacheSyncSummary>> = Some(Err(anyhow::anyhow!("network blip")));
        assert_eq!(rate_limit_retry_after(&other), None);
        assert_eq!(rate_limit_retry_after(&None), None);
    }

    #[test]
    fn target_backoff_honors_spotify_retry_after() {
        let now = tokio::time::Instant::now();
        let mut backoff = TargetBackoff::default();
        let retry_after = Duration::from_secs(60 * 60);
        let err = anyhow::Error::new(SpotifyError::RateLimited {
            retry_after,
            scope: "GET /me/player".to_string(),
        });

        backoff.record_failure(now, &err);

        assert!(!backoff.should_run(now + BACKOFF_MAX + Duration::from_secs(1)));
        assert!(backoff.should_run(now + retry_after));
    }
}
