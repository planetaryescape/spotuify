//! Phase 7 — sync engine. Moved out of the binary's `src/sync.rs`
//! once the `SyncContext` trait broke the cycle with `DaemonState`.
//!
//! All public functions are generic over `&impl SyncContext`. The
//! binary's wrapper supplies an `Arc<DaemonState>` (which impls
//! `SyncContext`) and the sync loop runs against the daemon's live
//! Spotify client, store, and event broadcaster -- no longer
//! compile-coupled to the daemon module.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use spotuify_core::{now_ms, MediaItem, MediaKind, Playback, Playlist};
use spotuify_protocol::{CacheSyncSummary, DaemonEvent, SyncTargetData};
use tokio::task::JoinHandle;

use crate::{should_refetch_playlist_tracks, should_refetch_saved_tracks, SyncContext};

/// Active poll cadence when at least one client is subscribed to
/// daemon events. Matches `spotify_player`'s 3s default — fast enough
/// that cross-device playback changes feel near-live, well under
/// Spotify's rate limits.
const ACTIVE_CADENCE: Duration = Duration::from_secs(3);

/// Idle poll cadence when no client is subscribed. Keeps the cache
/// somewhat fresh for the next launch without burning API quota.
const IDLE_CADENCE: Duration = Duration::from_secs(30);

/// Spawn the background sync loop. Runs until the daemon's shutdown
/// signal fires.
///
/// Sync is the daemon's job, not a client's. Two cadences:
///
/// 1. **Fast cadence (60 s)** — playback / devices / recent. These
///    change as the user listens; the TUI / CLI read them off the
///    in-memory store and they need to feel live.
/// 2. **Slow cadence (15 min)** — playlists / library (saved
///    tracks + albums + subscribed shows). These rarely change and
///    paginating them takes seconds; doing them on the same 60 s tick
///    would hammer Spotify.
///
/// Both cadences run **once immediately** when the daemon starts so
/// the cache is populated by the time any client connects. If the
/// first slow pass fails (auth not ready, rate limit, etc.) the next
/// 15 min tick retries.
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
    let fast_ctx = ctx.clone();
    let fast_future = async move {
        let mut shutdown_rx = fast_ctx.shutdown_receiver();
        // Tick at the active cadence; skip work between ticks when no
        // client is subscribed (idle keeps API spend low). The first
        // tick fires immediately so the cache is warm by the time the
        // user opens the TUI.
        let mut interval = tokio::time::interval(ACTIVE_CADENCE);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // `None` until the first successful sync — keeps the
        // first-tick "elapsed" check from gating us at boot when
        // subscribers aren't yet attached. (Tokio's `Instant` starts
        // at process boot, so we can't construct an Instant in the
        // past to satisfy `elapsed >= IDLE_CADENCE` on first tick.)
        let mut last_sync: Option<tokio::time::Instant> = None;
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let has_subscribers = fast_ctx.event_subscriber_count() > 0;
                    let elapsed = last_sync.map(|t| t.elapsed()).unwrap_or(IDLE_CADENCE);
                    if !has_subscribers && elapsed < IDLE_CADENCE {
                        continue;
                    }
                    // Parallel + per-task timeout. Without this, a
                    // single slow target (e.g. `/me/player/recently-played`
                    // stuck in Spotify rate-limit cooldown) would
                    // block every other sync the loop is supposed to
                    // run on this tick — and prevent Playback emits
                    // for as long as Recent hangs.
                    const PER_TARGET_TIMEOUT: Duration = Duration::from_secs(10);
                    let (pb, q, d, r) = tokio::join!(
                        tokio::time::timeout(
                            PER_TARGET_TIMEOUT,
                            sync_target(fast_ctx.as_ref(), SyncTargetData::Playback),
                        ),
                        tokio::time::timeout(
                            PER_TARGET_TIMEOUT,
                            sync_target(fast_ctx.as_ref(), SyncTargetData::Queue),
                        ),
                        tokio::time::timeout(
                            PER_TARGET_TIMEOUT,
                            sync_target(fast_ctx.as_ref(), SyncTargetData::Devices),
                        ),
                        tokio::time::timeout(
                            PER_TARGET_TIMEOUT,
                            sync_target(fast_ctx.as_ref(), SyncTargetData::Recent),
                        ),
                    );
                    if let Err(err) = pb.unwrap_or_else(|_| Err(anyhow::anyhow!("timed out"))) {
                        tracing::debug!(error = %err, "background playback sync failed");
                    }
                    if let Err(err) = q.unwrap_or_else(|_| Err(anyhow::anyhow!("timed out"))) {
                        tracing::debug!(error = %err, "background queue sync failed");
                    }
                    if let Err(err) = d.unwrap_or_else(|_| Err(anyhow::anyhow!("timed out"))) {
                        tracing::debug!(error = %err, "background device sync failed");
                    }
                    if let Err(err) = r.unwrap_or_else(|_| Err(anyhow::anyhow!("timed out"))) {
                        tracing::debug!(error = %err, "background recent sync failed");
                    }
                    last_sync = Some(tokio::time::Instant::now());
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
        let mut interval = tokio::time::interval(Duration::from_secs(15 * 60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(err) = sync_target(slow_ctx.as_ref(), SyncTargetData::Playlists).await {
                        tracing::warn!(error = %err, "background playlists sync failed");
                    }
                    if let Err(err) = sync_target(slow_ctx.as_ref(), SyncTargetData::Library).await {
                        tracing::warn!(error = %err, "background library sync failed");
                    }
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
                // `snapshot_queue` reads from the store, which sets
                // session_active=false by default (cache reads can't
                // know if the live session still holds). We just got
                // a fresh probe back, so flip it true on the broadcast
                // copy so clients render it as live.
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
            ctx.store()
                .record_sync_event_bulk("queue", started_at_ms, "error", 0, Some(&err.to_string()))
                .await?;
            Err(err.into())
        }
    }
}

