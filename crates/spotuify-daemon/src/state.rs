use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures::StreamExt;
use parking_lot::RwLock;
use spotuify_core::{BackendKind, Queue};
use spotuify_player::{DeviceId, PlayerBackend, PlayerEvent, PlayerResult, RepeatMode};
use tokio::runtime::{Builder as RuntimeBuilder, Handle as RuntimeHandle, Runtime};
use tokio::sync::{broadcast, mpsc, oneshot, watch, Mutex, OwnedMutexGuard};
use tokio::task::JoinHandle;

use crate::queue_warm::{QueueWarmRequest, QueueWarmScheduler};

/// Owns the dedicated background `Runtime` and shuts it down without
/// blocking when dropped. Dropping a `Runtime` directly inside an
/// async context panics (Tokio calls `block_on` internally to wait for
/// shutdown); routing the drop through `shutdown_background` avoids
/// that without leaking tasks. `Arc<OwnedBgRuntime>` makes the drop
/// fire exactly when the last reference is released, which on real
/// daemons is the IPC server's main shutdown path.
struct OwnedBgRuntime {
    inner: Option<Runtime>,
}

impl OwnedBgRuntime {
    fn new(runtime: Runtime) -> Self {
        Self {
            inner: Some(runtime),
        }
    }

    fn handle(&self) -> RuntimeHandle {
        self.inner
            .as_ref()
            .expect("bg runtime taken before drop")
            .handle()
            .clone()
    }
}

impl Drop for OwnedBgRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.inner.take() {
            runtime.shutdown_background();
        }
    }
}

use crate::analytics::{AnalyticsSource, AnalyticsStore};
use crate::player_factory;
use spotuify_protocol::{
    DaemonEvent, DaemonStatus, IpcMessage, IpcPayload, Request, IPC_PROTOCOL_VERSION,
};

use crate::viz_coordinator::VizCoordinator;
use spotuify_search::{SearchIndex, SearchServiceHandle};
use spotuify_spotify::auth::StoredToken;
use spotuify_spotify::client::{MediaItem, SchemaCompatReporter, SpotifyClient};
use spotuify_spotify::config::Config;
use spotuify_spotify::rate_limit::{Priority, RateLimitedClient};
use spotuify_store::Store;

type PlayerBox = Box<dyn PlayerBackend>;
type PlayerEventStream = tokio_stream::wrappers::UnboundedReceiverStream<PlayerEvent>;
type PlayerTokenSlot = Arc<RwLock<Option<String>>>;
type PlayerBuildResult = (PlayerBox, PlayerEventStream, PlayerTokenSlot);

