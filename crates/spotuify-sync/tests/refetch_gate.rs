#![allow(clippy::panic, clippy::unwrap_used)]

//! Phase 6.5 — sync refetch gate decision tests.

use spotuify_core::{
    AccessOutcome, AccessUnavailable, CollectionRequest, MediaItem, MediaKind, MusicProvider,
    PageRequest, PlayRequest, PlaySource, Playlist, ProviderCaps, ProviderError, ProviderId,
    ProviderPage, ProviderResult, RemoteTransport, RequestContext, ResourceUri, TransportCommand,
    TransportDevice, UriScheme,
};
use spotuify_protocol::{DaemonEvent, SyncTargetData};
use spotuify_provider_fake::{FakeDataset, FakeProvider};
use spotuify_store::Store;
use spotuify_sync::{should_refetch_playlist_tracks, sync_target, SyncContext, SyncProvider};
use tokio::sync::watch;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

// --- Playlist snapshot-id gate ---

#[test]
fn first_sync_with_no_local_snapshot_refetches() {
    assert!(should_refetch_playlist_tracks(None, Some("snap-1")));
}

#[test]
fn matching_snapshots_skip_refetch() {
    assert!(!should_refetch_playlist_tracks(
        Some("snap-1"),
        Some("snap-1")
    ));
}

#[test]
fn differing_snapshots_trigger_refetch() {
    assert!(should_refetch_playlist_tracks(
        Some("snap-1"),
        Some("snap-2")
    ));
}

#[test]
fn missing_remote_snapshot_refetches_defensively() {
    // The Spotify response didn't include snapshot_id -- we can't
    // prove unchanged, so refetch.
    assert!(should_refetch_playlist_tracks(Some("snap-1"), None));
}

#[test]
fn both_missing_snapshots_refetches() {
    // Cold start with a playlist that never carries snapshot_id.
    assert!(should_refetch_playlist_tracks(None, None));
}

#[test]
fn empty_string_snapshot_is_distinct_from_missing() {
    // Implementation detail: empty string is a valid (if degenerate)
    // snapshot id; it shouldn't be treated as None.
    assert!(!should_refetch_playlist_tracks(Some(""), Some("")));
    assert!(should_refetch_playlist_tracks(Some(""), Some("real-snap")));
}

struct FakeCtx {
    store: Store,
    shutdown_rx: watch::Receiver<bool>,
    emitted: std::sync::Mutex<Vec<DaemonEvent>>,
    provider: std::sync::Arc<dyn MusicProvider>,
    transport: Option<std::sync::Arc<dyn RemoteTransport>>,
    /// Stand-in for the daemon's `playback_clock`: tracks the most
    /// recent state passed through `apply_playback_poll` so the
    /// sync-loop's diff-then-broadcast gate sees a real before→after
    /// change and exercises the emit path in tests.
    fake_clock: std::sync::Mutex<spotuify_core::Playback>,
}

#[async_trait::async_trait]
impl SyncContext for FakeCtx {
    fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown_rx.clone()
    }

    fn store(&self) -> &Store {
        &self.store
    }

    fn emit_event(&self, event: DaemonEvent) {
        self.emitted.lock().unwrap().push(event);
    }

    async fn sync_providers(&self) -> anyhow::Result<Vec<SyncProvider>> {
        Ok(vec![SyncProvider::new(
            self.provider.clone(),
            self.transport.clone(),
        )?])
    }

    /// Pretend the clock always accepts the poll so the sync loop's
    /// emit gate exercises the broadcast path in tests. The real
    /// daemon clock applies source-priority + URI tie-break logic; we
    /// trust those unit-tested in `spotuify-daemon::clock`.
    fn apply_playback_poll(
        &self,
        _provider: &spotuify_core::ProviderId,
        playback: &spotuify_core::Playback,
        _captured_seq: u64,
        _state_seq: u64,
        _sampled_at_ms: i64,
        _provider_timestamp_ms: Option<i64>,
    ) -> bool {
        *self.fake_clock.lock().unwrap() = playback.clone();
        true
    }

    fn snapshot_playback(&self) -> spotuify_core::Playback {
        self.fake_clock.lock().unwrap().clone()
    }
}