async fn sync_playback<C: SyncContext>(ctx: &C, summary: &mut CacheSyncSummary) -> Result<()> {
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
                summary.playback_snapshots += ctx.store().persist_playback_bulk(&playback).await?;
                if let Some(item) = playback.item.as_ref() {
                    summary.media_items += 1;
                    if let Err(err) = ctx
                        .index_media_items(std::slice::from_ref(item), false)
                        .await
                    {
                        tracing::debug!(error = %err, "playback item index update failed");
                    }
                }
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
                let applied = ctx.apply_playback_poll(
                    &playback,
                    pre_seq,
                    state_seq,
                    now_ms(),
                    playback.provider_timestamp_ms,
                );
                if applied {
                    let after = ctx.snapshot_playback();
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
            ctx.store()
                .record_sync_event_bulk(
                    "playback",
                    started_at_ms,
                    "error",
                    0,
                    Some(&err.to_string()),
                )
                .await?;
            Err(err.into())
        }
    }
}

async fn sync_devices<C: SyncContext>(ctx: &C, summary: &mut CacheSyncSummary) -> Result<()> {
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
                summary.devices += ctx.store().persist_devices_bulk(&devices).await?;
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
            ctx.store()
                .record_sync_event_bulk(
                    "devices",
                    started_at_ms,
                    "error",
                    0,
                    Some(&err.to_string()),
                )
                .await?;
            Err(err.into())
        }
    }
}

async fn sync_playlists<C: SyncContext>(ctx: &C, summary: &mut CacheSyncSummary) -> Result<()> {
    if skip_rate_limited_domain(ctx, "playlists").await? {
        return Ok(());
    }
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
            ctx.store()
                .record_sync_event_bulk(
                    "playlists",
                    started_at_ms,
                    "error",
                    0,
                    Some(&err.to_string()),
                )
                .await?;
            Err(err.into())
        }
    }
}

async fn sync_recent<C: SyncContext>(ctx: &C, summary: &mut CacheSyncSummary) -> Result<()> {
    if skip_rate_limited_domain(ctx, "recent").await? {
        return Ok(());
    }
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
            ctx.store()
                .record_sync_event_bulk("recent", started_at_ms, "error", 0, Some(&err.to_string()))
                .await?;
            Err(err.into())
        }
    }
}

async fn sync_library<C: SyncContext>(ctx: &C, summary: &mut CacheSyncSummary) -> Result<()> {
    if skip_rate_limited_domain(ctx, "library").await? {
        return Ok(());
    }
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
    let progress_jump =
        (after.progress_ms as i64 - before.progress_ms as i64).abs() > 5_000;
    progress_jump
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

async fn skip_rate_limited_domain<C: SyncContext>(ctx: &C, domain: &str) -> Result<bool> {
    if let Some(remaining_ms) = ctx.store().rate_limit_cooldown_remaining_ms(domain).await? {
        tracing::debug!(
            domain,
            remaining_ms,
            "skipping sync while Spotify rate limit cooldown is active"
        );
        return Ok(true);
    }
    Ok(false)
}