enum PlayerCommand {
    RegisterDevice {
        name: String,
        resp: oneshot::Sender<Result<DeviceId>>,
    },
    Reconnect {
        name: String,
        resp: oneshot::Sender<Result<DeviceId>>,
    },
    IsConnected {
        resp: oneshot::Sender<bool>,
    },
    Kind {
        resp: oneshot::Sender<BackendKind>,
    },
    MercuryGet {
        uri: String,
        resp: oneshot::Sender<Result<bytes::Bytes>>,
    },
    QueueAdd {
        uri: String,
        resp: oneshot::Sender<PlayerResult<()>>,
    },
    /// Transport commands routed through the embedded librespot
    /// backend (Spirc) — bypasses the slow Web API path.
    Transport {
        cmd: TransportCmd,
        resp: oneshot::Sender<PlayerResult<()>>,
    },
    Shutdown {
        resp: oneshot::Sender<()>,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum TransportCmd {
    PlayUri { uri: String, position_ms: u32 },
    Pause,
    Resume,
    Next,
    Previous,
    Seek { position_ms: u32 },
    Volume { percent: u8 },
    Shuffle { on: bool },
    Repeat { mode: RepeatMode },
}

enum PlayerWarmCommand {
    PreloadUri { uri: String },
}

pub(crate) struct DaemonState {
    started_at: Instant,
    shutdown_tx: watch::Sender<bool>,
    pub(crate) event_tx: broadcast::Sender<IpcMessage>,
    store: Store,
    search: SearchServiceHandle,
    search_worker: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    background_tasks: StdMutex<Vec<JoinHandle<()>>>,
    token_cache: Arc<Mutex<Option<StoredToken>>>,
    /// Shared Spotify HTTP/backpressure runtime. `spotify_client()`
    /// still reloads config per request, but clones this runtime.
    spotify_rate_limiter: Mutex<Option<RateLimitedClient>>,
    transport_mutation_lock: Arc<Mutex<()>>,
    library_mutation_lock: Arc<Mutex<()>>,
    operation_mutation_lock: Arc<Mutex<()>>,
    /// Serializes fast-cadence syncs (Playback/Queue/Devices/Recent).
    /// Kept separate from `slow_sync_lock` so a 10-second `/me/playlists`
    /// stall on the slow scheduler can't block the 3-second player
    /// refresh that drives the TUI now-playing widget.
    fast_sync_lock: Arc<Mutex<()>>,
    /// Serializes slow-cadence syncs (Playlists/Library) and the
    /// full-refresh `SyncTargetData::All` path.
    slow_sync_lock: Arc<Mutex<()>>,
    playlist_mutation_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Once-per-process latch so the scope-drift banner fires at most
    /// once even though `spotify_client()` is called per request.
    scope_reauth_emitted: std::sync::atomic::AtomicBool,
    /// Latch — set the moment Spotify reports `invalid_grant` / refresh
    /// token revoked. Read by mutation handlers to fail-fast with a
    /// useful "re-authenticate" message instead of fire-and-forgetting
    /// commands into a librespot session that can't fetch any data.
    auth_revoked: std::sync::atomic::AtomicBool,
    /// Device name we last registered the embedded librespot session
    /// under. Set the first time `ensure_player_ready(name)` is called.
    /// Used by `own_device_id()` to derive the deterministic SHA-1
    /// device_id we publish to Spotify — selection code prefers an
    /// entry matching this ID so stale namesakes in
    /// `/v1/me/player/devices` are harmless.
    own_device_name: parking_lot::Mutex<Option<String>>,
    /// Phase 6.9 — recent-event ring buffer used by `doctor` to surface
    /// rate-limit / auth-error / schema-compat findings.
    event_log: Arc<tokio::sync::Mutex<spotuify_protocol::EventLog>>,

    // Phase 9.1 — player backend abstraction.
    //
    // `player` is the in-process backend the daemon talks to for
    // playback. Today (9.1) it's ConnectOnly or Spotifyd; 9.2+ adds
    // Embedded.
    //
    // `player_token_slot` is the seam the daemon uses to publish the
    // current Web API bearer token into the backend's TokenProvider.
    // Background refresh keeps this fresh; backends snapshot it
    // synchronously on every API call.
    player_tx: mpsc::Sender<PlayerCommand>,
    player_warm_tx: mpsc::Sender<PlayerWarmCommand>,
    player_token_slot: PlayerTokenSlot,
    player_actor: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    player_worker: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    queue_warm: QueueWarmScheduler,
    queue_warm_rx: StdMutex<Option<mpsc::Receiver<QueueWarmRequest>>>,
    // Phase 10 (F11) — listening-session tracker observed by
    // forward_player_events. Foundation pass: state machine label only;
    // Pass 2 (P10.1) wires finalize → listen_facts insertion.
    pub(crate) _session_tracker: Arc<crate::session_tracker::SessionTracker>,
    /// Phase 14 (P14-G) — system-integration actor: media controls,
    /// notifications, shell hooks, Discord RPC. Subscribes to every
    /// emitted `DaemonEvent` via `emit_event`.
    pub(crate) system_integration: Arc<spotuify_system::SystemIntegration>,
    viz_coordinator: Arc<VizCoordinator>,
    /// Monotonically-increasing mutation counter. Bumped on every
    /// hot-path PlaybackCommand entry. Background pollers (sync loop,
    /// `spawn_playback_refresh`, `spawn_queue_refresh`,
    /// `spawn_devices_refresh`) capture the seq before issuing a
    /// Spotify state read and drop the result if the local seq has
    /// advanced — Spotify's playback state is eventually consistent
    /// on mutation, so a poll that started before the user's Pause
    /// often returns the stale pre-mutation snapshot and would
    /// otherwise clobber the optimistic local cache. Same shape as
    /// Linear's `lastSyncId`.
    mutation_seq: Arc<AtomicU64>,
    /// Phase 2 — daemon-owned `PlaybackClock`. Single source of truth
    /// for "what's playing, where, since when". Fed by player events
    /// (highest), command results, and Web API polls (lowest). Reads
    /// from `parking_lot::RwLock` so `PlaybackGet` is sub-millisecond
    /// without any `.await`. See `crate::clock` for the priority rules.
    pub(crate) playback_clock: Arc<crate::clock::PlaybackClock>,
    /// Dedicated runtime for genuinely-bulk background work: the
    /// 60s/15min sync scheduler, the daily retention loop, large
    /// analytics flushes. Keeping these off the main runtime means a
    /// sync flush that floods its workers with awaits never starves
    /// the IPC/handler/player-forwarder tasks that need sub-100ms
    /// turnaround. Hot-path background work (`spawn_*_refresh`,
    /// optimistic-mutation bodies, session_tracker finalize) stays
    /// on the main runtime because those are themselves on the
    /// user-perceived path.
    bg_runtime: Arc<OwnedBgRuntime>,
}

impl DaemonState {
    pub(crate) async fn new() -> Result<Self> {
        let (shutdown_tx, _) = watch::channel(false);
        // Capacity sized for optimistic-mutation bursts: every transport
        // mutation now emits MutationAccepted + later MutationFinalized
        // in addition to its action-specific PlaybackChanged /
        // DevicesChanged / LibraryChanged events. A slow TUI lagging on
        // an event tide at 128 used to spill RecvError::Lagged and drop
        // events; 1024 leaves comfortable headroom for the worst case
        // (a sync flush that publishes a wave of SyncFinished /
        // PlaylistsChanged per playlist).
        let (event_tx, _) = broadcast::channel(1024);
        let store = Store::open_default().await?;
        // Phase 13 (P13-F) — refuse to start if the on-disk schema is
        // newer than this binary understands. Migrations only ever
        // run forward; a downgrade scenario without this guard would
        // silently corrupt or misread newer columns.
        store
            .check_cache_version()
            .await
            .context("cache schema mismatch (refusing to start)")?;
        if let Err(err) = recover_pending_receipts(&store, &event_tx, spotuify_core::now_ms()).await
        {
            tracing::warn!(error = %err, "failed to recover pending mutation receipts");
        }

        // Scope-drift detection used to fire here as a proactive
        // keychain read; on macOS that triggered a "spotuify wants to
        // access the keychain" prompt at every cold start, on top of
        // the prompts the lazy `access_token_cached` path already
        // causes. Net effect: 3–5 prompts on every fresh launch.
        //
        // Recovery: defer the scope-drift check to the first real API
        // call. `SpotifyClient::access_token_cached` already loads the
        // token once and caches it for the process; we hook the
        // scope-drift check off that single read (see
        // `emit_scope_reauth_event_if_needed` wiring in the request
        // handler). Net effect: keychain is read exactly as many
        // times as a vanilla "fetch token, refresh when expiring"
        // path would read it — no extra prompts.
        //
        // The keep-only-on-explicit-opt-in escape hatch is gone for
        // the same reason; if a future build wants the proactive
        // surface back, it has to share the same cached token rather
        // than re-reading.
        let _ = &event_tx;
        let (search, search_worker) =
            SearchServiceHandle::start(SearchIndex::open(store.index_path())?);

        let viz_coordinator = VizCoordinator::new(event_tx.clone());

        // Phase 0 — librespot-only. If embedded init fails the daemon
        // returns the error to its caller (binary entry point) which
        // logs it and exits non-zero. SPOTUIFY_FAKE_SPOTIFY routes to
        // MockPlayerBackend so integration tests still work.
        let (player_box, player_stream, token_slot) =
            build_player_or_default(Some(viz_coordinator.shared_analyzer()))
                .context("daemon failed to construct player backend")?;
        let embedded_sink_on_ready = player_box.kind() == BackendKind::Embedded;
        let (player_tx, player_warm_tx, player_actor) = spawn_player_actor(player_box);
        let (queue_warm, queue_warm_rx) = QueueWarmScheduler::new();
        // Phase 10 (P10.1): SessionTracker writes ListenFact rows to
        // the store and emits ListenQualified into the event broadcast
        // when the qualification rule fires.
        let session_tracker = Arc::new(crate::session_tracker::SessionTracker::with_store(
            Arc::new(store.clone()),
            event_tx.clone(),
        ));

        // Phase 2/8 — construct the playback clock NOW so we can pass it
        // into `forward_player_events`. Seeded from the durable store so
        // the first `PlaybackGet` after start returns something useful
        // before any live event arrives.
        let playback_clock = {
            let clock = crate::clock::PlaybackClock::new();
            if let Ok(Some(cached)) = store.latest_playback().await {
                clock.seed_from_cache(
                    cached,
                    spotuify_core::PlaybackStateSource::Cache,
                    spotuify_core::now_ms(),
                );
            } else if let Ok(Some(recent)) = store.latest_playback_or_recent().await {
                clock.seed_from_cache(
                    recent,
                    spotuify_core::PlaybackStateSource::RecentFallback,
                    spotuify_core::now_ms(),
                );
            }
            clock
        };

        let event_tx_for_worker = event_tx.clone();
        let tracker_for_worker = session_tracker.clone();
        let viz_for_worker = viz_coordinator.clone();
        let clock_for_worker = playback_clock.clone();
        let player_worker = tokio::spawn(async move {
            forward_player_events(
                player_stream,
                event_tx_for_worker,
                tracker_for_worker,
                viz_for_worker,
                clock_for_worker,
                embedded_sink_on_ready,
            )
            .await;
        });

        // Phase 14 (P14-G) — system-integration actor. Reads config
        // for opt-in subsystems; if the config can't be loaded
        // (first-run / missing client_id) we still build the cover
        // cache and a no-op hook dispatcher so the daemon stays up.
        let system_config = build_system_config();
        let system_integration = Arc::new(spotuify_system::SystemIntegration::spawn(system_config));

        // Phase 17 — apply persisted viz config. Best-effort: missing
        // first-run config leaves the default-off coordinator idle.
        if let Ok(config) = Config::load() {
            apply_viz_config(&viz_coordinator, &config).await;
        }

        Ok(Self {
            started_at: Instant::now(),
            shutdown_tx,
            event_tx,
            store,
            search,
            search_worker: tokio::sync::Mutex::new(Some(search_worker)),
            background_tasks: StdMutex::new(Vec::new()),
            token_cache: Arc::new(Mutex::new(None)),
            spotify_rate_limiter: Mutex::new(None),
            transport_mutation_lock: Arc::new(Mutex::new(())),
            library_mutation_lock: Arc::new(Mutex::new(())),
            operation_mutation_lock: Arc::new(Mutex::new(())),
            fast_sync_lock: Arc::new(Mutex::new(())),
            slow_sync_lock: Arc::new(Mutex::new(())),
            playlist_mutation_locks: Mutex::new(HashMap::new()),
            scope_reauth_emitted: std::sync::atomic::AtomicBool::new(false),
            auth_revoked: std::sync::atomic::AtomicBool::new(false),
            own_device_name: parking_lot::Mutex::new(None),
            event_log: Arc::new(tokio::sync::Mutex::new(spotuify_protocol::EventLog::new(
                128,
            ))),
            player_tx,
            player_warm_tx,
            player_token_slot: token_slot,
            player_actor: tokio::sync::Mutex::new(Some(player_actor)),
            player_worker: tokio::sync::Mutex::new(Some(player_worker)),
            queue_warm,
            queue_warm_rx: StdMutex::new(Some(queue_warm_rx)),
            _session_tracker: session_tracker,
            system_integration,
            viz_coordinator,
            mutation_seq: Arc::new(AtomicU64::new(0)),
            playback_clock,
            bg_runtime: Arc::new(OwnedBgRuntime::new(
                RuntimeBuilder::new_multi_thread()
                    .thread_name("spotuify-bg")
                    // Two workers comfortably handle: 60s playback/queue/
                    // devices/recent polls (4 awaits, all I/O-bound), the
                    // 15min playlists/library scheduler, and the daily
                    // retention sweep. Bulk persists run in here too but
                    // they're chunked so no single await holds a worker
                    // for long.
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .context("failed to build background runtime")?,
            )),
        })
    }

    pub(crate) fn viz_coordinator(&self) -> Arc<VizCoordinator> {
        self.viz_coordinator.clone()
    }

    /// Phase 2 — borrow the playback clock. Cheap clone of the `Arc`.
    pub(crate) fn playback_clock(&self) -> Arc<crate::clock::PlaybackClock> {
        self.playback_clock.clone()
    }

    /// Phase 2 — sub-millisecond `Playback` read. The IPC handler for
    /// `PlaybackGet` calls this instead of touching SQLite.
    pub(crate) fn snapshot_playback(&self) -> spotuify_core::Playback {
        self.playback_clock.snapshot()
    }

    /// Bump the mutation counter to a value strictly greater than
    /// every previously-observed value, and return the new value.
    /// Call from every hot-path PlaybackCommand dispatch entry; the
    /// return value lets the caller include the seq in an optimistic
    /// reply.
    pub(crate) fn bump_mutation_seq(&self) -> u64 {
        self.mutation_seq.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Snapshot the current mutation counter without bumping. Pollers
    /// call this *before* issuing a Spotify state read, then pass the
    /// value to `may_apply_state_update` after the read returns.
    pub(crate) fn current_mutation_seq(&self) -> u64 {
        self.mutation_seq.load(Ordering::Acquire)
    }

    /// Returns `true` when the mutation counter has not advanced since
    /// `captured_seq`. When `false`, a hot-path PlaybackCommand fired
    /// while the caller's poll was in flight; the caller must discard
    /// the polled state because Spotify's eventual-consistency window
    /// means the result is likely a stale pre-mutation snapshot.
    pub(crate) fn may_apply_state_update(&self, captured_seq: u64) -> bool {
        self.current_mutation_seq() == captured_seq
    }

    pub(crate) async fn mutation_guard(&self, request: &Request) -> Option<OwnedMutexGuard<()>> {
        let lock = match request {
            Request::PlaybackCommand { .. }
            | Request::DeviceTransfer { .. }
            | Request::QueueAdd { .. } => Some(self.transport_mutation_lock.clone()),
            Request::PlaylistAddItems { playlist, .. }
            | Request::PlaylistRemoveItems { playlist, .. }
            | Request::PlaylistTracks { playlist } => Some(self.playlist_lane(playlist).await),
            Request::PlaylistCreate { .. } => Some(self.playlist_lane("__playlist_create__").await),
            Request::LibrarySave { .. } | Request::LibraryUnsave { .. } => {
                Some(self.library_mutation_lock.clone())
            }
            Request::OpsUndo { .. } | Request::OpsRedo { .. } => {
                Some(self.operation_mutation_lock.clone())
            }
            _ => None,
        }?;
        Some(lock.lock_owned().await)
    }

    async fn playlist_lane(&self, key: &str) -> Arc<Mutex<()>> {
        let mut lanes = self.playlist_mutation_locks.lock().await;
        lanes
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub(crate) async fn apply_runtime_config(&self, config: &Config) {
        apply_viz_config(&self.viz_coordinator, config).await;
    }

    /// Register the daemon's Connect device. Idempotent — calling
    /// twice with the same name is safe (backends short-circuit).
    /// Emits `DaemonEvent::PlayerReady` on success or `PlayerFailed`
    /// on terminal error (the event-forward task does the
    /// translation; we just propagate Result here).
    pub(crate) async fn ensure_player_ready(&self, name: &str) -> Result<DeviceId> {
        // Record the name BEFORE issuing the register call so `own_device_id`
        // can answer correctly during the registration round-trip (selection
        // code may query it from a concurrent IPC handler).
        *self.own_device_name.lock() = Some(name.to_string());
        let (resp, rx) = oneshot::channel();
        self.player_tx
            .send(PlayerCommand::RegisterDevice {
                name: name.to_string(),
                resp,
            })
            .await
            .map_err(|err| anyhow::anyhow!("player actor stopped: {err}"))?;
        rx.await
            .map_err(|err| anyhow::anyhow!("player actor stopped: {err}"))?
    }

    /// SHA-1-hex of the device name we registered with the embedded
    /// librespot session. Selection code uses this to recognise our
    /// own Connect device in the (often-bloated) `/v1/me/player/devices`
    /// list and prefer it over stale namesakes left over from prior
    /// daemon runs. Returns `None` before the first
    /// `ensure_player_ready` succeeds.
    ///
    /// Mirrors the device_id librespot publishes — see
    /// `spotuify_player::backends::embedded::derive_device_id`.
    pub(crate) fn own_device_id(&self) -> Option<String> {
        self.own_device_name
            .lock()
            .as_deref()
            .map(derive_device_id_for_name)
    }

    pub(crate) async fn reconnect_player(&self, name: &str) -> Result<DeviceId> {
        let (resp, rx) = oneshot::channel();
        self.player_tx
            .send(PlayerCommand::Reconnect {
                name: name.to_string(),
                resp,
            })
            .await
            .map_err(|err| anyhow::anyhow!("player actor stopped: {err}"))?;
        rx.await
            .map_err(|err| anyhow::anyhow!("player actor stopped: {err}"))?
    }

    /// Snapshot the player's connection state. Backend-agnostic — the
    /// diagnostics module uses this so `doctor` doesn't need to know
    /// which backend is active.
    pub(crate) async fn player_is_connected(&self) -> bool {
        let (resp, rx) = oneshot::channel();
        if self
            .player_tx
            .send(PlayerCommand::IsConnected { resp })
            .await
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    /// Backend kind for diagnostics output.
    #[allow(dead_code)]
    pub(crate) async fn player_kind(&self) -> BackendKind {
        let (resp, rx) = oneshot::channel();
        if self
            .player_tx
            .send(PlayerCommand::Kind { resp })
            .await
            .is_err()
        {
            return BackendKind::Embedded;
        }
        rx.await.unwrap_or(BackendKind::Embedded)
    }

    pub(crate) async fn mercury_get(&self, uri: &str) -> Result<bytes::Bytes> {
        let (resp, rx) = oneshot::channel();
        self.player_tx
            .send(PlayerCommand::MercuryGet {
                uri: uri.to_string(),
                resp,
            })
            .await
            .map_err(|err| anyhow::anyhow!("player actor stopped: {err}"))?;
        rx.await
            .map_err(|err| anyhow::anyhow!("player actor stopped: {err}"))?
    }

    /// Dispatch a transport command through the embedded librespot
    /// backend (Spirc). Returns `Unsupported` for non-Embedded backends
    /// so callers can fall back to the Web API path.
    pub(crate) async fn transport(&self, cmd: TransportCmd) -> PlayerResult<()> {
        let (resp, rx) = oneshot::channel();
        if self
            .player_tx
            .send(PlayerCommand::Transport { cmd, resp })
            .await
            .is_err()
        {
            return Err(spotuify_player::PlayerError::Playback(
                "player actor stopped".to_string(),
            ));
        }
        rx.await.unwrap_or_else(|_| {
            Err(spotuify_player::PlayerError::Playback(
                "player actor stopped".to_string(),
            ))
        })
    }

    /// Append `uri` to the active device's queue via the in-process
    /// backend. Returns `PlayerResult` so callers can detect
    /// `Unsupported` (non-Embedded backends today) and fall back to
    /// the Web API path.
    pub(crate) async fn queue_add(&self, uri: &str) -> PlayerResult<()> {
        let (resp, rx) = oneshot::channel();
        if self
            .player_tx
            .send(PlayerCommand::QueueAdd {
                uri: uri.to_string(),
                resp,
            })
            .await
            .is_err()
        {
            return Err(spotuify_player::PlayerError::Playback(
                "player actor stopped".to_string(),
            ));
        }
        rx.await.unwrap_or_else(|_| {
            Err(spotuify_player::PlayerError::Playback(
                "player actor stopped".to_string(),
            ))
        })
    }

    /// Publish a Web API token into the slot every backend reads.
    /// Called by the token-refresh path (Phase 9.4 wires this for
    /// real; in 9.1 we set it once after first successful refresh).
    #[allow(dead_code)]
    pub(crate) fn update_player_token(&self, token: Option<String>) {
        *self.player_token_slot.write() = token;
    }

    pub(crate) fn socket_path() -> PathBuf {
        spotuify_protocol::paths::socket_path()
    }

    pub(crate) fn pid_path() -> PathBuf {
        spotuify_protocol::paths::pid_path()
    }

    pub(crate) fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown_tx.subscribe()
    }

    pub(crate) fn store(&self) -> &Store {
        &self.store
    }

    pub(crate) fn search(&self) -> &SearchServiceHandle {
        &self.search
    }

    pub(crate) fn start_queue_warm_scheduler(self: &Arc<Self>) -> Option<JoinHandle<()>> {
        let rx = match self.queue_warm_rx.lock() {
            Ok(mut rx) => rx.take(),
            Err(_) => {
                tracing::warn!("queue warm receiver registry poisoned; scheduler disabled");
                None
            }
        }?;
        Some(
            self.bg_runtime_handle()
                .spawn(crate::queue_warm::run_queue_warm_worker(self.clone(), rx)),
        )
    }

    pub(crate) fn warm_queue(&self, queue: &Queue) {
        self.queue_warm.enqueue_queue(queue);
    }

    pub(crate) fn warm_queue_uris(&self, uris: Vec<String>) {
        self.queue_warm.enqueue_uris(uris);
    }

    pub(crate) fn prewarm_next_audio(&self, uri: &str) {
        if let Err(err) = self.player_warm_tx.try_send(PlayerWarmCommand::PreloadUri {
            uri: uri.to_string(),
        }) {
            tracing::debug!(error = %err, uri, "next-track audio prewarm dropped");
        }
    }

    pub(crate) fn request_shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// Whether we've observed that the Spotify refresh token is
    /// revoked. Mutation handlers consult this to fail-fast with a
    /// "re-authenticate" message instead of issuing commands that
    /// silently no-op through a broken auth chain.
    pub(crate) fn auth_revoked(&self) -> bool {
        self.auth_revoked.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Drop the daemon's in-memory token cache and clear the
    /// `auth_revoked` latch so the next `spotify_client()` call
    /// re-reads fresh credentials from keychain/disk. Called by the
    /// `Request::ReloadAuth` IPC handler after a client has completed
    /// an interactive OAuth re-authentication.
    ///
    /// Idempotent — calling this when no token is cached and no latch
    /// is set is a no-op.
    pub(crate) async fn reload_auth(&self) {
        {
            let mut cache = self.token_cache.lock().await;
            *cache = None;
        }
        self.auth_revoked
            .store(false, std::sync::atomic::Ordering::Release);
    }

    pub(crate) fn emit_event(&self, event: DaemonEvent) {
        // Phase 6.9: tap the event stream into the recent-event log so
        // doctor can surface findings. We use try_lock so the fast
        // path stays lock-free; if contended (the log is in mid-read
        // by collect_report), we drop the tap entry rather than block.
        if let Ok(mut log) = self.event_log.try_lock() {
            if let Some(logged) =
                spotuify_protocol::LoggedEvent::from(&event, crate::analytics::now_ms())
            {
                log.push(logged);
            }
        }
        // Phase 14 (P14-G) — fan to system-integration subsystems.
        // Hooks/notifications/media-controls/discord all consume the
        // same DaemonEvent stream. Spawn so we don't block emit.
        let system = self.system_integration.clone();
        let event_for_system = event.clone();
        tokio::spawn(async move {
            system.handle_event(&event_for_system).await;
        });
        let _ = self.event_tx.send(IpcMessage {
            id: 0,
            source: None,
            payload: IpcPayload::Event(event),
        });
    }

    /// Phase 6.9 — snapshot of the event ring for doctor reporting.
    pub(crate) async fn event_log_snapshot(&self) -> Vec<spotuify_protocol::LoggedEvent> {
        self.event_log.lock().await.snapshot()
    }

    pub(crate) async fn shutdown_search(&self) {
        if let Err(err) = self.search.request_shutdown().await {
            tracing::warn!(error = %err, "search worker shutdown signal failed");
        }
        if let Some(handle) = self.search_worker.lock().await.take() {
            if let Err(err) = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await
            {
                tracing::warn!(error = %err, "search worker shutdown timed out");
            }
        }
    }

    pub(crate) fn spawn_background<F>(&self, name: &'static str, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let handle = tokio::spawn(async move {
            tracing::trace!(task = name, "daemon background task started");
            future.await;
            tracing::trace!(task = name, "daemon background task finished");
        });
        match self.background_tasks.lock() {
            Ok(mut tasks) => tasks.push(handle),
            Err(_) => {
                tracing::warn!(
                    task = name,
                    "background task registry poisoned; aborting task"
                );
                handle.abort();
            }
        }
    }

    /// Handle for the dedicated background runtime. Exposed so callers
    /// outside the daemon crate (the sync scheduler in `spotuify-sync`)
    /// can spawn their long-running loops on the bg runtime without
    /// re-implementing the wiring.
    pub(crate) fn bg_runtime_handle(&self) -> RuntimeHandle {
        self.bg_runtime.handle().clone()
    }

    pub(crate) async fn shutdown_background_tasks(&self, timeout: Duration) {
        let tasks = match self.background_tasks.lock() {
            Ok(mut tasks) => std::mem::take(&mut *tasks),
            Err(_) => Vec::new(),
        };
        let deadline = tokio::time::Instant::now() + timeout;
        for mut task in tasks {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                task.abort();
                continue;
            }
            tokio::select! {
                _ = &mut task => {}
                _ = tokio::time::sleep(remaining) => {
                    task.abort();
                    let _ = task.await;
                }
            }
        }
    }

    /// Gracefully shut down the player backend and abort its event
    /// forwarder. Called from the server's main shutdown path.
    pub(crate) async fn shutdown_player(&self) {
        // Best-effort backend shutdown so spotifyd can stop cleanly.
        if let Some(handle) = self.player_actor.lock().await.take() {
            let (resp, rx) = oneshot::channel();
            if self
                .player_tx
                .send(PlayerCommand::Shutdown { resp })
                .await
                .is_ok()
            {
                let _ = tokio::time::timeout(Duration::from_secs(2), rx).await;
            }
            if let Err(err) = tokio::time::timeout(Duration::from_secs(2), handle).await {
                tracing::warn!(error = %err, "player actor shutdown timed out");
            }
        }
        // Abort the forwarder task; dropping the player's sender will
        // close the stream and the task exits naturally too.
        if let Some(handle) = self.player_worker.lock().await.take() {
            handle.abort();
            let _ = handle.await;
        }
    }

    pub(crate) fn status(&self) -> DaemonStatus {
        let socket_path = Self::socket_path();
        DaemonStatus {
            running: true,
            socket_exists: socket_path.exists(),
            socket_reachable: true,
            stale_socket: false,
            socket_path: socket_path.display().to_string(),
            daemon_pid: Some(std::process::id()),
            uptime_secs: Some(self.started_at.elapsed().as_secs()),
            protocol_version: IPC_PROTOCOL_VERSION,
            daemon_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            daemon_build_id: Some(crate::server::current_build_id()),
        }
    }

    async fn shared_spotify_rate_limiter(&self) -> Result<RateLimitedClient> {
        let mut cached = self.spotify_rate_limiter.lock().await;
        if let Some(rate_limiter) = cached.as_ref() {
            return Ok(rate_limiter.clone());
        }
        let rate_limiter =
            SpotifyClient::default_rate_limiter().context("failed to build Spotify runtime")?;
        *cached = Some(rate_limiter.clone());
        Ok(rate_limiter)
    }

    pub(crate) async fn spotify_client(&self) -> Result<SpotifyClient> {
        if std::env::var_os("SPOTUIFY_FAKE_SPOTIFY").is_some() {
            let client =
                SpotifyClient::fake_with_rate_limiter(self.shared_spotify_rate_limiter().await?);
            return match AnalyticsStore::open_default().await {
                Ok(store) => Ok(client.with_analytics(Arc::new(store), AnalyticsSource::Daemon)),
                Err(err) => {
                    tracing::warn!(error = %err, "analytics store unavailable");
                    Ok(client)
                }
            };
        }
        let config = Config::load().context("failed to load Spotify config")?;
        // Only claim our own device for selection when the librespot
        // session is actually connected. Otherwise Spotify still
        // lists our `spotuify` device by SHA-1 id from a prior daemon
        // run, `preferred_device` step 0 picks it, transfer "succeeds"
        // against a phantom session, and `PUT /me/player/play`
        // returns `404 Not found.` because no actual device is on the
        // other end of the registry entry. Falling back to `None`
        // makes selection fall through to the next-best device
        // (active → name match → first available).
        let own_device_id = if self.player_is_connected().await {
            self.own_device_id()
        } else {
            None
        };
        let client =
            SpotifyClient::new_with_rate_limiter(config, self.shared_spotify_rate_limiter().await?)
                .with_token_cache(self.token_cache.clone())
                .with_schema_compat_reporter(Arc::new(DaemonSchemaCompatReporter {
                    event_tx: self.event_tx.clone(),
                    event_log: self.event_log.clone(),
                }))
                .with_own_device_id(own_device_id);
        match client.access_token().await {
            Ok(token) => {
                self.update_player_token(Some(token));
                // Self-healing: clear the latch if a previously revoked
                // token has been replaced (e.g. user ran `spotuify login`
                // in another shell). The TUI/CLI auto-reauth flow also
                // calls `Request::ReloadAuth` explicitly, but this catches
                // out-of-band recoveries too.
                self.auth_revoked
                    .store(false, std::sync::atomic::Ordering::Release);
            }
            Err(err) => {
                // Detect refresh-token-revoked: emit a sticky AuthError
                // event the first time we see it so the TUI shows a
                // re-login banner instead of letting downstream playback
                // commands no-op silently.
                if matches!(err, spotuify_spotify::SpotifyError::AuthRevoked)
                    && !self
                        .auth_revoked
                        .swap(true, std::sync::atomic::Ordering::AcqRel)
                {
                    // Log the full error chain on the FIRST latch flip
                    // so future "why is the re-auth modal showing?"
                    // diagnoses don't require re-running the daemon in
                    // verbose mode. The chain typically reads:
                    //   AuthRevoked -> "Spotify refresh token revoked"
                    //   plus the response-body snippet logged in
                    //   `spotuify_spotify::auth::refresh_token`.
                    tracing::warn!(
                        error = %err,
                        error_chain = ?err,
                        "Spotify refresh token revoked — emitting AuthError(InvalidGrant); re-login required"
                    );
                    let _ = self.event_tx.send(IpcMessage {
                        id: 0,
                        source: None,
                        payload: IpcPayload::Event(DaemonEvent::AuthError {
                            kind: spotuify_protocol::AuthErrorKind::InvalidGrant,
                        }),
                    });
                }
                tracing::debug!(error = %err, "spotify access token unavailable for player bridge")
            }
        }
        // Scope-drift surface — reuses the token that's now in
        // `self.token_cache` (no extra keychain read). Fires at most
        // once per daemon process via the atomic latch.
        if !self
            .scope_reauth_emitted
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            let cached = self.token_cache.lock().await;
            if emit_scope_reauth_event_if_needed(cached.as_ref(), &self.event_tx) {
                tracing::info!(
                    "stored Spotify token is missing required scopes; emitted ScopeReauthRequired event"
                );
            }
        }
        match AnalyticsStore::open_default().await {
            Ok(store) => Ok(client.with_analytics(Arc::new(store), AnalyticsSource::Daemon)),
            Err(err) => {
                tracing::warn!(error = %err, "analytics store unavailable");
                Ok(client)
            }
        }
    }
}

/// SHA-1-hex of `name`. Mirrors
/// `spotuify_player::backends::embedded::derive_device_id` so the
/// daemon can predict the device_id librespot publishes without
/// taking a dep on the (feature-gated) embedded backend module.
/// Three lines duplicated; cheaper than the dep-graph plumbing.
fn derive_device_id_for_name(name: &str) -> String {
    use sha1::{Digest, Sha1};
    let digest = Sha1::digest(name.as_bytes());
    let mut out = String::with_capacity(40);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn spawn_player_actor(
    mut player: PlayerBox,
) -> (
    mpsc::Sender<PlayerCommand>,
    mpsc::Sender<PlayerWarmCommand>,
    JoinHandle<()>,
) {
    let (tx, mut rx) = mpsc::channel(32);
    let (warm_tx, mut warm_rx) = mpsc::channel(16);
    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                command = rx.recv() => {
                    let Some(command) = command else {
                        break;
                    };
                    match command {
                        PlayerCommand::RegisterDevice { name, resp } => {
                            let _ = resp.send(player_result(player.register_device(&name).await));
                        }
                        PlayerCommand::Reconnect { name, resp } => {
                            let result = match player.shutdown().await {
                                Ok(()) => player.register_device(&name).await,
                                Err(err) => Err(err),
                            };
                            let _ = resp.send(player_result(result));
                        }
                        PlayerCommand::IsConnected { resp } => {
                            let _ = resp.send(player.is_connected().await);
                        }
                        PlayerCommand::Kind { resp } => {
                            let _ = resp.send(player.kind());
                        }
                        PlayerCommand::MercuryGet { uri, resp } => {
                            let _ = resp.send(player_result(player.mercury_get(&uri).await));
                        }
                        PlayerCommand::QueueAdd { uri, resp } => {
                            let _ = resp.send(player.queue_add(&uri).await);
                        }
                        PlayerCommand::Transport { cmd, resp } => {
                            let result = match cmd {
                                TransportCmd::PlayUri { uri, position_ms } => {
                                    player.play_uri(&uri, position_ms).await
                                }
                                TransportCmd::Pause => player.pause().await,
                                TransportCmd::Resume => player.resume().await,
                                TransportCmd::Next => player.next().await,
                                TransportCmd::Previous => player.previous().await,
                                TransportCmd::Seek { position_ms } => player.seek(position_ms).await,
                                TransportCmd::Volume { percent } => player.volume(percent).await,
                                TransportCmd::Shuffle { on } => player.shuffle(on).await,
                                TransportCmd::Repeat { mode } => player.repeat(mode).await,
                            };
                            let _ = resp.send(result);
                        }
                        PlayerCommand::Shutdown { resp } => {
                            if let Err(err) = player.shutdown().await {
                                tracing::warn!(error = %err, "player backend shutdown failed");
                            }
                            let _ = resp.send(());
                            break;
                        }
                    }
                }
                warm = warm_rx.recv() => {
                    let Some(warm) = warm else {
                        if rx.is_closed() {
                            break;
                        }
                        continue;
                    };
                    match warm {
                        PlayerWarmCommand::PreloadUri { uri } => {
                            if player.kind() != BackendKind::Embedded {
                                tracing::trace!(uri, backend = player.kind().label(), "audio prewarm unsupported by backend");
                                continue;
                            }
                            match player.preload_uri(&uri).await {
                                Ok(()) => tracing::trace!(uri, "audio prewarm queued"),
                                Err(err) => tracing::debug!(error = %err, uri, "audio prewarm failed"),
                            }
                        }
                    }
                }
            }
        }
    });
    (tx, warm_tx, handle)
}

