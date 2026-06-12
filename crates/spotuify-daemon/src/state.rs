use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures::StreamExt;
use parking_lot::RwLock;
use spotuify_core::{BackendKind, Device, Queue};
use spotuify_player::{DeviceId, PlayerBackend, PlayerEvent, PlayerResult, RepeatMode};
use tokio::runtime::{Builder as RuntimeBuilder, Handle as RuntimeHandle, Runtime};
use tokio::sync::{broadcast, mpsc, oneshot, watch, Mutex};
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
const PENDING_QUEUE_APPEND_TTL_MS: i64 = 5_000;

#[derive(Clone, Debug)]
struct PendingQueueAppend {
    item: MediaItem,
    required_occurrence: usize,
    added_at_ms: i64,
}

fn pending_queue_appends_for(
    live_uris: &std::collections::HashSet<String>,
    queued_items: &[MediaItem],
    added_at_ms: i64,
) -> Vec<PendingQueueAppend> {
    // Occurrence counts MUST be seeded from the same base the add's
    // dedup ran against (the LIVE queue), not the cached snapshot: a
    // URI present in the stale cache but absent live would otherwise
    // get required_occurrence=2 and the overlay would append a
    // phantom duplicate until the TTL expired.
    let mut counts: HashMap<String, usize> = HashMap::new();
    for uri in live_uris {
        counts.insert(uri.clone(), 1);
    }

    queued_items
        .iter()
        .map(|item| {
            let count = counts.entry(item.uri.clone()).or_default();
            *count += 1;
            PendingQueueAppend {
                item: item.clone(),
                required_occurrence: *count,
                added_at_ms,
            }
        })
        .collect()
}

fn merge_queue_pending_appends(
    mut queue: Queue,
    pending: &mut Vec<PendingQueueAppend>,
    now_ms: i64,
) -> (Queue, bool) {
    pending.retain(|entry| now_ms.saturating_sub(entry.added_at_ms) <= PENDING_QUEUE_APPEND_TTL_MS);
    if pending.is_empty() {
        return (queue, false);
    }

    let mut counts: HashMap<String, usize> = HashMap::new();
    for item in &queue.items {
        *counts.entry(item.uri.clone()).or_default() += 1;
    }

    let mut merged = false;
    for entry in pending.iter() {
        let count = counts.entry(entry.item.uri.clone()).or_default();
        if *count < entry.required_occurrence {
            queue.items.push(entry.item.clone());
            *count += 1;
            merged = true;
        }
    }
    if merged {
        queue.session_active = true;
        queue.as_of_ms = now_ms;
    }
    (queue, merged)
}

enum PlayerCommand {
    RegisterDevice {
        name: String,
        resp: oneshot::Sender<Result<DeviceId>>,
    },
    Reconnect {
        name: String,
        resp: oneshot::Sender<Result<DeviceId>>,
    },
    SetAudioOutput {
        device: Option<String>,
        resp: oneshot::Sender<()>,
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
    /// Mint the first-party Web API bearer from the backend's live
    /// librespot session (login5). `None` when no session is available
    /// yet. See `DaemonState::mint_web_api_token`. Currently unused in
    /// prod because `web_api_bearer()` mints inline rather than through
    /// the player actor; kept for the actor-backed path that returns
    /// when login5 needs to run on the player thread.
    #[allow(dead_code)]
    WebApiToken {
        resp: oneshot::Sender<Option<String>>,
    },
    /// Tear down the live librespot session (keeps the actor running) so
    /// it stops minting after logout. See `DaemonState::drop_player_session`.
    DropSession {
        resp: oneshot::Sender<()>,
    },
    QueueAdd {
        uri: String,
        resp: oneshot::Sender<PlayerResult<()>>,
    },
    Shutdown {
        resp: oneshot::Sender<()>,
    },
}

struct PlayerTransportCommand {
    cmd: TransportCmd,
    resp: oneshot::Sender<PlayerResult<()>>,
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

/// Health of the embedded player session, sampled by the periodic
/// health loop. A librespot session can go invalid silently (dropped
/// TCP, host sleep/wake) without emitting a `SessionDisconnected`
/// event, so the event-driven reconnect path never fires — this is the
/// "zombie session" the loop exists to catch.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PlayerHealth {
    /// `now_ms` of the last probe; 0 before the first probe runs.
    pub last_probe_ms: i64,
    /// Whether the last probe found a live session.
    pub connected: bool,
    /// Consecutive unhealthy probes; reset to 0 on a healthy probe.
    pub consecutive_failures: u32,
    /// `now_ms` of the last auto-reconnect the loop triggered.
    pub last_reconnect_ms: Option<i64>,
    /// True once we stopped auto-reconnecting after too many failures;
    /// cleared by a healthy probe. The next user transport re-registers
    /// the device via the event path regardless.
    pub gave_up: bool,
}

/// Stop auto-reconnecting after this many consecutive failed probes to
/// avoid a reconnect storm against a persistently unreachable Spotify.
/// At the 60s probe cadence this is ~5 minutes of retries.
pub(crate) const PLAYER_RECONNECT_GIVE_UP_AFTER: u32 = 5;

/// Pure decision: should the health loop trigger an auto-reconnect this
/// tick? Only when the session is down, the user still wants this device
/// active, no reconnect is already in flight, and we haven't hit the
/// give-up ceiling.
pub(crate) fn should_auto_reconnect_player(
    connected: bool,
    we_are_active: bool,
    reconnect_in_flight: bool,
    consecutive_failures: u32,
) -> bool {
    !connected
        && we_are_active
        && !reconnect_in_flight
        && consecutive_failures < PLAYER_RECONNECT_GIVE_UP_AFTER
}

#[derive(Debug)]
pub(crate) enum FastTransportStatus {
    /// The player actor acked within the fast deadline.
    Applied,
    /// The deadline elapsed before the ack. The player actor will still
    /// reply on `ack`; the caller watches it so a late failure
    /// reconciles instead of vanishing after we told clients it applied.
    Dispatched {
        ack: oneshot::Receiver<PlayerResult<()>>,
    },
}

enum PlayerWarmCommand {
    PreloadUri { uri: String },
}

/// A cached cross-show episode feed: the merged episodes plus the epoch-ms
/// timestamp of the fetch (sort + limit are applied per request).
type CachedEpisodeFeed = (Vec<spotuify_core::MediaItem>, i64);

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
    /// Latch for the simpler unauthenticated state: no token is stored.
    /// This is not a transient network failure, so hot background loops
    /// should fail fast until login/reload or the auth-health probe sees
    /// new credentials on disk.
    auth_required: std::sync::atomic::AtomicBool,
    /// Dedupe Spotify schema-compat events/log taps by endpoint + key set.
    schema_compat_seen: Arc<parking_lot::Mutex<HashSet<String>>>,
    /// Device name we last registered the embedded librespot session
    /// under. Set the first time `ensure_player_ready(name)` is called.
    /// Used by `own_device_id()` to derive the deterministic SHA-1
    /// device_id we publish to Spotify — selection code prefers an
    /// entry matching this ID so stale namesakes in
    /// `/v1/me/player/devices` are harmless. The caller should pair
    /// this with `player_is_connected()` before trusting the registry
    /// entry as live.
    own_device_name: Arc<parking_lot::Mutex<Option<String>>>,
    /// Last volume (0..=100) reported by the embedded device's librespot
    /// Spirc via `PlayerEvent::VolumeChanged`. The Web API reports our
    /// own device's volume as `null`, so this daemon-owned value is the
    /// source of truth for `connected_own_device`'s `volume_percent` and
    /// for the now-playing volume display. `None` until the device is
    /// first activated. Shared with the player-event forwarder task.
    own_device_volume: Arc<parking_lot::Mutex<Option<u8>>>,
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
    player_transport_tx: mpsc::Sender<PlayerTransportCommand>,
    player_warm_tx: mpsc::Sender<PlayerWarmCommand>,
    player_token_slot: PlayerTokenSlot,
    /// Cross-request cache of the first-party Web API bearer. Keeps the
    /// per-request bearer fetch from round-tripping the (sequential)
    /// player actor on every call — only re-mints when the cached token
    /// is past its short TTL or a 401 forces a refresh. Shared into each
    /// `FirstPartyBearerProvider`.
    #[cfg_attr(not(feature = "embedded-playback"), allow(dead_code))]
    first_party_bearer: Arc<parking_lot::Mutex<Option<(String, Instant)>>>,
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
    pending_queue_appends: Arc<parking_lot::Mutex<Vec<PendingQueueAppend>>>,
    /// Whether the user currently intends THIS device to be the playback
    /// target. Set true when our embedded device starts/resumes/changes a
    /// track, cleared when a poll shows another device became active. Gates
    /// `schedule_player_reconnect` so that after the user hands off to another
    /// device (e.g. their phone), a transient session drop doesn't auto-
    /// reconnect and let librespot steal playback back. The device still
    /// re-registers lazily on the next user transport targeting it.
    we_are_active: Arc<AtomicBool>,
    /// Guards in-flight auto-reconnects. Shared with the player-event
    /// worker so the event-driven path and the periodic health loop
    /// never reconnect twice at once.
    reconnect_in_flight: Arc<AtomicBool>,
    /// Latest player-session health sample (see `PlayerHealth`).
    player_health: Arc<parking_lot::Mutex<PlayerHealth>>,
    /// Update-awareness — the latest GitHub release observed by the periodic
    /// check (see `crate::update`). `None` until the first check resolves.
    /// Read by `Request::CheckUpdate`; written by the update loop.
    latest_release: Arc<parking_lot::Mutex<Option<crate::update::CachedRelease>>>,
    /// Cached cross-show episode feed `(merged_episodes, fetched_at_ms)`. The
    /// raw merged set is cached; `Request::EpisodeFeed` applies sort + limit per
    /// call. `None` until first built.
    episode_feed: Arc<parking_lot::Mutex<Option<CachedEpisodeFeed>>>,
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
        // credential read. With the old Keychain-backed auth that triggered a
        // "spotuify wants to access the keychain" prompt at every cold
        // start, on top of the prompts the lazy `access_token_cached`
        // path already caused. Net effect: 3–5 prompts on every fresh
        // launch.
        //
        // Recovery: defer the scope-drift check to the first real API
        // call. `SpotifyClient::access_token_cached` already loads the
        // token once and caches it for the process; we hook the
        // scope-drift check off that single read (see
        // `emit_scope_reauth_event_if_needed` wiring in the request
        // handler). Net effect: the auth file is read exactly as many
        // times as a vanilla "fetch token, refresh when expiring" path
        // would read it.
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
        // Capture the sink-tap counter before the backend moves into the
        // actor; the session tracker reads it for sink-accurate audible time.
        let audio_counter = player_box.audio_counter();
        let (player_tx, player_transport_tx, player_warm_tx, player_actor) =
            spawn_player_actor(player_box);
        let (queue_warm, queue_warm_rx) = QueueWarmScheduler::new();
        let system_config = build_system_config();
        let system_integration = Arc::new(spotuify_system::SystemIntegration::spawn(system_config));
        let event_log = Arc::new(tokio::sync::Mutex::new(spotuify_protocol::EventLog::new(
            128,
        )));