async fn fake_ctx() -> FakeCtx {
    let provider = std::sync::Arc::new(spotify_shaped_fake());
    provider
        .execute(
            RequestContext::FOREGROUND,
            TransportCommand::Play(PlayRequest {
                start_uri: ResourceUri::parse("spotify:track:track-1").unwrap(),
                source: PlaySource::Ordered(vec![
                    ResourceUri::parse("spotify:track:track-1").unwrap(),
                    ResourceUri::parse("spotify:track:track-2").unwrap(),
                ]),
                device: TransportDevice::Active,
                position_ms: 0,
            }),
        )
        .await
        .expect("seed fake playback");
    let music: std::sync::Arc<dyn MusicProvider> = provider.clone();
    let transport: std::sync::Arc<dyn RemoteTransport> = provider;
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    FakeCtx {
        store: Store::in_memory().await.expect("in-memory store"),
        shutdown_rx,
        emitted: std::sync::Mutex::new(Vec::new()),
        provider: music,
        transport: Some(transport),
        fake_clock: std::sync::Mutex::new(spotuify_core::Playback::default()),
    }
}

fn spotify_shaped_fake() -> FakeProvider {
    // Store playlist helpers remain Spotify-shaped until the provider-scoped
    // persistence phase lands. The behavior is still entirely supplied by
    // spotuify-provider-fake; only its deterministic fixture namespace is
    // configured to match the current store contract.
    FakeProvider::with_identity(
        ProviderId::new("spotify").unwrap(),
        UriScheme::Spotify,
        FakeDataset::Standard,
    )
}

struct PlaylistTrackCtx {
    store: Store,
    shutdown_rx: watch::Receiver<bool>,
    provider: std::sync::Arc<ScriptedPlaylistProvider>,
}

struct ScriptedPlaylistProvider {
    inner: FakeProvider,
    responses: std::sync::Mutex<VecDeque<ProviderResult<AccessOutcome<Vec<MediaItem>>>>>,
    version_changes: std::sync::Mutex<VecDeque<bool>>,
    fetch_count: AtomicUsize,
    item_read: bool,
}

#[async_trait::async_trait]
impl MusicProvider for ScriptedPlaylistProvider {
    fn id(&self) -> &ProviderId {
        MusicProvider::id(&self.inner)
    }

    fn uri_scheme(&self) -> &UriScheme {
        MusicProvider::uri_scheme(&self.inner)
    }

    fn display_name(&self) -> &str {
        "Scripted Playlist Provider"
    }

    fn capabilities(&self) -> ProviderCaps {
        let mut caps = self.inner.capabilities();
        caps.transport = None;
        caps.playlists.item_read = self.item_read;
        caps
    }

    fn playlist_version_changed(&self, previous: Option<&str>, current: Option<&str>) -> bool {
        self.version_changes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(previous != current)
    }

    async fn playlists(
        &self,
        context: RequestContext,
        page: PageRequest,
    ) -> ProviderResult<ProviderPage<Playlist>> {
        self.inner.playlists(context, page).await
    }

    async fn playlist_items(
        &self,
        _context: RequestContext,
        request: CollectionRequest,
    ) -> ProviderResult<AccessOutcome<ProviderPage<MediaItem>>> {
        self.fetch_count.fetch_add(1, Ordering::SeqCst);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("playlist-track response fixture exhausted")
            .map(|outcome| match outcome {
                AccessOutcome::Available(items) => AccessOutcome::Available(ProviderPage {
                    total: Some(items.len() as u64),
                    items,
                    requested_offset: request.page.offset,
                    next: None,
                }),
                AccessOutcome::Unavailable(reason) => AccessOutcome::Unavailable(reason),
            })
    }
}

