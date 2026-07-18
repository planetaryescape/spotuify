//! Provider-neutral background sync engine.
//!
//! [`SyncContext`] supplies host-owned persistence, indexing, events, and
//! scheduling state. Provider calls enter through [`SyncProvider`], keeping
//! this crate independent of concrete adapters and network clients.

pub mod privacy;
pub mod sync_loop;

pub use privacy::{redact_search_query_if_disabled, PrivacyGate};
pub use sync_loop::{
    spawn_background_scheduler, sync_provider_target_bounded,
    sync_provider_target_bounded_with_timeout, sync_target, sync_target_isolated,
    sync_target_isolated_with_timeout, AbortOnDropTask,
};

use spotuify_core::{
    Device, MediaItem, MusicProvider, Playback, ProviderError, ProviderId, Queue, RemoteTransport,
};
use spotuify_protocol::{DaemonEvent, SyncTargetData};
use spotuify_store::Store;
use std::sync::Arc;
use tokio::runtime::Handle as RuntimeHandle;
use tokio::sync::{watch, Mutex};

/// One provider's sync facets. Identity is read from the music adapter and
/// persisted separately from domain names so sync state cannot collide.
#[derive(Clone)]
pub struct SyncProvider {
    pub music: Arc<dyn MusicProvider>,
    pub transport: Option<Arc<dyn RemoteTransport>>,
}

impl SyncProvider {
    pub fn new(
        music: Arc<dyn MusicProvider>,
        transport: Option<Arc<dyn RemoteTransport>>,
    ) -> anyhow::Result<Self> {
        if let Some(transport) = transport.as_ref() {
            if transport.provider_id() != music.id() || transport.uri_scheme() != music.uri_scheme()
            {
                return Err(ProviderError::InvalidInput {
                    field: "sync_provider".to_string(),
                    message:
                        "music and transport facets must share provider identity and URI scheme"
                            .to_string(),
                }
                .into());
            }
        }
        if transport.is_some() && music.capabilities().transport.is_none() {
            return Err(ProviderError::InvalidInput {
                field: "sync_provider.transport".to_string(),
                message: "transport facet requires a declared transport capability".to_string(),
            }
            .into());
        }
        Ok(Self { music, transport })
    }

    pub fn id(&self) -> &str {
        self.music.id().as_str()
    }

    pub fn provider_id(&self) -> &ProviderId {
        self.music.id()
    }
}

/// Context the sync engine needs from its host process. The binary's
/// `DaemonState` impls this; tests can supply a fake implementation.
#[async_trait::async_trait]
pub trait SyncContext: Send + Sync {
    fn shutdown_receiver(&self) -> watch::Receiver<bool>;
    /// Optional monotonic host revision for provider registry/auth/config
    /// invalidation. The scheduler reconciles its lane tasks whenever this
    /// changes, allowing a new adapter instance to replace the same provider
    /// identity without restarting the daemon.
    fn sync_provider_revision_receiver(&self) -> Option<watch::Receiver<u64>> {
        None
    }
    fn store(&self) -> &Store;
    fn emit_event(&self, event: DaemonEvent);
    /// Per-provider, per-lane sync locks. The default returns no locks.
    /// Hosts return locks keyed by provider identity and lane so the slow scheduler's
    /// `Playlists`/`Library` fetches (which can stall for tens of
    /// seconds on a slow Spotify response) don't block the fast
    /// scheduler's `Playback`/`Queue`/`Devices`/`Recent` cadence.
    /// For `SyncTargetData::All` (full refresh on demand), return both
    /// lane locks in a stable order so callers serialize correctly.
    fn sync_locks_for(&self, _provider_id: &str, _target: SyncTargetData) -> Vec<Arc<Mutex<()>>> {
        Vec::new()
    }
    /// All provider adapters participating in sync. Every scheduler lane is
    /// isolated and backpressured independently per returned provider.
    async fn sync_providers(&self) -> anyhow::Result<Vec<SyncProvider>>;