fn player_result<T>(result: PlayerResult<T>) -> Result<T> {
    result.map_err(|err| anyhow::anyhow!(err))
}

async fn apply_viz_config(viz_coordinator: &Arc<VizCoordinator>, config: &Config) {
    viz_coordinator.set_target_fps(config.viz.target_fps);
    viz_coordinator.set_analyzer_params(config.viz.smoothing, config.viz.noise_gate);
    viz_coordinator
        .set_source(spotuify_protocol::VizSourceKindData::parse(
            &config.viz.source,
        ))
        .await;
    viz_coordinator.set_enabled(config.viz.enabled).await;
}

// Build the player backend from config, with a safe fallback path
// for the first-run / missing-config case. Returns the box, its
// event stream, and the token slot the daemon shares with the
// backend's TokenProvider.
/// Phase 14 (P14-G) — assemble the SystemIntegration config from the
/// on-disk `config.toml`. Best-effort: missing sections degrade to
/// "disabled" sub-configs. The cover-cache uses platform defaults
/// regardless so MPRIS + notifications can always file-serve art.
fn build_system_config() -> spotuify_system::SystemConfig {
    let mut system = spotuify_system::SystemConfig::default();
    if let Ok(config) = Config::load() {
        system.cover_cache.ttl = Duration::from_secs(
            config
                .cache
                .cover_cache_ttl_days
                .saturating_mul(24 * 60 * 60),
        );
        system.cover_cache.max_bytes = config.cache.cover_cache_mb.saturating_mul(1024 * 1024);
        system.hooks = config
            .analytics
            .hook_command
            .clone()
            .or_else(|| config.player.event_hook.clone())
            .map(|hook_command| spotuify_system::HookConfig {
                hook_command,
                timeout_ms: config.analytics.hook_timeout_ms,
            });
        #[cfg(feature = "system-integrations")]
        {
            system.notifications = Some(spotuify_system::notifications::NotificationsConfig {
                enabled: config.notifications.enabled,
                summary: config.notifications.summary.clone(),
                body: config.notifications.body.clone(),
                on_track_change: config.notifications.on_track_change,
                on_pause: config.notifications.on_pause,
                on_resume: config.notifications.on_resume,
                on_skip: config.notifications.on_skip,
                on_error: config.notifications.on_error,
            });
        }
    }
    system
}

