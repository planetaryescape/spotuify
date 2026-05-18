//! Phase 6.5 — sync refetch gate decision tests.

use spotuify_protocol::{DaemonEvent, SyncTargetData};
use spotuify_spotify::SpotifyClient;
use spotuify_store::Store;
use spotuify_sync::{
    should_refetch_playlist_tracks, should_refetch_saved_tracks, sync_target, SyncContext,
};
use tokio::sync::watch;

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

// --- Saved-tracks page-0 unchanged shortcut ---

#[test]
fn matching_total_and_first_ids_skips_refetch() {
    let local = ["track:1", "track:2", "track:3"];
    let remote = ["track:1", "track:2", "track:3"];
    assert!(!should_refetch_saved_tracks(100, &local, 100, &remote));
}

#[test]
fn differing_total_triggers_refetch() {
    let local = ["track:1", "track:2"];
    let remote = ["track:1", "track:2"];
    // total changed even though the visible page matches -- maybe a
    // delete at the bottom. Refetch to be safe.
    assert!(should_refetch_saved_tracks(100, &local, 99, &remote));
}

#[test]
fn new_track_at_top_changes_first_ids_and_refetches() {
    let local = ["old-1", "old-2"];
    let remote = ["new-1", "old-1", "old-2"];
    assert!(should_refetch_saved_tracks(100, &local, 101, &remote));
}

#[test]
fn same_total_but_different_first_ids_refetches() {
    // Rare reorder + replace where total stays equal. Refetch.
    let local = ["a", "b", "c"];
    let remote = ["b", "a", "c"];
    assert!(should_refetch_saved_tracks(50, &local, 50, &remote));
}

#[test]
fn empty_library_matches_empty_library() {
    let empty: [&str; 0] = [];
    assert!(!should_refetch_saved_tracks(0, &empty, 0, &empty));
}

#[test]
fn zero_local_versus_nonzero_remote_refetches() {
    let empty: [&str; 0] = [];
    let remote = ["track:1"];
    assert!(should_refetch_saved_tracks(0, &empty, 1, &remote));
}

struct FakeCtx {
    store: Store,
    shutdown_rx: watch::Receiver<bool>,
    emitted: std::sync::Mutex<Vec<DaemonEvent>>,
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

    async fn spotify_client(&self) -> anyhow::Result<SpotifyClient> {
        Ok(SpotifyClient::fake()?)
    }

    /// Pretend the clock always accepts the poll so the sync loop's
    /// emit gate exercises the broadcast path in tests. The real
    /// daemon clock applies source-priority + URI tie-break logic; we
    /// trust those unit-tested in `spotuify-daemon::clock`.
    fn apply_playback_poll(
        &self,
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

#[tokio::test]
async fn queue_sync_persists_current_and_upcoming_items() {
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let ctx = FakeCtx {
        store: Store::in_memory().await.expect("in-memory store"),
        shutdown_rx,
        emitted: std::sync::Mutex::new(Vec::new()),
        fake_clock: std::sync::Mutex::new(spotuify_core::Playback::default()),
    };

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
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let ctx = FakeCtx {
        store: Store::in_memory().await.expect("in-memory store"),
        shutdown_rx,
        emitted: std::sync::Mutex::new(Vec::new()),
        fake_clock: std::sync::Mutex::new(spotuify_core::Playback::default()),
    };

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
async fn library_sync_skips_saved_tracks_when_page_zero_is_unchanged() {
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let ctx = FakeCtx {
        store: Store::in_memory().await.expect("in-memory store"),
        shutdown_rx,
        emitted: std::sync::Mutex::new(Vec::new()),
        fake_clock: std::sync::Mutex::new(spotuify_core::Playback::default()),
    };

    let first = sync_target(&ctx, SyncTargetData::Library)
        .await
        .expect("first library sync");
    assert_eq!(first.library_items, 3);

    let second = sync_target(&ctx, SyncTargetData::Library)
        .await
        .expect("second library sync");
    assert_eq!(
        second.library_items, 1,
        "matching saved-track page 0 should skip the full saved-track refetch and only refresh albums"
    );
}

/// Regression test for the "TUI shows blank player at launch" bug:
/// the 60s background sync poll was persisting to SQLite but never
/// emitting `PlaybackChanged`, so subscribed clients never re-rendered
/// when playback changed on another Spotify device. After this fix
/// `sync_playback` must broadcast on every successful poll.
#[tokio::test]
async fn playback_sync_broadcasts_playback_changed_for_subscribed_clients() {
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let ctx = FakeCtx {
        store: Store::in_memory().await.expect("in-memory store"),
        shutdown_rx,
        emitted: std::sync::Mutex::new(Vec::new()),
        fake_clock: std::sync::Mutex::new(spotuify_core::Playback::default()),
    };

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
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let ctx = FakeCtx {
        store: Store::in_memory().await.expect("in-memory store"),
        shutdown_rx,
        emitted: std::sync::Mutex::new(Vec::new()),
        fake_clock: std::sync::Mutex::new(spotuify_core::Playback::default()),
    };

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
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let ctx = FakeCtx {
        store: Store::in_memory().await.expect("in-memory store"),
        shutdown_rx,
        emitted: std::sync::Mutex::new(Vec::new()),
        fake_clock: std::sync::Mutex::new(spotuify_core::Playback::default()),
    };

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
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let ctx = FakeCtx {
        store: Store::in_memory().await.expect("in-memory store"),
        shutdown_rx,
        emitted: std::sync::Mutex::new(Vec::new()),
        fake_clock: std::sync::Mutex::new(spotuify_core::Playback::default()),
    };

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
    assert_eq!(synced_first, 1, "first poll must emit (state changed from empty)");

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