        // Phase 10 (P10.1): SessionTracker writes ListenFact rows to
        // the store and emits ListenQualified into the event broadcast
        // when the qualification rule fires.
        let session_tracker = Arc::new(crate::session_tracker::SessionTracker::with_store(
            Arc::new(store.clone()),
            event_tx.clone(),
            audio_counter,
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

        // Shared embedded-device identity/volume cells: the forwarder task
        // writes the volume from VolumeChanged events; DaemonState reads
        // both for `connected_own_device`. Created here so the same Arcs
        // land in the struct literal below.
        let own_device_name = Arc::new(parking_lot::Mutex::new(None));
        let own_device_volume = Arc::new(parking_lot::Mutex::new(None));

        let event_tx_for_worker = event_tx.clone();
        let tracker_for_worker = session_tracker.clone();
        let viz_for_worker = viz_coordinator.clone();
        let clock_for_worker = playback_clock.clone();
        let store_for_worker = store.clone();
        let event_log_for_worker = event_log.clone();
        let system_for_worker = system_integration.clone();
        let player_tx_for_worker = player_tx.clone();
        let own_device_name_for_worker = own_device_name.clone();
        let own_device_volume_for_worker = own_device_volume.clone();
        let reconnect_in_flight = Arc::new(AtomicBool::new(false));
        let reconnect_in_flight_for_worker = reconnect_in_flight.clone();
        let we_are_active = Arc::new(AtomicBool::new(false));
        let we_are_active_for_worker = we_are_active.clone();
        let player_worker = tokio::spawn(async move {
            forward_player_events(
                player_stream,
                PlayerEventForwarder {
                    event_tx: event_tx_for_worker,
                    event_log: event_log_for_worker,
                    system_integration: system_for_worker,
                    session_tracker: tracker_for_worker,
                    viz_coordinator: viz_for_worker,
                    playback_clock: clock_for_worker,
                    store: store_for_worker,
                    player_tx: player_tx_for_worker,
                    own_device_name: own_device_name_for_worker,
                    own_device_volume: own_device_volume_for_worker,
                    reconnect_in_flight: reconnect_in_flight_for_worker,
                    we_are_active: we_are_active_for_worker,
                    embedded_sink_on_ready,
                },
            )
            .await;
        });

        // Phase 14 (P14-G) — system-integration actor. Reads config
        // for opt-in subsystems; if the config can't be loaded
        // (first-run / missing client_id) we still build the cover
        // cache and a no-op hook dispatcher so the daemon stays up.
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
            auth_required: std::sync::atomic::AtomicBool::new(false),
            schema_compat_seen: Arc::new(parking_lot::Mutex::new(HashSet::new())),
            own_device_name,
            own_device_volume,
            event_log,
            first_party_bearer: Arc::new(parking_lot::Mutex::new(None)),
            player_tx,
            player_transport_tx,
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
            pending_queue_appends: Arc::new(parking_lot::Mutex::new(Vec::new())),
            we_are_active,
            reconnect_in_flight,
            player_health: Arc::new(parking_lot::Mutex::new(PlayerHealth::default())),
            latest_release: Arc::new(parking_lot::Mutex::new(None)),
            episode_feed: Arc::new(parking_lot::Mutex::new(None)),
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

    pub(crate) fn track_pending_queue_appends(
        &self,
        live_uris: &std::collections::HashSet<String>,
        queued_items: &[MediaItem],
        added_at_ms: i64,
    ) {
        if queued_items.is_empty() {
            return;
        }
        self.pending_queue_appends
            .lock()
            .extend(pending_queue_appends_for(
                live_uris,
                queued_items,
                added_at_ms,
            ));
    }

    pub(crate) fn overlay_pending_queue_appends(&self, queue: Queue, now_ms: i64) -> Queue {
        let (queue, _) =
            merge_queue_pending_appends(queue, &mut self.pending_queue_appends.lock(), now_ms);
        queue
    }

    pub(crate) async fn mutation_lane(&self, request: &Request) -> Option<Arc<Mutex<()>>> {
        match request {
            Request::PlaybackCommand { .. }
            | Request::DeviceTransfer { .. }
            | Request::QueueAdd { .. } => Some(self.transport_mutation_lock.clone()),
            Request::PlaylistAddItems { playlist, .. }
            | Request::PlaylistRemoveItems { playlist, .. }
            | Request::PlaylistTracks { playlist, .. } => Some(self.playlist_lane(playlist).await),
            Request::PlaylistCreate { .. } => Some(self.playlist_lane("__playlist_create__").await),
            Request::LibrarySave { .. } | Request::LibraryUnsave { .. } => {
                Some(self.library_mutation_lock.clone())
            }
            Request::OpsUndo { .. } | Request::OpsRedo { .. } => {
                Some(self.operation_mutation_lock.clone())
            }
            _ => None,
        }
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
        let device_id = rx
            .await
            .map_err(|err| anyhow::anyhow!("player actor stopped: {err}"))??;
        let kind = self.player_kind().await;
        self.viz_coordinator.set_backend_kind(kind);
        if kind == BackendKind::Embedded {
            self.viz_coordinator.set_sink_available(true).await;
        }
        Ok(device_id)
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

    /// Explicitly set whether the user intends this device to be the playback
    /// target. Used by the transfer handler to flip immediately on a user-driven
    /// hand-off (the poll-based [`Self::note_active_device`] otherwise lags by a
    /// poll interval).
    pub(crate) fn set_we_are_active(&self, active: bool) {
        self.we_are_active.store(active, Ordering::Release);
    }

    /// Whether the user currently intends this device to be the playback target.
    pub(crate) fn is_we_are_active(&self) -> bool {
        self.we_are_active.load(Ordering::Acquire)
    }

    /// The latest GitHub release observed by the update loop, if any.
    pub(crate) fn cached_release(&self) -> Option<crate::update::CachedRelease> {
        self.latest_release.lock().clone()
    }

    /// Record the latest observed release (called by the update check).
    pub(crate) fn set_cached_release(&self, release: crate::update::CachedRelease) {
        *self.latest_release.lock() = Some(release);
    }

    /// The cached merged episode feed `(episodes, fetched_at_ms)`, if built.
    pub(crate) fn cached_episode_feed(&self) -> Option<CachedEpisodeFeed> {
        self.episode_feed.lock().clone()
    }

    /// Cache the merged episode feed with its fetch timestamp.
    pub(crate) fn set_cached_episode_feed(
        &self,
        episodes: Vec<spotuify_core::MediaItem>,
        fetched_at_ms: i64,
    ) {
        *self.episode_feed.lock() = Some((episodes, fetched_at_ms));
    }

    /// Reconcile `we_are_active` against an authoritative playback snapshot: set
    /// when our own device is the active one, clear when a *different* device is
    /// active (the user handed off — e.g. to their phone). Leaves the flag
    /// unchanged when no device is active, to avoid flapping during silence.
    pub(crate) fn note_active_device(&self, playback: &spotuify_core::Playback) {
        let Some(active_id) = playback.device.as_ref().and_then(|d| d.id.as_deref()) else {
            return;
        };
        match self.own_device_id() {
            Some(own) if own == active_id => self.we_are_active.store(true, Ordering::Release),
            _ => self.we_are_active.store(false, Ordering::Release),
        }
    }

    pub(crate) async fn connected_own_device(&self) -> Option<Device> {
        if !self.player_is_connected().await {
            return None;
        }
        let name = self.own_device_name.lock().clone()?;
        Some(Device {
            id: Some(derive_device_id_for_name(&name)),
            name,
            kind: "Speaker".to_string(),
            is_active: false,
            is_restricted: false,
            // Web API reports our own device's volume as `null`; surface the
            // librespot-reported value the forwarder tracks instead. The read
            // can be one VolumeChanged event behind a concurrent update —
            // a single render tick of staleness, accepted.
            volume_percent: *self.own_device_volume.lock(),
            supports_volume: true,
        })
    }

    pub(crate) fn configured_device_name() -> String {
        match Config::load() {
            Ok(config) => config.player.effective_device_name(),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "failed to load config for player device name; using default"
                );
                spotuify_spotify::config::PlayerConfig::default().effective_device_name()
            }
        }
    }

    pub(crate) async fn reconnect_player(&self, name: &str) -> Result<DeviceId> {
        *self.own_device_name.lock() = Some(name.to_string());
        let (resp, rx) = oneshot::channel();
        self.player_tx
            .send(PlayerCommand::Reconnect {
                name: name.to_string(),
                resp,
            })
            .await
            .map_err(|err| anyhow::anyhow!("player actor stopped: {err}"))?;
        let device_id = rx
            .await
            .map_err(|err| anyhow::anyhow!("player actor stopped: {err}"))??;
        let kind = self.player_kind().await;
        self.viz_coordinator.set_backend_kind(kind);
        if kind == BackendKind::Embedded {
            self.viz_coordinator.set_sink_available(true).await;
        }
        Ok(device_id)
    }

    /// Update the player backend's local audio output selection. Takes
    /// effect on the next reconnect (the sink chain is rebuilt then), so
    /// callers pair this with `reconnect_player`.
    pub(crate) async fn set_player_audio_output(&self, device: Option<String>) -> Result<()> {
        let (resp, rx) = oneshot::channel();
        self.player_tx
            .send(PlayerCommand::SetAudioOutput { device, resp })
            .await
            .map_err(|err| anyhow::anyhow!("player actor stopped: {err}"))?;
        rx.await
            .map_err(|err| anyhow::anyhow!("player actor stopped: {err}"))?;
        Ok(())
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

    /// One health-loop tick: probe the session, fold the result into the
    /// `PlayerHealth` snapshot, and auto-reconnect a zombie session when
    /// the user still wants this device active. Returns the snapshot for
    /// logging/tests.
    pub(crate) async fn probe_player_health(&self, now_ms: i64) -> PlayerHealth {
        let connected = self.player_is_connected().await;
        let active = self.is_we_are_active();
        let in_flight = self.reconnect_in_flight.load(Ordering::Acquire);

        let (snapshot, reconnect) = {
            let mut health = self.player_health.lock();
            health.last_probe_ms = now_ms;
            health.connected = connected;
            if connected {
                health.consecutive_failures = 0;
                health.gave_up = false;
            } else {
                health.consecutive_failures = health.consecutive_failures.saturating_add(1);
            }
            let reconnect = should_auto_reconnect_player(
                connected,
                active,
                in_flight,
                // Decide against the count BEFORE this failure so the
                // first failed probe still attempts a reconnect.
                health.consecutive_failures.saturating_sub(1),
            );
            if reconnect {
                health.last_reconnect_ms = Some(now_ms);
            } else if !connected
                && active
                && health.consecutive_failures >= PLAYER_RECONNECT_GIVE_UP_AFTER
            {
                health.gave_up = true;
            }
            (*health, reconnect)
        };

        if reconnect {
            tracing::warn!(
                consecutive_failures = snapshot.consecutive_failures,
                "player session is down while active; auto-reconnecting"
            );
            schedule_player_reconnect(self.player_tx.clone(), self.reconnect_in_flight.clone());
        }
        snapshot
    }

    /// Current player-session health snapshot for diagnostics.
    pub(crate) fn player_health_snapshot(&self) -> PlayerHealth {
        *self.player_health.lock()
    }

    /// Record the playback context (playlist/album/artist URI) the next
    /// started track plays from, for playlist-level listen analytics.
    pub(crate) fn set_playback_context(&self, context_uri: Option<String>) {
        self._session_tracker.set_current_context(context_uri);
    }

    /// Backend kind for diagnostics output.
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
            .player_transport_tx
            .send(PlayerTransportCommand { cmd, resp })
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

    pub(crate) async fn transport_fast(
        &self,
        cmd: TransportCmd,
        timeout: Duration,
    ) -> PlayerResult<FastTransportStatus> {
        let (resp, mut rx) = oneshot::channel();
        if self
            .player_transport_tx
            .try_send(PlayerTransportCommand { cmd, resp })
            .is_err()
        {
            return Err(spotuify_player::PlayerError::Playback(
                "player transport queue unavailable".to_string(),
            ));
        }
        // Borrow `rx` so the timeout doesn't consume it: on the deadline
        // we hand the still-open receiver back to the caller to watch
        // for the late ack instead of dropping the result on the floor.
        match tokio::time::timeout(timeout, &mut rx).await {
            Ok(Ok(Ok(()))) => Ok(FastTransportStatus::Applied),
            Ok(Ok(Err(err))) => Err(err),
            Ok(Err(_)) => Err(spotuify_player::PlayerError::Playback(
                "player actor stopped".to_string(),
            )),
            Err(_) => Ok(FastTransportStatus::Dispatched { ack: rx }),
        }
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

    pub(crate) fn auth_required(&self) -> bool {
        self.auth_required
            .load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn auth_gate_error(&self) -> Option<spotuify_spotify::SpotifyError> {
        if self.auth_revoked() {
            Some(spotuify_spotify::SpotifyError::AuthRevoked)
        } else if self.auth_required() {
            Some(spotuify_spotify::SpotifyError::AuthRequired)
        } else {
            None
        }
    }

    /// Daemon-owned auth health probe. This keeps the shared access
    /// token fresh while no client is connected and lets the daemon
    /// recover when a new login replaces a previously-revoked refresh
    /// token out-of-band.
    pub(crate) async fn refresh_auth_health(&self) -> Result<()> {
        if std::env::var_os("SPOTUIFY_FAKE_SPOTIFY").is_some() {
            return Ok(());
        }

        if let Some(err) = self.auth_gate_error() {
            if spotuify_spotify::auth::stored_credential_disk_snapshot().is_none() {
                return Err(anyhow::Error::new(err));
            }
            self.clear_auth_gate_for_disk_recovery().await;
        }

        let config = Config::load().context("failed to load Spotify config")?;
        let first_party = first_party_mode(&config);
        let client =
            SpotifyClient::new_with_rate_limiter(config, self.shared_spotify_rate_limiter().await?)
                .with_token_cache(self.token_cache.clone());
        let client = self.attach_bearer(client, first_party);

        match client.access_token().await {
            Ok(token) => {
                if !first_party {
                    self.update_player_token(Some(token));
                }
                if self
                    .auth_required
                    .swap(false, std::sync::atomic::Ordering::AcqRel)
                {
                    tracing::info!(
                        "Spotify auth recovered after login; cleared auth-required latch"
                    );
                }
                if self
                    .auth_revoked
                    .swap(false, std::sync::atomic::Ordering::AcqRel)
                {
                    tracing::info!(
                        "Spotify auth recovered after token replacement; cleared revoked latch"
                    );
                }
                if !first_party
                    && !self
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
                Ok(())
            }
            Err(err) => {
                if matches!(err, spotuify_spotify::SpotifyError::AuthRevoked) {
                    self.mark_auth_revoked(&err).await;
                } else if matches!(err, spotuify_spotify::SpotifyError::AuthRequired) {
                    self.mark_auth_required().await;
                }
                Err(anyhow::Error::new(err))
            }
        }
    }

    pub(crate) async fn mark_auth_revoked(&self, err: &spotuify_spotify::SpotifyError) {
        let first = !self
            .auth_revoked
            .swap(true, std::sync::atomic::Ordering::AcqRel);
        self.auth_required
            .store(false, std::sync::atomic::Ordering::Release);
        {
            let mut cache = self.token_cache.lock().await;
            *cache = None;
        }
        self.update_player_token(None);

        if first {
            tracing::warn!(
                error = %err,
                error_chain = ?err,
                "Spotify refresh token revoked — emitting AuthError(InvalidGrant); re-login required"
            );
            self.emit_event(DaemonEvent::AuthError {
                kind: spotuify_protocol::AuthErrorKind::InvalidGrant,
            });
        }
    }

    pub(crate) async fn mark_auth_required(&self) {
        let first = !self
            .auth_required
            .swap(true, std::sync::atomic::Ordering::AcqRel);
        {
            let mut cache = self.token_cache.lock().await;
            *cache = None;
        }
        self.update_player_token(None);

        if first {
            tracing::warn!("Spotify credentials missing — emitting AuthError(NotLoggedIn)");
            self.emit_event(DaemonEvent::AuthError {
                kind: spotuify_protocol::AuthErrorKind::NotLoggedIn,
            });
        }
    }

    /// Drop the daemon's in-memory token cache and clear the
    /// `auth_revoked` latch so the next `spotify_client()` call
    /// re-reads fresh credentials from the auth file. Called by the
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
        self.update_player_token(None);
        // Drop the cached first-party bearer so a logout isn't papered
        // over by the short bearer-cache TTL.
        self.first_party_bearer.lock().take();
        self.auth_revoked
            .store(false, std::sync::atomic::Ordering::Release);
        self.auth_required
            .store(false, std::sync::atomic::Ordering::Release);
        // If all credentials are now gone (logout), tear down the live
        // librespot session so it can't keep minting login5 bearers from
        // its in-memory connection until the next daemon restart.
        if matches!(
            spotuify_spotify::auth::stored_credential_snapshot(),
            Ok(None)
        ) {
            self.drop_player_session().await;
        }
    }

    async fn clear_auth_gate_for_disk_recovery(&self) {
        {
            let mut cache = self.token_cache.lock().await;
            *cache = None;
        }
        self.update_player_token(None);
        self.first_party_bearer.lock().take();
        self.auth_revoked
            .store(false, std::sync::atomic::Ordering::Release);
        self.auth_required
            .store(false, std::sync::atomic::Ordering::Release);
    }

    /// Shut down the embedded librespot session (without stopping the
    /// player actor) so it stops minting from cached credentials. The
    /// next playback command re-registers the device from fresh creds.
    async fn drop_player_session(&self) {
        let (resp, rx) = oneshot::channel();
        if self
            .player_tx
            .send(PlayerCommand::DropSession { resp })
            .await
            .is_ok()
        {
            let _ = rx.await;
        }
    }

    pub(crate) fn emit_event(&self, event: DaemonEvent) {
        emit_daemon_event(
            &self.event_tx,
            &self.event_log,
            &self.system_integration,
            event,
        );
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
            Ok(mut tasks) => {
                tasks.retain(|task| !task.is_finished());
                tasks.push(handle);
            }
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
        self.bg_runtime.handle()
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

    /// Attach the first-party login5 bearer provider when running in
    /// keymaster mode. No-op in legacy (dev-app) mode or in builds
    /// without the embedded backend.
    fn attach_bearer(&self, client: SpotifyClient, first_party: bool) -> SpotifyClient {
        let _ = first_party;
        #[cfg(feature = "embedded-playback")]
        if first_party {
            return client.with_bearer_provider(Arc::new(FirstPartyBearerProvider {
                player_tx: self.player_tx.clone(),
                token_slot: self.player_token_slot.clone(),
                cache: self.first_party_bearer.clone(),
            }));
        }
        client
    }

    /// Mint a first-party Web API bearer for a CLI-direct client (doctor,
    /// onboarding's initial sync) over IPC — those processes have no
    /// librespot session, so only the daemon can mint in first-party
    /// mode. Returns `None` in legacy mode or when the daemon can't mint
    /// (not logged in / no session). `force` re-mints after a 401.
    pub(crate) async fn web_api_bearer(&self, force: bool) -> Option<String> {
        let _ = force;
        #[cfg(feature = "embedded-playback")]
        {
            use spotuify_spotify::WebApiBearerProvider;
            let provider = FirstPartyBearerProvider {
                player_tx: self.player_tx.clone(),
                token_slot: self.player_token_slot.clone(),
                cache: self.first_party_bearer.clone(),
            };
            provider.bearer(force).await.ok()
        }
        #[cfg(not(feature = "embedded-playback"))]
        {
            None
        }
    }

    pub(crate) async fn spotify_client(&self) -> Result<SpotifyClient> {
        if let Some(err) = self.auth_gate_error() {
            return Err(anyhow::Error::new(err));
        }
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
        // In opt-in first-party (keymaster) mode, the bearer is minted
        // via login5 through the attached provider, and librespot
        // bootstraps from its own cached native credentials, so we must
        // NOT clobber the player token slot with the (Web-API-only)
        // login5 bearer here. Default dev-app mode keeps the old
        // slot-publish + scope-drift behaviour.
        let first_party = first_party_mode(&config);
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
                    seen: self.schema_compat_seen.clone(),
                }))
                .with_own_device_id(own_device_id);
        let client = self.attach_bearer(client, first_party);
        match client.access_token().await {
            Ok(token) => {
                if !first_party {
                    self.update_player_token(Some(token));
                }
                // Self-healing: clear the latch if a previously revoked
                // token has been replaced (e.g. user ran `spotuify login`
                // in another shell). The TUI/CLI auto-reauth flow also
                // calls `Request::ReloadAuth` explicitly, but this catches
                // out-of-band recoveries too.
                self.auth_revoked
                    .store(false, std::sync::atomic::Ordering::Release);
                self.auth_required
                    .store(false, std::sync::atomic::Ordering::Release);
            }
            Err(err) => {
                if matches!(err, spotuify_spotify::SpotifyError::AuthRevoked) {
                    self.mark_auth_revoked(&err).await;
                    return Err(anyhow::Error::new(err));
                } else if matches!(err, spotuify_spotify::SpotifyError::AuthRequired) {
                    self.mark_auth_required().await;
                    return Err(anyhow::Error::new(err));
                }
                tracing::debug!(error = %err, "spotify access token unavailable for player bridge")
            }
        }
        // Scope-drift surface — legacy dev-app only. login5 tokens always
        // report empty scopes, so the drift check would fire a permanent
        // false "re-login" banner in first-party mode. Reuses the token
        // that's now in `self.token_cache` (no extra auth file read).
        if !first_party
            && !self
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

/// First-party Web API bearer provider (login5).
///
/// Attached to the `SpotifyClient` in `spotify_client()` /
/// `refresh_auth_health()` when running in keymaster mode. Mints the
/// bearer from the live librespot session; if no session is up yet it
/// refreshes the stored OAuth token, publishes it as session-bootstrap
/// material, and re-mints. The OAuth access token is itself a valid
/// full-scope bearer, so it's the final fallback when login5 can't run.
/// TTL on the cross-request bearer cache. Short enough that a revoked or
/// near-expiry token is re-fetched quickly (a 401 also forces a refresh),
/// long enough that a sync burst doesn't round-trip the player actor on
/// every call.
#[cfg(feature = "embedded-playback")]
const FIRST_PARTY_BEARER_TTL: Duration = Duration::from_secs(60);

#[cfg(feature = "embedded-playback")]
struct FirstPartyBearerProvider {
    player_tx: mpsc::Sender<PlayerCommand>,
    token_slot: PlayerTokenSlot,
    cache: Arc<parking_lot::Mutex<Option<(String, Instant)>>>,
}

#[cfg(feature = "embedded-playback")]
impl FirstPartyBearerProvider {
    async fn mint(&self) -> Option<String> {
        let (resp, rx) = oneshot::channel();
        if self
            .player_tx
            .send(PlayerCommand::WebApiToken { resp })
            .await
            .is_err()
        {
            return None;
        }
        rx.await.unwrap_or(None)
    }

    fn cached(&self) -> Option<String> {
        self.cache
            .lock()
            .as_ref()
            .filter(|(_, expires_at)| *expires_at > Instant::now())
            .map(|(token, _)| token.clone())
    }

    fn store(&self, token: &str) {
        *self.cache.lock() = Some((token.to_string(), Instant::now() + FIRST_PARTY_BEARER_TTL));
    }
}

#[cfg(feature = "embedded-playback")]
#[async_trait::async_trait]
impl spotuify_spotify::WebApiBearerProvider for FirstPartyBearerProvider {
    async fn bearer(&self, force_refresh: bool) -> spotuify_spotify::SpotifyResult<String> {
        use spotuify_spotify::SpotifyError;
        if force_refresh {
            // A 401 means the cached/login5 bearer is dead; drop it.
            *self.cache.lock() = None;
        } else {
            // Fast path: a still-valid cached bearer, no actor round-trip.
            if let Some(token) = self.cached() {
                return Ok(token);
            }
            // Mint from the live librespot session (login5). Bounded by
            // the actor + the login5 timeout so a hung mint can't block.
            if let Some(bearer) = self.mint().await {
                self.store(&bearer);
                return Ok(bearer);
            }
        }
        // No live session (or forced): refresh the OAuth token so
        // librespot can (re)connect, and use the fresh access token
        // directly — it's a valid full-scope bearer. Re-minting via
        // login5 here would hand back its internally-cached token, i.e.
        // the same one that just 401'd on a forced refresh.
        let creds = spotuify_spotify::auth::load_first_party_credentials()?
            .ok_or(SpotifyError::AuthRequired)?;
        let oauth =
            spotuify_player::backends::first_party_auth::refresh_oauth(&creds.refresh_token)
                .await
                .map_err(first_party_refresh_error)?;
        // PKCE refresh tokens rotate; persist the new one or the stored
        // credential goes stale and the next refresh fails.
        if !oauth.refresh_token.is_empty() && oauth.refresh_token != creds.refresh_token {
            let rotated =
                spotuify_player::backends::first_party_auth::credentials_from_oauth_token(&oauth);
            if let Err(err) = spotuify_spotify::auth::save_first_party_credentials(&rotated) {
                tracing::warn!(error = %err, "failed to persist rotated first-party refresh token");
            }
        }
        *self.token_slot.write() = Some(oauth.access_token.clone());
        self.store(&oauth.access_token);
        Ok(oauth.access_token)
    }
}

/// Map a first-party OAuth refresh failure to a typed `SpotifyError`. A
/// revoked / `invalid_grant` refresh token must surface as `AuthRevoked`
/// (not a generic client error) so the daemon sets the revoked latch and
/// emits the re-login banner, matching the legacy dev-app path.
#[cfg(feature = "embedded-playback")]
fn first_party_refresh_error(err: spotuify_player::PlayerError) -> spotuify_spotify::SpotifyError {
    let text = err.to_string();
    let lower = text.to_lowercase();
    if lower.contains("invalid_grant") || lower.contains("revoked") {
        spotuify_spotify::SpotifyError::AuthRevoked
    } else {
        spotuify_spotify::SpotifyError::from(anyhow::anyhow!(
            "first-party OAuth refresh failed: {text}"
        ))
    }
}

/// First-party (keymaster) mode is now opt-in via
/// `SPOTUIFY_USE_FIRST_PARTY=1` (see `Config::is_first_party`). Default
/// is the dev-app flow. The stored-credential snapshot is no longer
/// consulted — a leftover `first-party.json` from the rework era must
/// not override the opt-in.
fn first_party_mode(config: &Config) -> bool {
    cfg!(feature = "embedded-playback") && config.is_first_party()
}

fn spawn_player_actor(
    mut player: PlayerBox,
) -> (
    mpsc::Sender<PlayerCommand>,
    mpsc::Sender<PlayerTransportCommand>,
    mpsc::Sender<PlayerWarmCommand>,
    JoinHandle<()>,
) {
    let (tx, mut rx) = mpsc::channel(32);
    let (transport_tx, mut transport_rx) = mpsc::channel(32);
    let (warm_tx, mut warm_rx) = mpsc::channel(16);
    let handle = tokio::spawn(async move {
        let mut transport_open = true;
        let mut command_open = true;
        let mut warm_open = true;
        loop {
            if !transport_open && !command_open && !warm_open {
                break;
            }
            tokio::select! {
                biased;
                transport = transport_rx.recv(), if transport_open => {
                    let Some(transport) = transport else {
                        transport_open = false;
                        continue;
                    };
                    handle_transport_command(&mut player, transport).await;
                }
                command = rx.recv(), if command_open => {
                    let Some(command) = command else {
                        command_open = false;
                        continue;
                    };
                    match command {
                        PlayerCommand::RegisterDevice { name, resp } => {
                            let _ = resp.send(player_result(player.register_device(&name).await));
                        }
                        PlayerCommand::Reconnect { name, resp } => {
                            if let Err(err) = player.shutdown().await {
                                tracing::warn!(
                                    error = %err,
                                    "player shutdown during reconnect failed; attempting register anyway"
                                );
                            }
                            let result = player.register_device(&name).await;
                            let _ = resp.send(player_result(result));
                        }
                        PlayerCommand::SetAudioOutput { device, resp } => {
                            player.set_audio_output_device(device);
                            let _ = resp.send(());
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
                        PlayerCommand::WebApiToken { resp } => {
                            let _ = resp.send(player.web_api_token().await);
                        }
                        PlayerCommand::DropSession { resp } => {
                            if let Err(err) = player.shutdown().await {
                                tracing::debug!(error = %err, "player session drop on logout failed");
                            }
                            let _ = resp.send(());
                        }
                        PlayerCommand::QueueAdd { uri, resp } => {
                            let _ = resp.send(player.queue_add(&uri).await);
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
                warm = warm_rx.recv(), if warm_open => {
                    let Some(warm) = warm else {
                        warm_open = false;
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
    (tx, transport_tx, warm_tx, handle)
}

async fn handle_transport_command(player: &mut PlayerBox, command: PlayerTransportCommand) {
    let result = match command.cmd {
        TransportCmd::PlayUri { uri, position_ms } => player.play_uri(&uri, position_ms).await,
        TransportCmd::Pause => player.pause().await,
        TransportCmd::Resume => player.resume().await,
        TransportCmd::Next => player.next().await,
        TransportCmd::Previous => player.previous().await,
        TransportCmd::Seek { position_ms } => player.seek(position_ms).await,
        TransportCmd::Volume { percent } => player.volume(percent).await,
        TransportCmd::Shuffle { on } => player.shuffle(on).await,
        TransportCmd::Repeat { mode } => player.repeat(mode).await,
    };
    let _ = command.resp.send(result);
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
            system.discord = Some(spotuify_system::discord::DiscordConfig {
                enabled: config.discord.enabled,
                application_id: config.discord.application_id.clone().unwrap_or_default(),
            });
            // Media controls (MPRIS / macOS Now Playing / Windows SMTC) are on
            // by default. `SPOTUIFY_NO_MEDIA_CONTROLS=1` opts out entirely —
            // `enabled: false` disables it on every platform, and
            // `allow_hidden_window: false` also skips the Windows hidden-window
            // driver. souvlaki init failures degrade gracefully (logged, no
            // handle), so enabling it can't break playback.
            let media_controls_off = std::env::var("SPOTUIFY_NO_MEDIA_CONTROLS")
                .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
            system.media_controls = Some(spotuify_system::media_controls::MediaControlsConfig {
                enabled: !media_controls_off,
                allow_hidden_window: !media_controls_off,
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
    seen: Arc<parking_lot::Mutex<HashSet<String>>>,
}

impl SchemaCompatReporter for DaemonSchemaCompatReporter {
    fn report_schema_compat(&self, endpoint: &str, missing_keys: &[String]) {
        let mut normalized = missing_keys.to_vec();
        normalized.sort();
        normalized.dedup();
        let key = format!("{endpoint}\n{}", normalized.join("\n"));
        if !self.seen.lock().insert(key) {
            return;
        }
        let event = DaemonEvent::SchemaCompat {
            endpoint: endpoint.to_string(),
            missing_keys: normalized,
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
struct PlayerEventForwarder {
    event_tx: broadcast::Sender<IpcMessage>,
    event_log: Arc<tokio::sync::Mutex<spotuify_protocol::EventLog>>,
    system_integration: Arc<spotuify_system::SystemIntegration>,
    session_tracker: Arc<crate::session_tracker::SessionTracker>,
    viz_coordinator: Arc<VizCoordinator>,
    playback_clock: Arc<crate::clock::PlaybackClock>,
    store: Store,
    player_tx: mpsc::Sender<PlayerCommand>,
    own_device_name: Arc<parking_lot::Mutex<Option<String>>>,
    own_device_volume: Arc<parking_lot::Mutex<Option<u8>>>,
    reconnect_in_flight: Arc<AtomicBool>,
    we_are_active: Arc<AtomicBool>,
    embedded_sink_on_ready: bool,
}

async fn forward_player_events(
    mut stream: tokio_stream::wrappers::UnboundedReceiverStream<PlayerEvent>,
    ctx: PlayerEventForwarder,
) {
    while let Some(event) = stream.next().await {
        // Phase 10 (F11): fan the raw event into the session tracker
        // BEFORE translating, so the tracker sees every transition
        // including ones we don't surface as DaemonEvents (PositionTick,
        // PreloadNext, etc.).
        ctx.session_tracker.observe(&event).await;
        // Phase 8 — feed the playback clock. PlayerEvent is the
        // highest-trust source: ~sub-100ms after the audio actually
        // changed state. Web API polls become reconciliation only.
        ctx.playback_clock
            .apply_player_event(&event, spotuify_core::now_ms());
        if let Some(uri) = player_event_media_uri(&event).map(str::to_string) {
            if let Some(item) = lookup_player_event_media_item(&ctx.store, &uri).await {
                if ctx.playback_clock.enrich_current_item(&item) {
                    tracing::debug!(uri, "enriched playback clock item from local metadata");
                }
            }
        }
        match &event {
            PlayerEvent::Ready { .. } if ctx.embedded_sink_on_ready => {
                ctx.viz_coordinator.set_sink_available(true).await;
            }
            PlayerEvent::PlaybackStarted { .. }
            | PlayerEvent::PlaybackResumed
            | PlayerEvent::TrackChanged { .. } => ctx.viz_coordinator.set_playing(true),
            PlayerEvent::PlaybackPaused
            | PlayerEvent::EndOfTrack { .. }
            | PlayerEvent::SessionDisconnected { .. }
            | PlayerEvent::Failed { .. } => ctx.viz_coordinator.set_playing(false),
            PlayerEvent::VolumeChanged { percent } => {
                // The embedded device is the only source of its own volume
                // (Web API reports `null`). Record it for
                // `connected_own_device` and fold it into the clock so the
                // now-playing volume row is correct and rate-limit-proof.
                *ctx.own_device_volume.lock() = Some(*percent);
                let percent = *percent;
                let name = ctx.own_device_name.lock().clone();
                ctx.playback_clock.apply_device_volume(
                    percent,
                    || {
                        name.map(|name| Device {
                            id: Some(derive_device_id_for_name(&name)),
                            name,
                            kind: "Speaker".to_string(),
                            is_active: true,
                            is_restricted: false,
                            volume_percent: Some(percent),
                            supports_volume: true,
                        })
                    },
                    spotuify_core::now_ms(),
                );
            }
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
                | PlayerEvent::VolumeChanged { .. }
        )
        .then(|| ctx.playback_clock.snapshot());
        // Our embedded device is producing audio → the user intends this
        // device to be the active target. (Cleared by the Web API poll when a
        // different device becomes active — see `note_active_device`.)
        if matches!(
            &event,
            PlayerEvent::PlaybackStarted { .. }
                | PlayerEvent::PlaybackResumed
                | PlayerEvent::TrackChanged { .. }
        ) {
            ctx.we_are_active.store(true, Ordering::Release);
        }
        let should_reconnect = matches!(
            &event,
            PlayerEvent::SessionDisconnected { .. } | PlayerEvent::Failed { .. }
        );
        let daemon_event = translate_player_event_with_snapshot(event, snapshot_for_push);
        let Some(daemon_event) = daemon_event else {
            continue;
        };
        emit_daemon_event(
            &ctx.event_tx,
            &ctx.event_log,
            &ctx.system_integration,
            daemon_event,
        );
        // Only auto-reconnect when the user still wants this device active.
        // After a hand-off to another device, `we_are_active` is false, so a
        // session drop leaves us idle instead of re-registering and letting
        // librespot grab playback back. The next user transport re-registers.
        if should_reconnect && ctx.we_are_active.load(Ordering::Acquire) {
            schedule_player_reconnect(ctx.player_tx.clone(), ctx.reconnect_in_flight.clone());
        }
    }
}

fn player_event_media_uri(event: &PlayerEvent) -> Option<&str> {
    match event {
        PlayerEvent::PlaybackStarted { uri, .. } | PlayerEvent::TrackChanged { uri, .. } => {
            Some(uri.as_str())
        }
        _ => None,
    }
}

async fn lookup_player_event_media_item(store: &Store, uri: &str) -> Option<MediaItem> {
    if let Ok(Some(queue)) = store.latest_queue(500).await {
        if let Some(item) = queue.currently_playing {
            if is_known_media_item(&item, uri) {
                return Some(item);
            }
        }
        if let Some(item) = queue
            .items
            .into_iter()
            .find(|item| is_known_media_item(item, uri))
        {
            return Some(item);
        }
    }

    let uri = uri.to_string();
    store
        .media_items_by_uris(std::slice::from_ref(&uri))
        .await
        .ok()?
        .into_iter()
        .find(|item| is_known_media_item(item, &uri))
}

fn is_known_media_item(item: &MediaItem, uri: &str) -> bool {
    item.uri == uri && (!item.name.is_empty() || item.duration_ms > 0 || item.image_url.is_some())
}

fn emit_daemon_event(
    event_tx: &broadcast::Sender<IpcMessage>,
    event_log: &Arc<tokio::sync::Mutex<spotuify_protocol::EventLog>>,
    system_integration: &Arc<spotuify_system::SystemIntegration>,
    event: DaemonEvent,
) {
    let event = spotuify_protocol::sanitize_daemon_event(event);
    if let Ok(mut log) = event_log.try_lock() {
        if let Some(logged) =
            spotuify_protocol::LoggedEvent::from(&event, crate::analytics::now_ms())
        {
            log.push(logged);
        }
    }
    let system = system_integration.clone();
    let event_for_system = event.clone();
    tokio::spawn(async move {
        system.handle_event(&event_for_system).await;
    });
    let _ = event_tx.send(IpcMessage {
        id: 0,
        source: None,
        payload: IpcPayload::Event(event),
    });
}

fn schedule_player_reconnect(
    player_tx: mpsc::Sender<PlayerCommand>,
    reconnect_in_flight: Arc<AtomicBool>,
) {
    if reconnect_in_flight.swap(true, Ordering::AcqRel) {
        return;
    }
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let device_name = DaemonState::configured_device_name();
        let (resp, rx) = oneshot::channel();
        let sent = player_tx
            .send(PlayerCommand::Reconnect {
                name: device_name,
                resp,
            })
            .await;
        if sent.is_ok() {
            match tokio::time::timeout(Duration::from_secs(10), rx).await {
                Ok(Ok(Ok(_))) => tracing::info!("player auto-reconnect succeeded"),
                Ok(Ok(Err(err))) => tracing::warn!(error = %err, "player auto-reconnect failed"),
                Ok(Err(err)) => {
                    tracing::warn!(error = %err, "player auto-reconnect response dropped")
                }
                Err(_) => tracing::warn!("player auto-reconnect timed out"),
            }
        }
        reconnect_in_flight.store(false, Ordering::Release);
    });
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
        PlayerEvent::VolumeChanged { percent } => Some(DaemonEvent::PlaybackChanged {
            action: format!("volume {percent}"),
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
    fn sync_lock_for(&self, target: spotuify_protocol::SyncTargetData) -> Option<Arc<Mutex<()>>> {
        use spotuify_protocol::SyncTargetData;
        match target {
            // Slow scheduler + on-demand full refresh; both lanes block
            // each other but not the fast cadence.
            SyncTargetData::Playlists | SyncTargetData::Library | SyncTargetData::All => {
                Some(self.slow_sync_lock.clone())
            }
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
        // Track active-device hand-off so a session drop after the user moves to
        // another device doesn't trigger an auto-reconnect that steals playback.
        self.note_active_device(playback);
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
    fn embedded_is_active_playback(&self) -> bool {
        // Our embedded device is the live source iff the clock shows our own
        // device id. While paused, polling `/me/player` every 3s is still
        // redundant; slow reconciliation is enough to catch external handoff.
        let Some(own) = self.own_device_id() else {
            return false;
        };
        let playback = self.playback_clock.snapshot();
        playback
            .device
            .as_ref()
            .and_then(|device| device.id.as_deref())
            == Some(own.as_str())
    }
    async fn snapshot_queue(&self) -> spotuify_spotify::client::Queue {
        self.store
            .latest_queue(500)
            .await
            .ok()
            .flatten()
            .unwrap_or_default()
    }
    fn overlay_pending_queue_appends(
        &self,
        queue: spotuify_spotify::client::Queue,
        now_ms: i64,
    ) -> spotuify_spotify::client::Queue {
        DaemonState::overlay_pending_queue_appends(self, queue, now_ms)
    }
    async fn snapshot_devices(&self) -> Vec<spotuify_core::Device> {
        self.store.list_devices().await.unwrap_or_default()
    }
    fn event_subscriber_count(&self) -> usize {
        self.event_tx.receiver_count()
    }
}

#[cfg(test)]
mod queue_pending_tests {
    use super::{
        merge_queue_pending_appends, pending_queue_appends_for, PENDING_QUEUE_APPEND_TTL_MS,
    };
    use spotuify_core::{MediaItem, MediaKind, Queue};

    fn track(uri: &str, name: &str) -> MediaItem {
        MediaItem {
            id: uri.rsplit(':').next().map(str::to_string),
            uri: uri.to_string(),
            name: name.to_string(),
            subtitle: "Artist".to_string(),
            context: "Album".to_string(),
            duration_ms: 180_000,
            kind: MediaKind::Track,
            ..Default::default()
        }
    }

    fn queue(items: Vec<MediaItem>, as_of_ms: i64) -> Queue {
        Queue {
            currently_playing: None,
            items,
            session_active: true,
            as_of_ms,
        }
    }

    #[test]
    fn pending_queue_append_keeps_duplicate_visible_until_ttl() {
        let existing = track("spotify:track:a", "Existing");
        let queued = track("spotify:track:a", "Queued duplicate");
        let live: std::collections::HashSet<String> =
            std::iter::once(existing.uri.clone()).collect();
        let mut pending = pending_queue_appends_for(&live, std::slice::from_ref(&queued), 100);

        let (merged, changed) =
            merge_queue_pending_appends(queue(vec![existing.clone()], 2), &mut pending, 200);
        assert!(changed);
        assert_eq!(
            merged
                .items
                .iter()
                .map(|item| item.uri.as_str())
                .collect::<Vec<_>>(),
            vec!["spotify:track:a", "spotify:track:a"]
        );

        let (confirmed, changed) = merge_queue_pending_appends(
            queue(vec![existing.clone(), queued], 3),
            &mut pending,
            300,
        );
        assert!(!changed);
        assert_eq!(
            confirmed
                .items
                .iter()
                .map(|item| item.uri.as_str())
                .collect::<Vec<_>>(),
            vec!["spotify:track:a", "spotify:track:a"]
        );

        let (late_stale, changed) =
            merge_queue_pending_appends(queue(vec![existing], 4), &mut pending, 400);
        assert!(changed);
        assert_eq!(
            late_stale
                .items
                .iter()
                .map(|item| item.uri.as_str())
                .collect::<Vec<_>>(),
            vec!["spotify:track:a", "spotify:track:a"]
        );
    }

    #[test]
    fn pending_queue_append_expires_back_to_live_queue() {
        let existing = track("spotify:track:a", "Existing");
        let queued = track("spotify:track:a", "Queued duplicate");
        let live: std::collections::HashSet<String> =
            std::iter::once(existing.uri.clone()).collect();
        let mut pending = pending_queue_appends_for(&live, &[queued], 100);

        let (merged, changed) = merge_queue_pending_appends(
            queue(vec![existing], 2),
            &mut pending,
            101 + PENDING_QUEUE_APPEND_TTL_MS,
        );

        assert!(!changed);
        assert!(pending.is_empty());
        assert_eq!(
            merged
                .items
                .iter()
                .map(|item| item.uri.as_str())
                .collect::<Vec<_>>(),
            vec!["spotify:track:a"]
        );
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
    use std::collections::HashSet;
    use std::sync::Arc;

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
                    user-follow-read user-follow-modify ugc-image-upload \
                    streaming app-remote-control"
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
            seen: Arc::new(parking_lot::Mutex::new(HashSet::new())),
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

    #[tokio::test]
    async fn schema_compat_reporter_dedupes_same_endpoint_and_keys() {
        let (tx, mut rx) = broadcast::channel::<IpcMessage>(8);
        let event_log =
            std::sync::Arc::new(tokio::sync::Mutex::new(spotuify_protocol::EventLog::new(8)));
        let reporter = DaemonSchemaCompatReporter {
            event_tx: tx,
            event_log: event_log.clone(),
            seen: Arc::new(parking_lot::Mutex::new(HashSet::new())),
        };

        reporter.report_schema_compat(
            "/me/tracks?limit=50",
            &[
                "items.track.popularity".into(),
                "items.track.linked_from".into(),
            ],
        );
        reporter.report_schema_compat(
            "/me/tracks?limit=50",
            &[
                "items.track.linked_from".into(),
                "items.track.popularity".into(),
            ],
        );

        let _ = rx.recv().await.expect("first schema compat event");
        assert!(rx.try_recv().is_err());
        assert_eq!(event_log.lock().await.snapshot().len(), 1);
    }
}

#[cfg(test)]
mod auth_revocation_tests {
    use std::ffi::OsString;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use spotuify_protocol::{AuthErrorKind, DaemonEvent, IpcMessage, IpcPayload};
    use spotuify_spotify::auth::StoredToken;
    use spotuify_spotify::SpotifyError;
    use tempfile::TempDir;
    use tokio::sync::broadcast;

    use super::DaemonState;

    struct TestEnv {
        _temp: TempDir,
        old_values: Vec<(&'static str, Option<OsString>)>,
    }

    impl TestEnv {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("tempdir");
            let old_values = vec![
                (
                    "SPOTUIFY_FAKE_SPOTIFY",
                    std::env::var_os("SPOTUIFY_FAKE_SPOTIFY"),
                ),
                ("SPOTUIFY_CACHE_DB", std::env::var_os("SPOTUIFY_CACHE_DB")),
                (
                    "SPOTUIFY_SEARCH_INDEX",
                    std::env::var_os("SPOTUIFY_SEARCH_INDEX"),
                ),
                (
                    "SPOTUIFY_RUNTIME_DIR",
                    std::env::var_os("SPOTUIFY_RUNTIME_DIR"),
                ),
                ("SPOTUIFY_DATA_DIR", std::env::var_os("SPOTUIFY_DATA_DIR")),
            ];

            std::env::set_var("SPOTUIFY_FAKE_SPOTIFY", "1");
            std::env::set_var("SPOTUIFY_CACHE_DB", temp.path().join("cache.sqlite3"));
            std::env::set_var("SPOTUIFY_SEARCH_INDEX", temp.path().join("search-index"));
            std::env::set_var("SPOTUIFY_RUNTIME_DIR", temp.path().join("runtime"));
            std::env::set_var("SPOTUIFY_DATA_DIR", temp.path().join("data"));

            Self {
                _temp: temp,
                old_values,
            }
        }
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            for (key, value) in &self.old_values {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn stored_token() -> StoredToken {
        StoredToken {
            access_token: "access".to_string(),
            refresh_token: "refresh".to_string(),
            expires_at: 4_000_000_000,
            scope: "user-read-playback-state".to_string(),
            token_type: "Bearer".to_string(),
        }
    }

    async fn test_state() -> (TestEnv, DaemonState) {
        let env = TestEnv::new();
        let state = DaemonState::new().await.expect("daemon state");
        (env, state)
    }

    async fn shutdown_state(state: DaemonState) {
        state.request_shutdown();
        state.shutdown_player().await;
        state.shutdown_search().await;
        state
            .shutdown_background_tasks(Duration::from_millis(100))
            .await;
    }

    async fn recv_auth_error(rx: &mut broadcast::Receiver<IpcMessage>, expected: AuthErrorKind) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(!remaining.is_zero(), "timed out waiting for AuthError");
            let msg = tokio::time::timeout(remaining, rx.recv())
                .await
                .expect("auth event timeout")
                .expect("auth event");
            if matches!(
                msg.payload,
                IpcPayload::Event(DaemonEvent::AuthError {
                    kind
                }) if kind == expected
            ) {
                return;
            }
        }
    }

    fn drain_events(rx: &mut broadcast::Receiver<IpcMessage>) {
        while rx.try_recv().is_ok() {}
    }

    #[tokio::test]
    async fn mark_auth_revoked_clears_cache_slot_and_emits_once() {
        let _guard = crate::ENV_LOCK.lock().await;
        let (_env, state) = test_state().await;
        let mut rx = state.event_tx.subscribe();
        drain_events(&mut rx);

        *state.token_cache.lock().await = Some(stored_token());
        state.update_player_token(Some("stale-access".to_string()));

        state.mark_auth_revoked(&SpotifyError::AuthRevoked).await;

        assert!(state.auth_revoked());
        assert!(state.token_cache.lock().await.is_none());
        assert!(state.player_token_slot.read().is_none());
        recv_auth_error(&mut rx, AuthErrorKind::InvalidGrant).await;

        drain_events(&mut rx);
        state.mark_auth_revoked(&SpotifyError::AuthRevoked).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let saw_second_auth = std::iter::from_fn(|| rx.try_recv().ok()).any(|msg| {
            matches!(
                msg.payload,
                IpcPayload::Event(DaemonEvent::AuthError {
                    kind: AuthErrorKind::InvalidGrant
                })
            )
        });
        assert!(!saw_second_auth, "AuthError should be one-shot per latch");

        shutdown_state(state).await;
    }

    #[tokio::test]
    async fn mark_auth_required_clears_cache_slot_and_emits_once() {
        let _guard = crate::ENV_LOCK.lock().await;
        let (_env, state) = test_state().await;
        let mut rx = state.event_tx.subscribe();
        drain_events(&mut rx);

        *state.token_cache.lock().await = Some(stored_token());
        state.update_player_token(Some("stale-access".to_string()));

        state.mark_auth_required().await;

        assert!(state.auth_required());
        assert!(state.token_cache.lock().await.is_none());
        assert!(state.player_token_slot.read().is_none());
        recv_auth_error(&mut rx, AuthErrorKind::NotLoggedIn).await;

        drain_events(&mut rx);
        state.mark_auth_required().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let saw_second_auth = std::iter::from_fn(|| rx.try_recv().ok()).any(|msg| {
            matches!(
                msg.payload,
                IpcPayload::Event(DaemonEvent::AuthError {
                    kind: AuthErrorKind::NotLoggedIn
                })
            )
        });
        assert!(
            !saw_second_auth,
            "AuthRequired should be one-shot per latch"
        );

        shutdown_state(state).await;
    }

    #[tokio::test]
    async fn reload_auth_clears_cache_slot_and_latch() {
        let _guard = crate::ENV_LOCK.lock().await;
        let (_env, state) = test_state().await;

        *state.token_cache.lock().await = Some(stored_token());
        state.update_player_token(Some("stale-access".to_string()));
        state
            .auth_revoked
            .store(true, std::sync::atomic::Ordering::Release);
        state
            .auth_required
            .store(true, std::sync::atomic::Ordering::Release);

        state.reload_auth().await;

        assert!(!state.auth_revoked());
        assert!(!state.auth_required());
        assert!(state.token_cache.lock().await.is_none());
        assert!(state.player_token_slot.read().is_none());

        shutdown_state(state).await;
    }

    #[tokio::test]
    async fn spotify_client_fails_fast_while_auth_revoked() {
        let _guard = crate::ENV_LOCK.lock().await;
        let (_env, state) = test_state().await;
        state.auth_revoked.store(true, Ordering::Release);

        let result = state.spotify_client().await;
        assert!(result.is_err(), "latched auth revocation should fail fast");
        let err = result.err().expect("spotify client result should be error");

        assert!(matches!(
            err.downcast_ref::<SpotifyError>(),
            Some(SpotifyError::AuthRevoked)
        ));

        shutdown_state(state).await;
    }

    #[tokio::test]
    async fn spotify_client_fails_fast_while_auth_required() {
        let _guard = crate::ENV_LOCK.lock().await;
        let (_env, state) = test_state().await;
        state.auth_required.store(true, Ordering::Release);

        let result = state.spotify_client().await;
        assert!(result.is_err(), "latched missing auth should fail fast");
        let err = result.err().expect("spotify client result should be error");

        assert!(matches!(
            err.downcast_ref::<SpotifyError>(),
            Some(SpotifyError::AuthRequired)
        ));

        shutdown_state(state).await;
    }

    #[tokio::test]
    async fn timeout_borrows_receiver_so_late_ack_survives() {
        // Regression guard for `transport_fast`: it used to pass the
        // receiver *by value* into `tokio::time::timeout`, so on the
        // deadline the receiver was dropped and the player actor's late
        // ack (success or failure) vanished after we had already told
        // clients the command applied. Borrowing the receiver keeps it
        // open; the `Dispatched { ack }` variant then hands it to the
        // reconcile watcher. This locks the exact channel contract.
        use tokio::sync::oneshot;
        let (tx, mut rx) = oneshot::channel::<spotuify_player::PlayerResult<()>>();

        // First poll: the actor hasn't acked yet, so the deadline elapses
        // without consuming the receiver.
        let timed_out = tokio::time::timeout(Duration::from_millis(5), &mut rx)
            .await
            .is_err();
        assert!(timed_out, "deadline should elapse before the ack");

        // The actor replies late with a failure; the still-open receiver
        // delivers it instead of dropping it on the floor.
        tx.send(Err(spotuify_player::PlayerError::Playback(
            "spirc rejected".to_string(),
        )))
        .expect("receiver must still be open");
        let late = rx.await.expect("ack channel should stay open");
        assert!(late.is_err(), "late failure must be observable: {late:?}");
    }

    #[test]
    fn auto_reconnect_decision_covers_active_inflight_and_giveup() {
        use super::{should_auto_reconnect_player, PLAYER_RECONNECT_GIVE_UP_AFTER};

        // Down + active + idle + under the ceiling → reconnect.
        assert!(should_auto_reconnect_player(false, true, false, 0));
        // Healthy session → never reconnect.
        assert!(!should_auto_reconnect_player(true, true, false, 0));
        // Not the active device (handed off to a phone) → leave it alone.
        assert!(!should_auto_reconnect_player(false, false, false, 0));
        // A reconnect already in flight → don't stack another.
        assert!(!should_auto_reconnect_player(false, true, true, 0));
        // Hit the give-up ceiling → stop until a user transport re-registers.
        assert!(!should_auto_reconnect_player(
            false,
            true,
            false,
            PLAYER_RECONNECT_GIVE_UP_AFTER
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