fn build_player_or_default(
    viz_analyzer: Option<spotuify_audio::SharedAnalyzer>,
) -> Result<PlayerBuildResult> {
    // Phase 0 — librespot-only. When `SPOTUIFY_FAKE_SPOTIFY` is set the
    // daemon picks the in-memory mock backend so integration tests
    // and headless CI smoke runs don't need a real librespot session.
    // Otherwise: try embedded and return any error to the caller so
    // the binary entry point can log it before exiting.
    let token_slot = Arc::new(RwLock::new(None::<String>));
    if std::env::var_os("SPOTUIFY_FAKE_SPOTIFY").is_some() {
        tracing::info!("SPOTUIFY_FAKE_SPOTIFY set; using MockPlayerBackend");
        let (backend, stream) = spotuify_player::backends::mock::MockPlayerBackend::new();
        return Ok((Box::new(backend), stream, token_slot));
    }
    let config = Config::load().context(
        "spotify config unavailable — run `spotuify config init` and `spotuify login` first",
    )?;
    let (backend, stream) = player_factory::build_player(&config, token_slot.clone(), viz_analyzer)
        .context(
            "embedded librespot backend failed to initialize — \
             rebuild with --features embedded-playback + an audio backend (e.g. rodio-backend) \
             if you used --no-default-features",
        )?;
    Ok((backend, stream, token_slot))
}