#[async_trait::async_trait]
impl SyncContext for PlaylistTrackCtx {
    fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown_rx.clone()
    }

    fn store(&self) -> &Store {
        &self.store
    }

    fn emit_event(&self, _event: DaemonEvent) {}

    async fn sync_providers(&self) -> anyhow::Result<Vec<SyncProvider>> {
        let provider: std::sync::Arc<dyn MusicProvider> = self.provider.clone();
        Ok(vec![SyncProvider::new(provider, None)?])
    }
}

async fn playlist_track_ctx(responses: Vec<ProviderResult<Vec<MediaItem>>>) -> PlaylistTrackCtx {
    playlist_track_ctx_with_item_read(responses, true).await
}

async fn playlist_track_ctx_with_item_read(
    responses: Vec<ProviderResult<Vec<MediaItem>>>,
    item_read: bool,
) -> PlaylistTrackCtx {
    let outcomes = responses
        .into_iter()
        .map(|result| result.map(AccessOutcome::Available))
        .collect();
    playlist_track_ctx_from_outcomes(outcomes, item_read).await
}

async fn playlist_track_ctx_from_outcomes(
    responses: Vec<ProviderResult<AccessOutcome<Vec<MediaItem>>>>,
    item_read: bool,
) -> PlaylistTrackCtx {
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let provider = std::sync::Arc::new(ScriptedPlaylistProvider {
        inner: spotify_shaped_fake(),
        responses: std::sync::Mutex::new(responses.into()),
        version_changes: std::sync::Mutex::new(VecDeque::new()),
        fetch_count: AtomicUsize::new(0),
        item_read,
    });
    PlaylistTrackCtx {
        store: Store::in_memory().await.expect("in-memory store"),
        shutdown_rx,
        provider,
    }
}

