use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use crate::daemon::state::DaemonState;
use crate::protocol::{CacheSyncSummary, DaemonEvent, SyncTargetData};
use crate::store::now_ms;

pub(crate) fn spawn_background_scheduler(state: Arc<DaemonState>) {
    tokio::spawn(async move {
        let mut shutdown_rx = state.shutdown_receiver();
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(err) = sync_target(state.clone(), SyncTargetData::Playback).await {
                        tracing::debug!(error = %err, "background playback sync failed");
                    }
                    if let Err(err) = sync_target(state.clone(), SyncTargetData::Devices).await {
                        tracing::debug!(error = %err, "background device sync failed");
                    }
                    if let Err(err) = sync_target(state.clone(), SyncTargetData::Recent).await {
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

pub(crate) async fn sync_target(
    state: Arc<DaemonState>,
    target: SyncTargetData,
) -> Result<CacheSyncSummary> {
    state.emit_event(DaemonEvent::SyncStarted { target });
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
            sync_playback(&state, &mut summary).await?;
            sync_devices(&state, &mut summary).await?;
            sync_playlists(&state, &mut summary).await?;
            sync_recent(&state, &mut summary).await?;
            sync_library(&state, &mut summary).await?;
        }
        SyncTargetData::Playback => sync_playback(&state, &mut summary).await?,
        SyncTargetData::Devices => sync_devices(&state, &mut summary).await?,
        SyncTargetData::Playlists => sync_playlists(&state, &mut summary).await?,
        SyncTargetData::Recent => sync_recent(&state, &mut summary).await?,
        SyncTargetData::Library => sync_library(&state, &mut summary).await?,
    }

    state.emit_event(DaemonEvent::SyncFinished {
        summary: summary.clone(),
    });
    Ok(summary)
}

async fn sync_playback(state: &Arc<DaemonState>, summary: &mut CacheSyncSummary) -> Result<()> {
    let started_at_ms = now_ms();
    let mut client = state.spotify_client().await?;
    match client.playback().await {
        Ok(playback) => {
            summary.playback_snapshots += state.store().persist_playback(&playback).await?;
            if playback.item.is_some() {
                summary.media_items += 1;
            }
            state
                .store()
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
            state
                .store()
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

async fn sync_devices(state: &Arc<DaemonState>, summary: &mut CacheSyncSummary) -> Result<()> {
    let started_at_ms = now_ms();
    let mut client = state.spotify_client().await?;
    match client.devices().await {
        Ok(devices) => {
            summary.devices += state.store().persist_devices(&devices).await?;
            state
                .store()
                .record_sync_event("devices", started_at_ms, "ok", devices.len() as u32, None)
                .await?;
            Ok(())
        }
        Err(err) => {
            state
                .store()
                .record_sync_event("devices", started_at_ms, "error", 0, Some(&err.to_string()))
                .await?;
            Err(err)
        }
    }
}

async fn sync_playlists(state: &Arc<DaemonState>, summary: &mut CacheSyncSummary) -> Result<()> {
    if skip_rate_limited_domain(state, "playlists").await? {
        return Ok(());
    }
    let started_at_ms = now_ms();
    let mut client = state.spotify_client().await?;
    match client.playlists().await {
        Ok(playlists) => {
            summary.playlists += state.store().persist_playlists(&playlists).await?;
            summary.media_items += playlists.len() as u32;
            // Phase 6.5: snapshot_id refetch gate. Compare each remote
            // playlist's snapshot_id against our local cached value;
            // skip the expensive paginated playlist_tracks call when
            // they match.
            for playlist in &playlists {
                let local_snapshot =
                    state.store().playlist_snapshot_id(&playlist.id).await.ok().flatten();
                let needs_refetch = spotuify_sync::should_refetch_playlist_tracks(
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
                        summary.playlist_items += state
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
            state
                .store()
                .record_sync_event("playlists", started_at_ms, "ok", summary.playlists, None)
                .await?;
            Ok(())
        }
        Err(err) => {
            state
                .store()
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

async fn sync_recent(state: &Arc<DaemonState>, summary: &mut CacheSyncSummary) -> Result<()> {
    if skip_rate_limited_domain(state, "recent").await? {
        return Ok(());
    }
    let started_at_ms = now_ms();
    let mut client = state.spotify_client().await?;
    match client.recently_played().await {
        Ok(items) => {
            summary.recent_items += state.store().persist_recent_items(&items).await?;
            summary.media_items += items.len() as u32;
            state
                .store()
                .record_sync_event("recent", started_at_ms, "ok", items.len() as u32, None)
                .await?;
            Ok(())
        }
        Err(err) => {
            state
                .store()
                .record_sync_event("recent", started_at_ms, "error", 0, Some(&err.to_string()))
                .await?;
            Err(err)
        }
    }
}

async fn sync_library(state: &Arc<DaemonState>, summary: &mut CacheSyncSummary) -> Result<()> {
    if skip_rate_limited_domain(state, "library").await? {
        return Ok(());
    }
    let started_at_ms = now_ms();
    let mut client = state.spotify_client().await?;
    let mut items = Vec::new();
    match client.saved_tracks().await {
        Ok(saved_tracks) => items.extend(saved_tracks),
        Err(err) => tracing::warn!(error = %err, "saved tracks sync failed"),
    }
    match client.saved_albums().await {
        Ok(saved_albums) => items.extend(saved_albums),
        Err(err) => tracing::warn!(error = %err, "saved albums sync failed"),
    }
    summary.library_items += state.store().persist_library_items(&items).await?;
    summary.media_items += items.len() as u32;
    state
        .store()
        .record_sync_event("library", started_at_ms, "ok", items.len() as u32, None)
        .await?;
    Ok(())
}

async fn skip_rate_limited_domain(state: &DaemonState, domain: &str) -> Result<bool> {
    if let Some(remaining_ms) = state
        .store()
        .rate_limit_cooldown_remaining_ms(domain)
        .await?
    {
        tracing::debug!(
            domain,
            remaining_ms,
            "skipping sync while Spotify rate limit cooldown is active"
        );
        return Ok(true);
    }
    Ok(false)
}