struct DaemonSchemaCompatReporter {
    event_tx: broadcast::Sender<IpcMessage>,
    event_log: Arc<tokio::sync::Mutex<spotuify_protocol::EventLog>>,
}

impl SchemaCompatReporter for DaemonSchemaCompatReporter {
    fn report_schema_compat(&self, endpoint: &str, missing_keys: &[String]) {
        let event = DaemonEvent::SchemaCompat {
            endpoint: endpoint.to_string(),
            missing_keys: missing_keys.to_vec(),
        };
        if let Ok(mut log) = self.event_log.try_lock() {
            if let Some(logged) =
                spotuify_protocol::LoggedEvent::from(&event, spotuify_core::now_ms())
            {
                log.push(logged);
            }
        }
        let _ = self.event_tx.send(IpcMessage {
            id: 0,
            source: None,
            payload: IpcPayload::Event(event),
        });
    }
}

async fn recover_pending_receipts(
    store: &Store,
    event_tx: &broadcast::Sender<IpcMessage>,
    finished_at_ms: i64,
) -> Result<usize> {
    let pending = store.list_pending_receipts().await?;
    let mut recovered = 0;
    for receipt in pending {
        let message = format!(
            "{} failed because the daemon stopped before Spotify confirmed it",
            receipt.action
        );
        let error = spotuify_protocol::ApiErrorSummary {
            kind: spotuify_protocol::IpcErrorKind::Internal,
            message: message.clone(),
            retry_after_secs: None,
        };
        store
            .finalize_receipt(
                receipt.receipt_id,
                spotuify_protocol::ReceiptStatus::Failed,
                &message,
                finished_at_ms,
                Some(&error),
            )
            .await?;
        let _ = event_tx.send(IpcMessage {
            id: 0,
            source: None,
            payload: IpcPayload::Event(DaemonEvent::MutationFinalized {
                receipt_id: receipt.receipt_id,
                status: spotuify_protocol::ReceiptStatus::Failed,
                message,
            }),
        });
        recovered += 1;
    }
    Ok(recovered)
}