#[tokio::test]
async fn playlist_sync_skips_items_when_provider_does_not_advertise_item_read() {
    let ctx = playlist_track_ctx_with_item_read(Vec::new(), false).await;

    let summary = sync_target(&ctx, SyncTargetData::Playlists)
        .await
        .expect("metadata-only playlist sync");

    assert_eq!(summary.playlists, 1);
    assert_eq!(summary.playlist_items, 0);
    assert_eq!(ctx.provider.fetch_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn queue_sync_persists_current_and_upcoming_items() {
    let ctx = fake_ctx().await;

    let summary = sync_target(&ctx, SyncTargetData::Queue)
        .await
        .expect("queue sync");
    let queue = ctx
        .store
        .latest_queue(10)
        .await
        .expect("queue cache read")
        .expect("queue cache should exist");

    assert_eq!(summary.queue_snapshots, 1);
    assert_eq!(summary.queue_items, 1);
    assert!(queue.currently_playing.is_some());
    assert_eq!(queue.items.len(), 1);
}

#[tokio::test]
async fn playlist_sync_fetches_tracks_on_cold_start_then_skips_when_snapshot_matches() {
    let ctx = fake_ctx().await;

    let first = sync_target(&ctx, SyncTargetData::Playlists)
        .await
        .expect("first playlist sync");
    assert_eq!(first.playlists, 1);
    assert_eq!(
        first.playlist_items, 2,
        "cold start must fetch playlist tracks before persisting remote snapshot"
    );

    let second = sync_target(&ctx, SyncTargetData::Playlists)
        .await
        .expect("second playlist sync");
    assert_eq!(second.playlists, 1);
    assert_eq!(
        second.playlist_items, 0,
        "matching snapshot should skip expensive tracks refetch"
    );
}

#[tokio::test]
async fn transient_playlist_track_failure_is_reported_then_next_run_recovers() {
    let new_item = MediaItem {
        uri: "spotify:track:new".to_string(),
        name: "New".to_string(),
        kind: MediaKind::Track,
        ..Default::default()
    };
    let ctx = playlist_track_ctx(vec![
        Err(ProviderError::Network("transient".to_string())),
        Ok(vec![new_item.clone()]),
    ])
    .await;
    let playlist = Playlist {
        id: "spotify:playlist:playlist-1".to_string(),
        name: "Transient".to_string(),
        owner: "owner".to_string(),
        tracks_total: 1,
        image_url: None,
        version_token: Some("version-old".to_string()),
    };
    let old_item = MediaItem {
        uri: "spotify:track:old".to_string(),
        name: "Old".to_string(),
        kind: MediaKind::Track,
        ..Default::default()
    };
    ctx.store
        .persist_provider_playlists("spotify", std::slice::from_ref(&playlist))
        .await
        .expect("initial metadata");
    ctx.store
        .persist_provider_playlist_items_with_version_bulk(
            &ProviderId::new("spotify").expect("valid provider id"),
            &playlist.id,
            std::slice::from_ref(&old_item),
            playlist.version_token.as_deref(),
        )
        .await
        .expect("initial items and version");

    let err = sync_target(&ctx, SyncTargetData::Playlists)
        .await
        .expect_err("partial playlist fetch must fail the provider pass");
    assert!(err.to_string().contains("transient"), "{err}");
    assert_eq!(ctx.provider.fetch_count.load(Ordering::SeqCst), 1);
    let after_failure = ctx
        .store
        .playlist_version_token(&playlist.id)
        .await
        .expect("version read");
    assert_eq!(after_failure.as_deref(), Some("version-old"));
    assert_eq!(
        ctx.store
            .playlist_items_for_provider(
                &playlist.id,
                10,
                Some(&ProviderId::new("spotify").expect("valid provider id")),
            )
            .await
            .expect("old items remain")[0]
            .uri,
        old_item.uri
    );
    let status: String = sqlx::query_scalar(
        "SELECT status FROM sync_events
         WHERE provider = 'spotify' AND domain = 'playlists'
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(ctx.store.reader())
    .await
    .unwrap();
    assert_eq!(status, "error");

    sync_target(&ctx, SyncTargetData::Playlists)
        .await
        .expect("second sync recovers");
    assert_eq!(ctx.provider.fetch_count.load(Ordering::SeqCst), 2);
    let recovered = ctx
        .store
        .playlist_version_token(&playlist.id)
        .await
        .expect("recovered version read");
    assert_eq!(recovered.as_deref(), Some("v1"));
    assert_eq!(
        ctx.store
            .playlist_items_for_provider(
                &playlist.id,
                10,
                Some(&ProviderId::new("spotify").expect("valid provider id")),
            )
            .await
            .expect("new items visible")[0]
            .uri,
        new_item.uri
    );
}

#[tokio::test]
async fn expired_playlist_token_retry_forces_item_refetch() {
    let new_item = MediaItem {
        uri: "spotify:track:new-after-expiry".to_string(),
        name: "New after expiry".to_string(),
        kind: MediaKind::Track,
        ..Default::default()
    };
    let ctx = playlist_track_ctx(vec![
        Err(ProviderError::SyncTokenExpired {
            reason: "scripted playlist cursor expired".to_string(),
        }),
        Ok(vec![new_item.clone()]),
    ])
    .await;
    ctx.provider
        .version_changes
        .lock()
        .unwrap()
        .extend([true, false]);
    let playlist = Playlist {
        id: "spotify:playlist:playlist-1".to_string(),
        name: "Expiry".to_string(),
        owner: "owner".to_string(),
        tracks_total: 1,
        image_url: None,
        version_token: Some("v1".to_string()),
    };
    let old_item = MediaItem {
        uri: "spotify:track:old-before-expiry".to_string(),
        name: "Old before expiry".to_string(),
        kind: MediaKind::Track,
        ..Default::default()
    };
    ctx.store
        .persist_provider_playlists("spotify", std::slice::from_ref(&playlist))
        .await
        .unwrap();
    ctx.store
        .persist_provider_playlist_items_with_version_bulk(
            &ProviderId::new("spotify").expect("valid provider id"),
            &playlist.id,
            std::slice::from_ref(&old_item),
            playlist.version_token.as_deref(),
        )
        .await
        .unwrap();

    let summary = sync_target(&ctx, SyncTargetData::Playlists)
        .await
        .expect("expired token must recover with one forced full retry");

    assert_eq!(ctx.provider.fetch_count.load(Ordering::SeqCst), 2);
    assert_eq!(summary.playlist_items, 1);
    assert_eq!(
        ctx.store
            .playlist_items_for_provider(
                &playlist.id,
                10,
                Some(&ProviderId::new("spotify").expect("valid provider id")),
            )
            .await
            .unwrap()
            .into_iter()
            .map(|item| item.uri)
            .collect::<Vec<_>>(),
        vec![new_item.uri]
    );
}

#[tokio::test]
async fn forbidden_changed_version_retries_once_then_same_version_stays_inaccessible() {
    let ctx = playlist_track_ctx(vec![Err(ProviderError::Forbidden {
        operation: "playlist_items".to_string(),
    })])
    .await;
    let playlist = Playlist {
        id: "spotify:playlist:playlist-1".to_string(),
        name: "Forbidden".to_string(),
        owner: "owner".to_string(),
        tracks_total: 1,
        image_url: None,
        version_token: Some("version-old".to_string()),
    };
    ctx.store
        .persist_provider_playlists("spotify", std::slice::from_ref(&playlist))
        .await
        .unwrap();
    ctx.store
        .persist_provider_playlist_items_with_version_bulk(
            &ProviderId::new("spotify").expect("valid provider id"),
            &playlist.id,
            &[],
            playlist.version_token.as_deref(),
        )
        .await
        .unwrap();
    ctx.store
        .mark_playlist_tracks_inaccessible_at_version(
            &playlist.id,
            playlist.version_token.as_deref(),
        )
        .await
        .unwrap();

    // The fake provider now reports `v1`, so the changed token must
    // retry even though the old version was terminally inaccessible.
    sync_target(&ctx, SyncTargetData::Playlists).await.unwrap();
    assert_eq!(ctx.provider.fetch_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        ctx.store
            .playlist_version_token(&playlist.id)
            .await
            .unwrap()
            .as_deref(),
        Some("v1")
    );
    assert!(!ctx
        .store
        .playlist_tracks_accessible(&playlist.id)
        .await
        .unwrap());

    // The second run sees the same terminal token and must not fetch again.
    sync_target(&ctx, SyncTargetData::Playlists).await.unwrap();
    assert_eq!(ctx.provider.fetch_count.load(Ordering::SeqCst), 1);
    assert!(!ctx
        .store
        .playlist_tracks_accessible(&playlist.id)
        .await
        .unwrap());
}

#[tokio::test]
async fn temporarily_unavailable_playlist_tracks_are_not_latched_and_refetch_next_cycle() {
    // A transient `TemporarilyUnavailable` outcome must not latch the playlist
    // inaccessible or advance its version, or the skip gate would never retry it.
    let new_item = MediaItem {
        uri: "spotify:track:recovered".to_string(),
        name: "Recovered".to_string(),
        kind: MediaKind::Track,
        ..Default::default()
    };
    let ctx = playlist_track_ctx_from_outcomes(
        vec![
            Ok(AccessOutcome::Unavailable(
                AccessUnavailable::TemporarilyUnavailable,
            )),
            Ok(AccessOutcome::Available(vec![new_item.clone()])),
        ],
        true,
    )
    .await;
    let playlist = Playlist {
        id: "spotify:playlist:playlist-1".to_string(),
        name: "Transient outcome".to_string(),
        owner: "owner".to_string(),
        tracks_total: 1,
        image_url: None,
        version_token: Some("version-old".to_string()),
    };
    ctx.store
        .persist_provider_playlists("spotify", std::slice::from_ref(&playlist))
        .await
        .unwrap();
    ctx.store
        .persist_provider_playlist_items_with_version_bulk(
            &ProviderId::new("spotify").expect("valid provider id"),
            &playlist.id,
            &[],
            playlist.version_token.as_deref(),
        )
        .await
        .unwrap();

    // First cycle: the transient outcome must leave the playlist eligible.
    sync_target(&ctx, SyncTargetData::Playlists).await.unwrap();
    assert_eq!(ctx.provider.fetch_count.load(Ordering::SeqCst), 1);
    assert!(
        ctx.store
            .playlist_tracks_accessible(&playlist.id)
            .await
            .unwrap(),
        "transient failure must not latch the playlist inaccessible"
    );
    assert_eq!(
        ctx.store
            .playlist_version_token(&playlist.id)
            .await
            .unwrap()
            .as_deref(),
        Some("version-old"),
        "transient failure must not advance the stored version"
    );

    // Second cycle: still eligible, so it refetches and recovers.
    sync_target(&ctx, SyncTargetData::Playlists).await.unwrap();
    assert_eq!(ctx.provider.fetch_count.load(Ordering::SeqCst), 2);
    assert_eq!(
        ctx.store
            .playlist_items_for_provider(
                &playlist.id,
                10,
                Some(&ProviderId::new("spotify").expect("valid provider id")),
            )
            .await
            .unwrap()[0]
            .uri,
        new_item.uri
    );
}

#[tokio::test]
async fn terminal_unavailable_playlist_tracks_stay_latched() {
    // A terminal outcome (e.g. subscription required) latches the playlist
    // inaccessible so the skip gate stops retrying the same version.
    let ctx = playlist_track_ctx_from_outcomes(
        vec![Ok(AccessOutcome::Unavailable(
            AccessUnavailable::SubscriptionRequired,
        ))],
        true,
    )
    .await;
    let playlist = Playlist {
        id: "spotify:playlist:playlist-1".to_string(),
        name: "Terminal outcome".to_string(),
        owner: "owner".to_string(),
        tracks_total: 1,
        image_url: None,
        version_token: Some("version-old".to_string()),
    };
    ctx.store
        .persist_provider_playlists("spotify", std::slice::from_ref(&playlist))
        .await
        .unwrap();
    ctx.store
        .persist_provider_playlist_items_with_version_bulk(
            &ProviderId::new("spotify").expect("valid provider id"),
            &playlist.id,
            &[],
            playlist.version_token.as_deref(),
        )
        .await
        .unwrap();

    sync_target(&ctx, SyncTargetData::Playlists).await.unwrap();
    assert_eq!(ctx.provider.fetch_count.load(Ordering::SeqCst), 1);
    assert!(
        !ctx.store
            .playlist_tracks_accessible(&playlist.id)
            .await
            .unwrap(),
        "terminal failure must latch the playlist inaccessible"
    );

    // Same version stays terminal: the skip gate must not refetch.
    sync_target(&ctx, SyncTargetData::Playlists).await.unwrap();
    assert_eq!(ctx.provider.fetch_count.load(Ordering::SeqCst), 1);
    assert!(!ctx
        .store
        .playlist_tracks_accessible(&playlist.id)
        .await
        .unwrap());
}

#[tokio::test]
async fn library_sync_skips_unchanged_kinds_after_cold_sync() {
    let ctx = fake_ctx().await;

    let first = sync_target(&ctx, SyncTargetData::Library)
        .await
        .expect("first library sync");
    // Sync only enumerates the kinds advertised by provider capabilities.
    assert_eq!(
        first.library_items, 2,
        "cold library sync persists the fake provider's saved track and followed artist"
    );

    let second = sync_target(&ctx, SyncTargetData::Library)
        .await
        .expect("second library sync");
    assert_eq!(
        second.library_items, 0,
        "matching per-kind probes should skip the full library refetch"
    );
}

/// Regression test for the "TUI shows blank player at launch" bug:
/// the 60s background sync poll was persisting to SQLite but never
/// emitting `PlaybackChanged`, so subscribed clients never re-rendered
/// when playback changed on another Spotify device. After this fix
/// `sync_playback` must broadcast on every successful poll.
#[tokio::test]
async fn playback_sync_broadcasts_playback_changed_for_subscribed_clients() {
    let ctx = fake_ctx().await;

    sync_target(&ctx, SyncTargetData::Playback)
        .await
        .expect("playback sync");

    let events = ctx.emitted.lock().unwrap().clone();
    assert!(
        events.iter().any(|e| matches!(
            e,
            DaemonEvent::PlaybackChanged { action, .. } if action == "synced"
        )),
        "sync_playback must emit a `synced` PlaybackChanged so TUI/MCP \
         subscribers re-render after the background poll (otherwise \
         cross-device playback changes never reach the player widget). \
         Saw events: {events:?}"
    );
}

/// Companion regression: queue sync must also broadcast so the queue
/// rail in the TUI reflects items added on another device.
#[tokio::test]
async fn queue_sync_broadcasts_queue_changed_for_subscribed_clients() {
    let ctx = fake_ctx().await;

    sync_target(&ctx, SyncTargetData::Queue)
        .await
        .expect("queue sync");

    let events = ctx.emitted.lock().unwrap().clone();
    assert!(
        events.iter().any(|e| matches!(
            e,
            DaemonEvent::QueueChanged { action, .. } if action == "synced"
        )),
        "sync_queue must emit a `synced` QueueChanged. Saw: {events:?}"
    );
}

/// Companion regression: device sync must broadcast so the device
/// picker reflects newly-available speakers between explicit user
/// refreshes.
#[tokio::test]
async fn devices_sync_broadcasts_devices_changed_for_subscribed_clients() {
    let ctx = fake_ctx().await;

    sync_target(&ctx, SyncTargetData::Devices)
        .await
        .expect("devices sync");

    let events = ctx.emitted.lock().unwrap().clone();
    assert!(
        events.iter().any(|e| matches!(
            e,
            DaemonEvent::DevicesChanged { action, .. } if action == "synced"
        )),
        "sync_devices must emit a `synced` DevicesChanged. Saw: {events:?}"
    );
}

/// Diff-then-broadcast regression: when the daemon polls and the
/// state is byte-identical to the previous poll, no `PlaybackChanged`
/// event should fire. Subscribers were getting ~20 events/min during
/// steady-state listening which (a) churned the TUI's toast row and
/// (b) wasted IPC bandwidth.
#[tokio::test]
async fn playback_sync_skips_emit_when_state_unchanged() {
    let ctx = fake_ctx().await;

    // First sync: fake clock starts empty, poll returns a fake
    // playback — diff is meaningful, emit fires.
    sync_target(&ctx, SyncTargetData::Playback)
        .await
        .expect("first playback sync");
    let synced_first = ctx
        .emitted
        .lock()
        .unwrap()
        .iter()
        .filter(|e| {
            matches!(
                e,
                DaemonEvent::PlaybackChanged { action, .. } if action == "synced"
            )
        })
        .count();
    assert_eq!(
        synced_first, 1,
        "first poll must emit (state changed from empty)"
    );

    // Drain so we only count the second poll's emits.
    ctx.emitted.lock().unwrap().clear();

    // Second sync: poll returns the SAME fake playback, fake_clock
    // already holds it. before == after, diff returns false.
    sync_target(&ctx, SyncTargetData::Playback)
        .await
        .expect("second playback sync");
    let synced_second = ctx
        .emitted
        .lock()
        .unwrap()
        .iter()
        .filter(|e| {
            matches!(
                e,
                DaemonEvent::PlaybackChanged { action, .. } if action == "synced"
            )
        })
        .count();
    assert_eq!(
        synced_second, 0,
        "steady-state poll with no diff must NOT re-emit PlaybackChanged"
    );
}
