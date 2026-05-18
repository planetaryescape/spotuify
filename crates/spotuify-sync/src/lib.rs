//! Phase 6.5 sync refetch gate helpers + Phase 7 SyncContext trait.
//!
//! Pure functions for the daemon's background sync loop to decide
//! whether to skip expensive paginated refetches when nothing has
//! changed since the last successful sync.
//!
//! The [`SyncContext`] trait abstracts over the daemon host so this
//! crate owns the background sync loop without depending on daemon
//! internals.
//!
//! Patterns from ncspot `library.rs:140-148` (snapshot_id-aware
//! playlist sync) and ncspot `library.rs:499-514` (saved-tracks
//! page-0 unchanged shortcut).

pub mod privacy;
pub mod sync_loop;

pub use privacy::{redact_search_query_if_disabled, PrivacyGate};
pub use sync_loop::{spawn_background_scheduler, sync_target};

use spotuify_core::{Device, Playback};
use spotuify_protocol::{DaemonEvent, SyncTargetData};
use spotuify_spotify::{
    client::{MediaItem, Queue},
    SpotifyClient,
};
use spotuify_store::Store;
use std::sync::Arc;
use tokio::runtime::Handle as RuntimeHandle;
use tokio::sync::{watch, Mutex};

/// Context the sync engine needs from its host process. The binary's
/// `DaemonState` impls this; tests can supply a fake implementation.
#[async_trait::async_trait]
pub trait SyncContext: Send + Sync {
    fn shutdown_receiver(&self) -> watch::Receiver<bool>;
    fn store(&self) -> &Store;
    fn emit_event(&self, event: DaemonEvent);
    /// Per-target sync lock. The default returns `None` (no lock).
    /// Hosts return a lock keyed by domain so the slow scheduler's
    /// `Playlists`/`Library` fetches (which can stall for tens of
    /// seconds on a slow Spotify response) don't block the fast
    /// scheduler's `Playback`/`Queue`/`Devices`/`Recent` cadence.
    /// For `SyncTargetData::All` (full refresh on demand), return the
    /// most restrictive lock so callers serialize correctly.
    fn sync_lock_for(&self, _target: SyncTargetData) -> Option<Arc<Mutex<()>>> {
        None
    }
    /// A live Spotify client. `&self` so impls can manage their own
    /// caching / token-refresh / fake-mode injection without sync
    /// having to know.
    async fn spotify_client(&self) -> anyhow::Result<SpotifyClient>;

    async fn index_media_items(&self, _items: &[MediaItem], _saved: bool) -> anyhow::Result<()> {
        Ok(())
    }

    fn warm_queue(&self, _queue: &Queue) {}

    /// Feed a Web API playback poll into the host's in-memory clock so
    /// subsequent `snapshot_playback` reads reflect the freshest state.
    /// Returns `true` when the sample was applied (caller should then
    /// broadcast `DaemonEvent::PlaybackChanged`), `false` when the
    /// clock rejected it (stale-by-mutation, lower-priority source,
    /// etc.). Default no-op for hosts that don't maintain a clock.
    fn apply_playback_poll(
        &self,
        _playback: &Playback,
        _captured_seq: u64,
        _state_seq: u64,
        _sampled_at_ms: i64,
        _provider_timestamp_ms: Option<i64>,
    ) -> bool {
        false
    }

    /// Snapshot the host's current `Playback` view. Default returns
    /// `Playback::default()` for hosts that don't maintain a clock.
    fn snapshot_playback(&self) -> Playback {
        Playback::default()
    }

    /// Snapshot the host's current `Queue` view. Default falls back to
    /// `store().latest_queue(500)` so hosts without a richer cache
    /// still get the SQLite-persisted view.
    async fn snapshot_queue(&self) -> Queue {
        self.store()
            .latest_queue(500)
            .await
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// Snapshot the host's current device list. Default reads from
    /// `store().list_devices()`.
    async fn snapshot_devices(&self) -> Vec<Device> {
        self.store().list_devices().await.unwrap_or_default()
    }

    /// How many subscribers are currently attached to the daemon's
    /// event broadcast. Used by `spawn_background_scheduler` to relax
    /// poll cadence when no client cares. Default `0` keeps the
    /// scheduler in idle cadence for hosts that don't expose a count
    /// (test fakes).
    fn event_subscriber_count(&self) -> usize {
        0
    }

    /// Snapshot the host's monotonically-increasing mutation counter.
    /// Sync should call this BEFORE issuing a Spotify state-read so a
    /// concurrent PlaybackCommand can be detected on the way back.
    /// Default `0` opts out of the gate for hosts that don't care
    /// (test fakes).
    fn observe_mutation_seq(&self) -> u64 {
        0
    }

    /// Returns `true` when the host's mutation counter has not
    /// advanced since `captured_seq`. When `false`, the caller should
    /// discard whatever it just read from Spotify because a newer
    /// local mutation has superseded it. Default `true` (no gating)
    /// for hosts that don't track a counter.
    fn may_apply_playback_update(&self, _captured_seq: u64) -> bool {
        true
    }

    /// Optional dedicated runtime handle for the sync scheduler. When
    /// provided, `spawn_background_scheduler` spawns its long-running
    /// loops there instead of the caller's runtime; that isolates
    /// sync flushes from hot-path IPC/handler work on the main
    /// runtime. Returning `None` falls back to `tokio::spawn` which
    /// uses the current runtime (the default for test fakes).
    fn background_runtime(&self) -> Option<RuntimeHandle> {
        None
    }
}

/// Decide whether to refetch a playlist's full track listing.
///
/// The Spotify Playlist envelope carries `snapshot_id`, a string token
/// that changes on every mutation. Comparing the local cached value
/// against the fresh `GET /playlists/{id}` response tells us whether
/// the expensive paginated `GET /playlists/{id}/tracks` call is worth
/// making.
///
/// Returns true when in doubt -- a missing snapshot on either side
/// means we can't prove unchanged.
pub fn should_refetch_playlist_tracks(
    local_snapshot: Option<&str>,
    remote_snapshot: Option<&str>,
) -> bool {
    match (local_snapshot, remote_snapshot) {
        // First sync: nothing local yet.
        (None, _) => true,
        // Remote didn't include a snapshot id; can't compare.
        (_, None) => true,
        // Both present -- refetch only if they differ.
        (Some(local), Some(remote)) => local != remote,
    }
}

/// Decide whether to refetch the user's saved-tracks library beyond
/// page 0.
///
/// Spotify's saved-tracks endpoint returns `(total, items)` per page.
/// If both the total count AND the first page's IDs match what we
/// have locally, the library is unchanged and we can skip the
/// remaining pages.
///
/// Ordering matters: Spotify returns saved tracks in reverse-added
/// order, so a new add at the top changes both `local_first_ids` and
/// `total`.
///
/// This is an approximation: a rare reorder-without-add-or-remove
/// would slip through. Acceptable trade-off given the API-cost
/// savings for the common steady-state case.
pub fn should_refetch_saved_tracks(
    local_total: u64,
    local_first_ids: &[&str],
    remote_total: u64,
    remote_first_ids: &[&str],
) -> bool {
    if local_total != remote_total {
        return true;
    }
    local_first_ids != remote_first_ids
}