/// Emit a one-shot `AuthError { kind: ScopeReauthRequired }` event
/// when the persisted Spotify token is missing scopes that the daemon
/// now requires (i.e. it was issued before the scope list grew).
///
/// Returns `true` when the event was emitted, `false` otherwise.
/// Logged-out users (`token == None`) and fully-scoped tokens both
/// return `false`: neither case warrants a banner.
fn emit_scope_reauth_event_if_needed(
    token: Option<&StoredToken>,
    event_tx: &broadcast::Sender<IpcMessage>,
) -> bool {
    if !spotuify_spotify::auth::token_needs_scope_reauth(token) {
        return false;
    }
    let _ = event_tx.send(IpcMessage {
        id: 0,
        source: None,
        payload: IpcPayload::Event(DaemonEvent::AuthError {
            kind: spotuify_protocol::AuthErrorKind::ScopeReauthRequired,
        }),
    });
    true
}

// Drain the player's PlayerEvent stream and translate each event to
// the wire-level DaemonEvent. Lives on its own task so the player
// can emit asynchronously without blocking commands.
async fn forward_player_events(
    mut stream: tokio_stream::wrappers::UnboundedReceiverStream<PlayerEvent>,
    event_tx: broadcast::Sender<IpcMessage>,
    session_tracker: Arc<crate::session_tracker::SessionTracker>,
    viz_coordinator: Arc<VizCoordinator>,
    playback_clock: Arc<crate::clock::PlaybackClock>,
    embedded_sink_on_ready: bool,
) {
    while let Some(event) = stream.next().await {
        // Phase 10 (F11): fan the raw event into the session tracker
        // BEFORE translating, so the tracker sees every transition
        // including ones we don't surface as DaemonEvents (PositionTick,
        // PreloadNext, etc.).
        session_tracker.observe(&event).await;
        // Phase 8 — feed the playback clock. PlayerEvent is the
        // highest-trust source: ~sub-100ms after the audio actually
        // changed state. Web API polls become reconciliation only.
        playback_clock.apply_player_event(&event, spotuify_core::now_ms());
        match &event {
            PlayerEvent::Ready { .. } if embedded_sink_on_ready => {
                viz_coordinator.set_sink_available(true).await;
            }
            PlayerEvent::PlaybackStarted { .. }
            | PlayerEvent::PlaybackResumed
            | PlayerEvent::TrackChanged { .. } => viz_coordinator.set_playing(true),
            PlayerEvent::PlaybackPaused
            | PlayerEvent::EndOfTrack { .. }
            | PlayerEvent::SessionDisconnected { .. }
            | PlayerEvent::Failed { .. } => viz_coordinator.set_playing(false),
            _ => {}
        }
        // Phase 8 — for events that translate to a `PlaybackChanged`,
        // embed the freshly-updated clock snapshot so subscribers get
        // local-event truth in one IPC.
        let snapshot_for_push = matches!(
            &event,
            PlayerEvent::PlaybackStarted { .. }
                | PlayerEvent::PlaybackPaused
                | PlayerEvent::PlaybackResumed
                | PlayerEvent::TrackChanged { .. }
                | PlayerEvent::EndOfTrack { .. }
        )
        .then(|| playback_clock.snapshot());
        let daemon_event = translate_player_event_with_snapshot(event, snapshot_for_push);
        let Some(daemon_event) = daemon_event else {
            continue;
        };
        let _ = event_tx.send(IpcMessage {
            id: 0,
            source: None,
            payload: IpcPayload::Event(daemon_event),
        });
    }
}

fn translate_player_event_with_snapshot(
    event: PlayerEvent,
    snapshot: Option<spotuify_core::Playback>,
) -> Option<DaemonEvent> {
    let mut translated = translate_player_event(event)?;
    if let DaemonEvent::PlaybackChanged { playback, .. } = &mut translated {
        if playback.is_none() {
            *playback = snapshot;
        }
    }
    Some(translated)
}

fn translate_player_event(event: PlayerEvent) -> Option<DaemonEvent> {
    match event {
        PlayerEvent::Ready { device_id, name } => Some(DaemonEvent::PlayerReady {
            device_id: device_id.0,
            name,
        }),
        PlayerEvent::Degraded { reason } => Some(DaemonEvent::PlayerDegraded { reason }),
        PlayerEvent::PremiumRequired => Some(DaemonEvent::PremiumRequired),
        PlayerEvent::SessionDisconnected { reason } => {
            Some(DaemonEvent::SessionDisconnected { reason })
        }
        PlayerEvent::Failed { reason, restarts } => {
            Some(DaemonEvent::PlayerFailed { reason, restarts })
        }
        PlayerEvent::PlaybackStarted { uri, .. } => Some(DaemonEvent::PlaybackChanged {
            action: format!("started {uri}"),
            playback: None,
        }),
        PlayerEvent::PlaybackPaused => Some(DaemonEvent::PlaybackChanged {
            action: "paused".to_string(),
            playback: None,
        }),
        PlayerEvent::PlaybackResumed => Some(DaemonEvent::PlaybackChanged {
            action: "resumed".to_string(),
            playback: None,
        }),
        PlayerEvent::TrackChanged { uri, .. } => Some(DaemonEvent::PlaybackChanged {
            action: format!("track changed {uri}"),
            playback: None,
        }),
        PlayerEvent::EndOfTrack { uri } => Some(DaemonEvent::PlaybackChanged {
            action: format!("ended {uri}"),
            playback: None,
        }),
        PlayerEvent::PositionTick { .. } | PlayerEvent::PreloadNext { .. } => None,
    }
}

