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
use spotuify_core::now_ms;
use spotuify_protocol::{CacheSyncSummary, DaemonEvent, SyncTargetData};

use crate::{should_refetch_playlist_tracks, SyncContext};

/// Spawn the 60-second background sync loop. Runs until the daemon's
/// shutdown signal fires.
pub fn spawn_background_scheduler<C>(ctx: Arc<C>)
where
    C: SyncContext + 'static,
{
    tokio::spawn(async move {
        let mut shutdown_rx = ctx.shutdown_receiver();
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(err) = sync_target(ctx.as_ref(), SyncTargetData::Playback).await {
                        tracing::debug!(error = %err, "background playback sync failed");
                    }
                    if let Err(err) = sync_target(ctx.as_ref(), SyncTargetData::Devices).await {
                        tracing::debug!(error = %err, "background device sync failed");
                    }
                    if let Err(err) = sync_target(ctx.as_ref(), SyncTargetData::Recent).await {
                        tracing::debug!(error = %err, "background recent sync failed");
                    }
                }
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow_and_update() {
                        break;
                    }
                }
            }
        }
    });
}

/// Run one sync pass for the given target. Used both by the background
/// scheduler and by the `spotuify sync` CLI command.
pub async fn sync_target<C: SyncContext>(
    ctx: &C,
    target: SyncTargetData,
) -> Result<CacheSyncSummary> {
    ctx.emit_event(DaemonEvent::SyncStarted { target });
    let mut summary = CacheSyncSummary {
        target,
        playback_snapshots: 0,
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
            sync_devices(ctx, &mut summary).await?;
            sync_playlists(ctx, &mut summary).await?;
            sync_recent(ctx, &mut summary).await?;
            sync_library(ctx, &mut summary).await?;
        }
        SyncTargetData::Playback => sync_playback(ctx, &mut summary).await?,
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

async fn sync_playback<C: SyncContext>(ctx: &C, summary: &mut CacheSyncSummary) -> Result<()> {
    let started_at_ms = now_ms();
    let mut client = ctx.spotify_client().await?;
    match client.playback().await {
        Ok(playback) => {
            summary.playback_snapshots += ctx.store().persist_playback(&playback).await?;
            if playback.item.is_some() {
                summary.media_items += 1;
            }
            ctx.store()
                .record_sync_event(
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
                .record_sync_event(
                    "playback",
                    started_at_ms,
                    "error",
                    0,
                    Some(&err.to_string()),
                )
                .await?;
            Err(err)
        }
    }
}

async fn sync_devices<C: SyncContext>(ctx: &C, summary: &mut CacheSyncSummary) -> Result<()> {
    let started_at_ms = now_ms();
    let mut client = ctx.spotify_client().await?;
    match client.devices().await {
        Ok(devices) => {
            summary.devices += ctx.store().persist_devices(&devices).await?;
            ctx.store()
                .record_sync_event("devices", started_at_ms, "ok", devices.len() as u32, None)
                .await?;
            Ok(())
        }
        Err(err) => {
            ctx.store()
                .record_sync_event("devices", started_at_ms, "error", 0, Some(&err.to_string()))
                .await?;
            Err(err)
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
            summary.playlists += ctx.store().persist_playlists(&playlists).await?;
            summary.media_items += playlists.len() as u32;
            // Phase 6.5: snapshot_id refetch gate.
            for playlist in &playlists {
                let local_snapshot = ctx
                    .store()
                    .playlist_snapshot_id(&playlist.id)
                    .await
                    .ok()
                    .flatten();
                let needs_refetch = should_refetch_playlist_tracks(
                    local_snapshot.as_deref(),
                    playlist.snapshot_id.as_deref(),
                );
                if !needs_refetch {
                    tracing::debug!(
                        playlist = %playlist.id,
                        snapshot = %playlist.snapshot_id.as_deref().unwrap_or(""),
                        "playlist unchanged; skipping tracks refetch"
                    );
                    continue;
                }
                match client.playlist_tracks(&playlist.id).await {
                    Ok(items) => {
                        summary.playlist_items += ctx
                            .store()
                            .persist_playlist_items(&playlist.id, &items)
                            .await?;
                        summary.media_items += items.len() as u32;
                    }
                    Err(err) => {
                        tracing::warn!(playlist = %playlist.id, error = %err, "playlist item sync failed")
                    }
                }
            }
            ctx.store()
                .record_sync_event("playlists", started_at_ms, "ok", summary.playlists, None)
                .await?;
            Ok(())
        }
        Err(err) => {
            ctx.store()
                .record_sync_event(
                    "playlists",
                    started_at_ms,
                    "error",
                    0,
                    Some(&err.to_string()),
                )
                .await?;
            Err(err)
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
            summary.recent_items += ctx.store().persist_recent_items(&items).await?;
            summary.media_items += items.len() as u32;
            ctx.store()
                .record_sync_event("recent", started_at_ms, "ok", items.len() as u32, None)
                .await?;
            Ok(())
        }
        Err(err) => {
            ctx.store()
                .record_sync_event("recent", started_at_ms, "error", 0, Some(&err.to_string()))
                .await?;
            Err(err)
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
    match client.saved_tracks().await {
        Ok(saved_tracks) => items.extend(saved_tracks),
        Err(err) => tracing::warn!(error = %err, "saved tracks sync failed"),
    }
    match client.saved_albums().await {
        Ok(saved_albums) => items.extend(saved_albums),
        Err(err) => tracing::warn!(error = %err, "saved albums sync failed"),
    }
    summary.library_items += ctx.store().persist_library_items(&items).await?;
    summary.media_items += items.len() as u32;
    ctx.store()
        .record_sync_event("library", started_at_ms, "ok", items.len() as u32, None)
        .await?;
    Ok(())
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