    async fn index_media_items(
        &self,
        _provider_id: &str,
        _items: &[MediaItem],
        _saved: bool,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn remove_indexed_media_items(&self, _uris: &[String]) -> anyhow::Result<()> {
        Ok(())
    }

    fn warm_queue(&self, _queue: &Queue) {}

    /// Give hosts a chance to overlay daemon-owned optimistic queue
    /// mutations before a live queue poll is persisted or broadcast.
    /// Default is identity for tests and hosts without optimistic state.
    fn overlay_pending_queue_appends(
        &self,
        _provider: &ProviderId,
        queue: Queue,
        _now_ms: i64,
    ) -> Queue {
        queue
    }

    /// Feed a Web API playback poll into the host's in-memory clock so
    /// subsequent `snapshot_playback` reads reflect the freshest state.
    /// Returns `true` when the sample was applied (caller should then
    /// broadcast `DaemonEvent::PlaybackChanged`), `false` when the
    /// clock rejected it (stale-by-mutation, lower-priority source,
    /// etc.). Default no-op for hosts that don't maintain a clock.
    fn apply_playback_poll(
        &self,
        _provider: &ProviderId,
        _playback: &Playback,
        _captured_seq: u64,
        _state_seq: u64,
        _sampled_at_ms: i64,
        _provider_timestamp_ms: Option<i64>,
    ) -> bool {
        false
    }

    /// Prepare the durable representation of a playback poll without changing
    /// canonical in-memory state. Hosts may suppress transient empty samples.
    fn prepare_playback_poll(
        &self,
        playback: &Playback,
        _sampled_at_ms: i64,
        _provider_timestamp_ms: Option<i64>,
    ) -> Option<Playback> {
        Some(playback.clone())
    }

    /// Snapshot the host's current `Playback` view. Default returns
    /// `Playback::default()` for hosts that don't maintain a clock.
    fn snapshot_playback(&self) -> Playback {
        Playback::default()
    }

    /// Snapshot the host's current `Queue` view. Default falls back to
    /// `store().latest_queue(500)` so hosts without a richer cache
    /// still get the SQLite-persisted view.
    async fn snapshot_queue(&self, provider: &ProviderId) -> Queue {
        self.store()
            .latest_provider_queue(500, provider)
            .await
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// Snapshot the host's current device list. Default reads from
    /// `store().list_devices()`.
    async fn snapshot_devices(&self, provider: &ProviderId) -> Vec<Device> {
        self.store()
            .list_provider_devices(provider)
            .await
            .unwrap_or_default()
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

    /// Returns `true` when the daemon's own embedded librespot device is
    /// the active player. In that state librespot's player events feed
    /// the clock live, so the Web API `/me/player` poll is redundant and
    /// the scheduler downgrades it to a slow reconciliation. Default
    /// `false` keeps full polling for hosts without an embedded session.
    fn embedded_is_active_playback(&self) -> bool {
        false
    }

    /// Returns `true` when the host's mutation counter has not
    /// advanced since `captured_seq`. When `false`, the caller should
    /// discard whatever it just read from Spotify because a newer
    /// local mutation has superseded it. Default `true` (no gating)
    /// for hosts that don't track a counter.
    fn may_apply_transport_update(&self, _provider: &ProviderId, _captured_seq: u64) -> bool {
        true
    }

    /// Persist a playback poll only while it still precedes every local
    /// transport mutation. Hosts with a transport mutation lane should
    /// override this and hold that lane across the final sequence check and
    /// SQLite write; that ordering prevents a stale poll from becoming the
    /// newest durable snapshot after a newer command.
    async fn prepare_and_persist_playback_poll_if_current(
        &self,
        provider: &ProviderId,
        playback: &Playback,
        captured_seq: u64,
        sampled_at_ms: i64,
        provider_timestamp_ms: Option<i64>,
    ) -> anyhow::Result<Option<(u32, Playback)>> {
        if !self.may_apply_transport_update(provider, captured_seq) {
            return Ok(None);
        }
        let Some(candidate) =
            self.prepare_playback_poll(playback, sampled_at_ms, provider_timestamp_ms)
        else {
            return Ok(None);
        };
        let written = self
            .store()
            .persist_provider_playback_bulk(provider, &candidate)
            .await?;
        Ok(Some((written, candidate)))
    }

    async fn persist_queue_poll_if_current(
        &self,
        provider: &ProviderId,
        queue: &Queue,
        captured_seq: u64,
    ) -> anyhow::Result<Option<u32>> {
        if !self.may_apply_transport_update(provider, captured_seq) {
            return Ok(None);
        }
        Ok(Some(
            self.store()
                .persist_provider_queue_bulk(provider, queue)
                .await?,
        ))
    }

    async fn persist_devices_poll_if_current(
        &self,
        provider: &ProviderId,
        devices: &[Device],
        captured_seq: u64,
    ) -> anyhow::Result<Option<u32>> {
        if !self.may_apply_transport_update(provider, captured_seq) {
            return Ok(None);
        }
        Ok(Some(
            self.store()
                .replace_provider_devices(provider, devices)
                .await?,
        ))
    }

    /// Optional host runtime retained for compatibility and non-provider
    /// background work. Provider discovery and adapter futures deliberately
    /// remain on the caller's runtime because reqwest/hyper connection drivers
    /// may be runtime-affine.
    fn background_runtime(&self) -> Option<RuntimeHandle> {
        None
    }
}

/// Decide whether to refetch a playlist's full track listing.
///
/// Providers may attach an opaque playlist version token that changes on
/// mutation. Comparing the local cached value against the fresh remote value
/// tells us whether a full playlist-track refetch is worth making. Spotify's
/// adapter maps its `snapshot_id` into this token.
///
/// Returns true when in doubt -- a missing token on either side
/// means we can't prove unchanged.
pub fn should_refetch_playlist_tracks(
    local_version_token: Option<&str>,
    remote_version_token: Option<&str>,
) -> bool {
    match (local_version_token, remote_version_token) {
        // First sync: nothing local yet.
        (None, _) => true,
        // Remote didn't include a version token; can't compare.
        (_, None) => true,
        // Both present -- refetch only if they differ.
        (Some(local), Some(remote)) => local != remote,
    }
}