// Phase 7 architectural cut: DaemonState satisfies the SyncContext
// trait so the sync engine could move into spotuify-sync without
// holding a reference to this concrete type. Today src/sync.rs still
// uses Arc<DaemonState> directly; this impl is the seam that makes
// the move mechanical when scheduled.
#[async_trait::async_trait]
impl spotuify_sync::SyncContext for DaemonState {
    fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown_tx.subscribe()
    }
    fn store(&self) -> &spotuify_store::Store {
        &self.store
    }
    fn emit_event(&self, event: spotuify_protocol::DaemonEvent) {
        DaemonState::emit_event(self, event);
    }
    fn sync_lock_for(
        &self,
        target: spotuify_protocol::SyncTargetData,
    ) -> Option<Arc<Mutex<()>>> {
        use spotuify_protocol::SyncTargetData;
        match target {
            // Slow scheduler + on-demand full refresh; both lanes block
            // each other but not the fast cadence.
            SyncTargetData::Playlists
            | SyncTargetData::Library
            | SyncTargetData::All => Some(self.slow_sync_lock.clone()),
            // Fast scheduler — Playback drives the TUI now-playing
            // widget and must stay responsive even while slow sync is
            // mid-flight.
            SyncTargetData::Playback
            | SyncTargetData::Queue
            | SyncTargetData::Devices
            | SyncTargetData::Recent => Some(self.fast_sync_lock.clone()),
        }
    }
    async fn spotify_client(&self) -> anyhow::Result<SpotifyClient> {
        Ok(DaemonState::spotify_client(self)
            .await?
            .with_default_priority(Priority::BackgroundSync))
    }
    fn observe_mutation_seq(&self) -> u64 {
        DaemonState::current_mutation_seq(self)
    }
    fn may_apply_playback_update(&self, captured_seq: u64) -> bool {
        DaemonState::may_apply_state_update(self, captured_seq)
    }
    fn background_runtime(&self) -> Option<RuntimeHandle> {
        Some(self.bg_runtime_handle())
    }
    async fn index_media_items(&self, items: &[MediaItem], saved: bool) -> anyhow::Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let entries = items
            .iter()
            .cloned()
            .map(|item| spotuify_store::IndexedMediaItem {
                item,
                liked: saved,
                saved,
                added_at_ms: Some(spotuify_store::now_ms()),
                source: "spotify".to_string(),
            })
            .collect();
        self.search
            .apply_batch(spotuify_search::SearchUpdateBatch {
                entries,
                removed_uris: Vec::new(),
            })
            .await
    }
    fn warm_queue(&self, queue: &spotuify_spotify::client::Queue) {
        DaemonState::warm_queue(self, queue);
    }
    fn apply_playback_poll(
        &self,
        playback: &spotuify_core::Playback,
        captured_seq: u64,
        state_seq: u64,
        sampled_at_ms: i64,
        provider_timestamp_ms: Option<i64>,
    ) -> bool {
        self.playback_clock.apply_web_api_poll(
            playback,
            captured_seq,
            state_seq,
            sampled_at_ms,
            provider_timestamp_ms,
        )
    }
    fn snapshot_playback(&self) -> spotuify_core::Playback {
        DaemonState::snapshot_playback(self)
    }
    async fn snapshot_queue(&self) -> spotuify_spotify::client::Queue {
        self.store
            .latest_queue(500)
            .await
            .ok()
            .flatten()
            .unwrap_or_default()
    }
    async fn snapshot_devices(&self) -> Vec<spotuify_core::Device> {
        self.store.list_devices().await.unwrap_or_default()
    }
    fn event_subscriber_count(&self) -> usize {
        self.event_tx.receiver_count()
    }
}

#[cfg(test)]
mod system_config_tests {
    use super::build_system_config;

    fn restore_env(key: &str, value: Option<std::ffi::OsString>) {
        if let Some(value) = value {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn system_config_includes_analytics_hook_command() {
        let _guard = crate::ENV_LOCK.blocking_lock();
        let temp = tempfile::TempDir::new().expect("tempdir");
        let config_path = temp.path().join("spotuify.toml");
        std::fs::write(
            &config_path,
            r#"
client_id = "client"
client_secret = "secret"

[analytics]
hook_command = "echo hook"
hook_timeout_ms = 1234
"#,
        )
        .expect("write config");

        let old_config = std::env::var_os("SPOTUIFY_CONFIG");
        std::env::set_var("SPOTUIFY_CONFIG", &config_path);

        let system = build_system_config();

        restore_env("SPOTUIFY_CONFIG", old_config);

        let hooks = system.hooks.expect("hook config should be enabled");
        assert_eq!(hooks.hook_command, "echo hook");
        assert_eq!(hooks.timeout_ms, 1234);
    }

    #[test]
    fn system_config_uses_player_event_hook_as_legacy_fallback() {
        let _guard = crate::ENV_LOCK.blocking_lock();
        let temp = tempfile::TempDir::new().expect("tempdir");
        let config_path = temp.path().join("spotuify.toml");
        std::fs::write(
            &config_path,
            r#"
client_id = "client"
client_secret = "secret"

[player]
event_hook = "legacy-hook"
"#,
        )
        .expect("write config");

        let old_config = std::env::var_os("SPOTUIFY_CONFIG");
        std::env::set_var("SPOTUIFY_CONFIG", &config_path);

        let system = build_system_config();

        restore_env("SPOTUIFY_CONFIG", old_config);

        let hooks = system.hooks.expect("legacy hook should be enabled");
        assert_eq!(hooks.hook_command, "legacy-hook");
        assert_eq!(hooks.timeout_ms, 5_000);
    }

    #[test]
    fn system_config_prefers_analytics_hook_over_legacy_player_event_hook() {
        let _guard = crate::ENV_LOCK.blocking_lock();
        let temp = tempfile::TempDir::new().expect("tempdir");
        let config_path = temp.path().join("spotuify.toml");
        std::fs::write(
            &config_path,
            r#"
client_id = "client"
client_secret = "secret"

[player]
event_hook = "legacy-hook"

[analytics]
hook_command = "analytics-hook"
"#,
        )
        .expect("write config");

        let old_config = std::env::var_os("SPOTUIFY_CONFIG");
        std::env::set_var("SPOTUIFY_CONFIG", &config_path);

        let system = build_system_config();

        restore_env("SPOTUIFY_CONFIG", old_config);

        let hooks = system.hooks.expect("analytics hook should be enabled");
        assert_eq!(hooks.hook_command, "analytics-hook");
    }

    #[cfg(feature = "system-integrations")]
    #[test]
    fn system_config_includes_notification_preferences() {
        let _guard = crate::ENV_LOCK.blocking_lock();
        let temp = tempfile::TempDir::new().expect("tempdir");
        let config_path = temp.path().join("spotuify.toml");
        std::fs::write(
            &config_path,
            r#"
client_id = "client"
client_secret = "secret"

[notifications]
enabled = true
summary = "{track}"
body = "{artist}"
on_pause = true
on_resume = true
on_error = false
"#,
        )
        .expect("write config");

        let old_config = std::env::var_os("SPOTUIFY_CONFIG");
        std::env::set_var("SPOTUIFY_CONFIG", &config_path);

        let system = build_system_config();

        restore_env("SPOTUIFY_CONFIG", old_config);

        let notifications = system
            .notifications
            .expect("notification config should be present");
        assert!(notifications.enabled);
        assert_eq!(notifications.summary, "{track}");
        assert_eq!(notifications.body, "{artist}");
        assert!(notifications.on_track_change);
        assert!(notifications.on_pause);
        assert!(notifications.on_resume);
        assert!(!notifications.on_skip);
        assert!(!notifications.on_error);
    }
}

#[cfg(test)]
mod receipt_recovery {
    use super::{recover_pending_receipts, DaemonSchemaCompatReporter};
    use spotuify_protocol::{
        DaemonEvent, IpcMessage, IpcPayload, Receipt, ReceiptId, ReceiptStatus,
    };
    use spotuify_spotify::client::SchemaCompatReporter;
    use spotuify_store::Store;
    use tokio::sync::broadcast;

    fn receipt(action: &str, status: ReceiptStatus) -> Receipt {
        Receipt {
            receipt_id: ReceiptId::new_v7(),
            action: action.to_string(),
            status,
            message: "queued".to_string(),
            started_at_ms: 10,
            finished_at_ms: None,
            error: None,
        }
    }

    #[tokio::test]
    async fn startup_recovery_fails_pending_receipts_and_emits_finalized_events() {
        let store = Store::in_memory().await.expect("in-memory store");
        let pending = receipt("playlist-add", ReceiptStatus::Pending);
        let confirmed = receipt("queue", ReceiptStatus::Pending);
        store
            .insert_pending_receipt(&pending, "{}")
            .await
            .expect("pending receipt insert");
        store
            .insert_pending_receipt(&confirmed, "{}")
            .await
            .expect("confirmed receipt insert");
        store
            .finalize_receipt(
                confirmed.receipt_id,
                ReceiptStatus::Confirmed,
                "ok",
                20,
                None,
            )
            .await
            .expect("confirmed receipt finalize");
        let (tx, mut rx) = broadcast::channel::<IpcMessage>(8);

        let recovered = recover_pending_receipts(&store, &tx, 30)
            .await
            .expect("receipt recovery");

        assert_eq!(recovered, 1);
        let got = store
            .get_receipt(pending.receipt_id)
            .await
            .expect("pending receipt should still exist");
        assert_eq!(got.status, ReceiptStatus::Failed);
        assert_eq!(got.finished_at_ms, Some(30));
        assert!(got
            .error
            .as_ref()
            .is_some_and(|err| err.message.contains("daemon stopped")));
        let still_confirmed = store
            .get_receipt(confirmed.receipt_id)
            .await
            .expect("confirmed receipt should still exist");
        assert_eq!(still_confirmed.status, ReceiptStatus::Confirmed);

        let event = rx.recv().await.expect("finalized event");
        assert!(matches!(
            event.payload,
            IpcPayload::Event(DaemonEvent::MutationFinalized {
                receipt_id,
                status: ReceiptStatus::Failed,
                ..
            }) if receipt_id == pending.receipt_id
        ));
    }

    #[tokio::test]
    async fn scope_reauth_event_fires_when_stored_token_is_missing_required_scope() {
        use spotuify_protocol::AuthErrorKind;
        use spotuify_spotify::auth::StoredToken;

        let (tx, mut rx) = broadcast::channel::<IpcMessage>(8);
        // Reproduces the user-reported drift: stored token issued
        // before `user-follow-read` / `user-follow-modify` were added.
        let stale_token = StoredToken {
            access_token: "access".to_string(),
            refresh_token: "refresh".to_string(),
            expires_at: 4_000_000_000,
            scope: "user-read-playback-state user-modify-playback-state \
                    user-read-private user-library-read user-library-modify \
                    playlist-read-private playlist-modify-private playlist-modify-public"
                .to_string(),
            token_type: "Bearer".to_string(),
        };

        let emitted = super::emit_scope_reauth_event_if_needed(Some(&stale_token), &tx);

        assert!(
            emitted,
            "missing-scope token should trigger the proactive re-auth banner event"
        );
        let event = rx.recv().await.expect("scope-reauth event");
        assert!(matches!(
            event.payload,
            IpcPayload::Event(DaemonEvent::AuthError {
                kind: AuthErrorKind::ScopeReauthRequired,
            })
        ));
    }

    #[tokio::test]
    async fn scope_reauth_event_silent_when_stored_token_already_carries_every_required_scope() {
        use spotuify_spotify::auth::StoredToken;

        let (tx, mut rx) = broadcast::channel::<IpcMessage>(8);
        let healthy_token = StoredToken {
            access_token: "access".to_string(),
            refresh_token: "refresh".to_string(),
            expires_at: 4_000_000_000,
            scope: "user-read-playback-state user-read-currently-playing \
                    user-read-recently-played user-read-playback-position \
                    user-modify-playback-state \
                    user-read-private playlist-read-private \
                    playlist-read-collaborative playlist-modify-private \
                    playlist-modify-public user-library-read user-library-modify \
                    user-follow-read user-follow-modify streaming app-remote-control"
                .to_string(),
            token_type: "Bearer".to_string(),
        };

        let emitted = super::emit_scope_reauth_event_if_needed(Some(&healthy_token), &tx);

        assert!(!emitted, "fully-scoped token should not trigger a banner");
        assert!(
            rx.try_recv().is_err(),
            "no AuthError event should be broadcast when scopes are healthy"
        );
    }

    #[tokio::test]
    async fn scope_reauth_event_silent_when_no_token_is_stored_yet() {
        let (tx, mut rx) = broadcast::channel::<IpcMessage>(8);

        let emitted = super::emit_scope_reauth_event_if_needed(None, &tx);

        assert!(!emitted, "logged-out users should not see a re-auth banner");
        assert!(
            rx.try_recv().is_err(),
            "no event should be broadcast when there is no stored token"
        );
    }

    #[tokio::test]
    async fn schema_compat_reporter_broadcasts_and_logs_event() {
        let (tx, mut rx) = broadcast::channel::<IpcMessage>(8);
        let event_log =
            std::sync::Arc::new(tokio::sync::Mutex::new(spotuify_protocol::EventLog::new(8)));
        let reporter = DaemonSchemaCompatReporter {
            event_tx: tx,
            event_log: event_log.clone(),
        };

        reporter.report_schema_compat("/me/playlists?limit=50", &["items.followers".into()]);

        let event = rx.recv().await.expect("schema compat event");
        assert!(matches!(
            event.payload,
            IpcPayload::Event(DaemonEvent::SchemaCompat {
                ref endpoint,
                ref missing_keys,
            }) if endpoint == "/me/playlists?limit=50"
                && missing_keys == &vec!["items.followers".to_string()]
        ));
        let snapshot = event_log.lock().await.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert!(matches!(
            snapshot[0].kind,
            spotuify_protocol::LoggedKind::SchemaCompat { .. }
        ));
    }
}

#[cfg(test)]
mod phase_9_1_translate {
    //! Phase 9.1 — PlayerEvent → DaemonEvent translation. Pure
    //! function, no daemon spin-up needed. Adversarial: assert each
    //! lifecycle event maps to exactly one DaemonEvent with all
    //! fields preserved, and every playback-progress event maps to
    //! None so the wire bus stays clean during 9.1 (Phase 9.3 wires
    //! a richer position event).

    use super::translate_player_event;
    use spotuify_player::{DeviceId, PlayerEvent};
    use spotuify_protocol::DaemonEvent;

    fn player_ready(event: DaemonEvent) -> Option<(String, String)> {
        match event {
            DaemonEvent::PlayerReady { device_id, name } => Some((device_id, name)),
            _ => None,
        }
    }

    fn player_failed(event: DaemonEvent) -> Option<(String, u32)> {
        match event {
            DaemonEvent::PlayerFailed { reason, restarts } => Some((reason, restarts)),
            _ => None,
        }
    }

    #[test]
    fn ready_translates_with_device_id_and_name() {
        let translated = translate_player_event(PlayerEvent::Ready {
            device_id: DeviceId::new("dev-7"),
            name: "studio".to_string(),
        })
        .expect("Ready must translate");
        let (device_id, name) = player_ready(translated).expect("expected PlayerReady");
        assert_eq!(device_id, "dev-7");
        assert_eq!(name, "studio");
    }

    #[test]
    fn degraded_translates_with_reason() {
        let translated = translate_player_event(PlayerEvent::Degraded {
            reason: "spirc-timeout".to_string(),
        })
        .expect("Degraded must translate");
        assert!(
            matches!(translated, DaemonEvent::PlayerDegraded { ref reason } if reason == "spirc-timeout"),
            "got {translated:?}"
        );
    }

    #[test]
    fn premium_required_translates_to_unit_event() {
        let translated = translate_player_event(PlayerEvent::PremiumRequired)
            .expect("PremiumRequired must translate");
        assert!(matches!(translated, DaemonEvent::PremiumRequired));
    }

    #[test]
    fn session_disconnected_translates_with_reason() {
        let translated = translate_player_event(PlayerEvent::SessionDisconnected {
            reason: "session-invalid".to_string(),
        })
        .expect("SessionDisconnected must translate");
        assert!(
            matches!(translated, DaemonEvent::SessionDisconnected { ref reason } if reason == "session-invalid"),
            "got {translated:?}"
        );
    }

    #[test]
    fn failed_translates_with_restart_count() {
        let translated = translate_player_event(PlayerEvent::Failed {
            reason: "sink-panic-budget".to_string(),
            restarts: 5,
        })
        .expect("Failed must translate");
        let (reason, restarts) = player_failed(translated).expect("expected PlayerFailed");
        assert_eq!(reason, "sink-panic-budget");
        assert_eq!(restarts, 5);
    }

    #[test]
    fn playback_events_translate_to_playback_changed() {
        let cases = [
            (
                PlayerEvent::PlaybackStarted {
                    uri: "spotify:track:abc".to_string(),
                    position_ms: 0,
                },
                "started spotify:track:abc",
            ),
            (PlayerEvent::PlaybackPaused, "paused"),
            (PlayerEvent::PlaybackResumed, "resumed"),
            (
                PlayerEvent::TrackChanged {
                    uri: "spotify:track:def".to_string(),
                    position_ms: 0,
                },
                "track changed spotify:track:def",
            ),
            (
                PlayerEvent::EndOfTrack {
                    uri: "spotify:track:ghi".to_string(),
                },
                "ended spotify:track:ghi",
            ),
        ];

        for (event, expected) in cases {
            let translated = translate_player_event(event).expect("playback event should emit");
            assert!(
                matches!(translated, DaemonEvent::PlaybackChanged { ref action, .. } if action == expected),
                "got {translated:?}"
            );
        }
    }

    #[test]
    fn high_frequency_playback_events_stay_local() {
        for event in [
            PlayerEvent::PositionTick {
                position_ms: 12_000,
            },
            PlayerEvent::PreloadNext {
                uri: "spotify:track:abc".to_string(),
            },
        ] {
            assert!(
                translate_player_event(event.clone()).is_none(),
                "{event:?} should not produce a broadcast event"
            );
        }
    }
}
