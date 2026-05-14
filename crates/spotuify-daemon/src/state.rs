use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use futures::StreamExt;
use parking_lot::RwLock;
use spotuify_core::BackendKind;
use spotuify_player::{DeviceId, PlayerBackend, PlayerEvent};
use tokio::sync::{broadcast, watch, Mutex};
use tokio::task::JoinHandle;

use crate::analytics::{AnalyticsSource, AnalyticsStore};
use crate::player_factory;
use spotuify_protocol::{DaemonEvent, DaemonStatus, IpcMessage, IpcPayload, IPC_PROTOCOL_VERSION};
use spotuify_search::{SearchIndex, SearchServiceHandle};
use spotuify_spotify::auth::StoredToken;
use spotuify_spotify::client::SpotifyClient;
use spotuify_spotify::config::Config;
use spotuify_store::Store;

pub(crate) struct DaemonState {
    started_at: Instant,
    shutdown_tx: watch::Sender<bool>,
    pub(crate) event_tx: broadcast::Sender<IpcMessage>,
    store: Store,
    search: SearchServiceHandle,
    search_worker: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    token_cache: Arc<Mutex<Option<StoredToken>>>,
    /// Phase 6.9 — recent-event ring buffer used by `doctor` to surface
    /// rate-limit / auth-error / schema-compat findings.
    event_log: tokio::sync::Mutex<spotuify_protocol::EventLog>,

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
    player: Arc<tokio::sync::Mutex<Box<dyn PlayerBackend>>>,
    player_token_slot: Arc<RwLock<Option<String>>>,
    player_worker: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    // Phase 10 (F11) — listening-session tracker observed by
    // forward_player_events. Foundation pass: state machine label only;
    // Pass 2 (P10.1) wires finalize → listen_facts insertion.
    pub(crate) session_tracker: Arc<crate::session_tracker::SessionTracker>,
}

impl DaemonState {
    pub(crate) async fn new() -> Result<Self> {
        let (shutdown_tx, _) = watch::channel(false);
        let (event_tx, _) = broadcast::channel(128);
        let store = Store::open_default().await?;
        let (search, search_worker) =
            SearchServiceHandle::start(SearchIndex::open(store.index_path())?);

        // Phase 9.1 — build the player backend from config. When the
        // config isn't loadable (first run, missing client_id), fall
        // back to the Connect backend so the daemon still starts; the
        // PremiumRequired / device handling kicks in lazily on the
        // first command. Existing spotifyd users see no behavioural
        // change because Spotifyd is the default backend.
        let (player_box, player_stream, token_slot) = build_player_or_default();
        // Phase 10 (P10.1): SessionTracker writes ListenFact rows to
        // the store and emits ListenQualified into the event broadcast
        // when the qualification rule fires.
        let session_tracker = Arc::new(crate::session_tracker::SessionTracker::with_store(
            Arc::new(store.clone()),
            event_tx.clone(),
        ));
        let event_tx_for_worker = event_tx.clone();
        let tracker_for_worker = session_tracker.clone();
        let player_worker = tokio::spawn(async move {
            forward_player_events(player_stream, event_tx_for_worker, tracker_for_worker).await;
        });

        Ok(Self {
            started_at: Instant::now(),
            shutdown_tx,
            event_tx,
            store,
            search,
            search_worker: tokio::sync::Mutex::new(Some(search_worker)),
            token_cache: Arc::new(Mutex::new(None)),
            event_log: tokio::sync::Mutex::new(spotuify_protocol::EventLog::new(128)),
            player: Arc::new(tokio::sync::Mutex::new(player_box)),
            player_token_slot: token_slot,
            player_worker: tokio::sync::Mutex::new(Some(player_worker)),
            session_tracker,
        })
    }

    /// Register the daemon's Connect device. Idempotent — calling
    /// twice with the same name is safe (backends short-circuit).
    /// Emits `DaemonEvent::PlayerReady` on success or `PlayerFailed`
    /// on terminal error (the event-forward task does the
    /// translation; we just propagate Result here).
    pub(crate) async fn ensure_player_ready(&self, name: &str) -> Result<DeviceId> {
        let mut player = self.player.lock().await;
        player
            .register_device(name)
            .await
            .map_err(|err| anyhow::anyhow!(err))
    }

    /// Snapshot the player's connection state. Backend-agnostic — the
    /// diagnostics module uses this so `doctor` doesn't need to know
    /// which backend is active.
    pub(crate) async fn player_is_connected(&self) -> bool {
        let player = self.player.lock().await;
        player.is_connected().await
    }

    /// Backend kind for diagnostics output.
    pub(crate) async fn player_kind(&self) -> BackendKind {
        let player = self.player.lock().await;
        player.kind()
    }

    /// Publish a Web API token into the slot every backend reads.
    /// Called by the token-refresh path (Phase 9.4 wires this for
    /// real; in 9.1 we set it once after first successful refresh).
    #[allow(dead_code)]
    pub(crate) fn update_player_token(&self, token: Option<String>) {
        *self.player_token_slot.write() = token;
    }

    pub(crate) fn runtime_dir() -> PathBuf {
        if let Some(path) = std::env::var_os("SPOTUIFY_RUNTIME_DIR") {
            return PathBuf::from(path);
        }

        dirs::cache_dir()
            .or_else(|| dirs::home_dir().map(|home| home.join(".cache")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("spotuify")
    }

    pub(crate) fn socket_path() -> PathBuf {
        if let Some(path) = std::env::var_os("SPOTUIFY_SOCKET") {
            return PathBuf::from(path);
        }
        Self::runtime_dir().join("daemon.sock")
    }

    pub(crate) fn pid_path() -> PathBuf {
        Self::runtime_dir().join("daemon.pid")
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

    pub(crate) fn request_shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
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
        let _ = self.event_tx.send(IpcMessage {
            id: 0,
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

    /// Gracefully shut down the player backend and abort its event
    /// forwarder. Called from the server's main shutdown path.
    pub(crate) async fn shutdown_player(&self) {
        // Best-effort backend shutdown so spotifyd can stop cleanly.
        {
            let mut player = self.player.lock().await;
            if let Err(err) = player.shutdown().await {
                tracing::warn!(error = %err, "player backend shutdown failed");
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

    pub(crate) async fn spotify_client(&self) -> Result<SpotifyClient> {
        if std::env::var_os("SPOTUIFY_FAKE_SPOTIFY").is_some() {
            let client = SpotifyClient::fake()?;
            return match AnalyticsStore::open_default().await {
                Ok(store) => Ok(client.with_analytics(Arc::new(store), AnalyticsSource::Daemon)),
                Err(err) => {
                    tracing::warn!(error = %err, "analytics store unavailable");
                    Ok(client)
                }
            };
        }
        let config = Config::load().context("failed to load Spotify config")?;
        let client = SpotifyClient::new(config)?.with_token_cache(self.token_cache.clone());
        match AnalyticsStore::open_default().await {
            Ok(store) => Ok(client.with_analytics(Arc::new(store), AnalyticsSource::Daemon)),
            Err(err) => {
                tracing::warn!(error = %err, "analytics store unavailable");
                Ok(client)
            }
        }
    }
}

// Build the player backend from config, with a safe fallback path
// for the first-run / missing-config case. Returns the box, its
// event stream, and the token slot the daemon shares with the
// backend's TokenProvider.
fn build_player_or_default() -> (
    Box<dyn PlayerBackend>,
    tokio_stream::wrappers::UnboundedReceiverStream<PlayerEvent>,
    Arc<RwLock<Option<String>>>,
) {
    let token_slot = Arc::new(RwLock::new(None::<String>));
    let config = Config::load();
    match config {
        Ok(config) => match player_factory::build_player(&config, token_slot.clone()) {
            Ok((backend, stream)) => (backend, stream, token_slot),
            Err(err) => {
                tracing::warn!(error = %err, "player factory failed; using ConnectOnly fallback");
                fallback_connect_only(token_slot)
            }
        },
        Err(err) => {
            tracing::warn!(
                error = %err,
                "config unavailable; player using ConnectOnly fallback until config is set"
            );
            fallback_connect_only(token_slot)
        }
    }
}

fn fallback_connect_only(
    token_slot: Arc<RwLock<Option<String>>>,
) -> (
    Box<dyn PlayerBackend>,
    tokio_stream::wrappers::UnboundedReceiverStream<PlayerEvent>,
    Arc<RwLock<Option<String>>>,
) {
    let token = Arc::new(player_factory::DaemonTokenProvider::new(token_slot.clone()));
    let (backend, stream) =
        spotuify_player::backends::connect_only::ConnectOnlyBackend::with_base_url(
            "https://api.spotify.com".to_string(),
            token,
        );
    (Box::new(backend), stream, token_slot)
}

// Drain the player's PlayerEvent stream and translate each event to
// the wire-level DaemonEvent. Lives on its own task so the player
// can emit asynchronously without blocking commands.
async fn forward_player_events(
    mut stream: tokio_stream::wrappers::UnboundedReceiverStream<PlayerEvent>,
    event_tx: broadcast::Sender<IpcMessage>,
    session_tracker: Arc<crate::session_tracker::SessionTracker>,
) {
    while let Some(event) = stream.next().await {
        // Phase 10 (F11): fan the raw event into the session tracker
        // BEFORE translating, so the tracker sees every transition
        // including ones we don't surface as DaemonEvents (PositionTick,
        // PreloadNext, etc.).
        session_tracker.observe(&event).await;
        let daemon_event = translate_player_event(event);
        let Some(daemon_event) = daemon_event else {
            continue;
        };
        let _ = event_tx.send(IpcMessage {
            id: 0,
            payload: IpcPayload::Event(daemon_event),
        });
    }
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
        // Playback-position deltas don't have a wire-level
        // DaemonEvent today — Phase 9.3 wires PositionTick to a
        // dedicated event. For 9.1 we drop them silently.
        PlayerEvent::PlaybackStarted { .. }
        | PlayerEvent::PlaybackPaused
        | PlayerEvent::PlaybackResumed
        | PlayerEvent::TrackChanged { .. }
        | PlayerEvent::PositionTick { .. }
        | PlayerEvent::EndOfTrack { .. }
        | PlayerEvent::PreloadNext { .. } => None,
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
    async fn spotify_client(&self) -> anyhow::Result<SpotifyClient> {
        DaemonState::spotify_client(self).await
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

    #[test]
    fn ready_translates_with_device_id_and_name() {
        let translated = translate_player_event(PlayerEvent::Ready {
            device_id: DeviceId::new("dev-7"),
            name: "studio".to_string(),
        })
        .expect("Ready must translate");
        match translated {
            DaemonEvent::PlayerReady { device_id, name } => {
                assert_eq!(device_id, "dev-7");
                assert_eq!(name, "studio");
            }
            other => panic!("expected PlayerReady, got {other:?}"),
        }
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
        match translated {
            DaemonEvent::PlayerFailed { reason, restarts } => {
                assert_eq!(reason, "sink-panic-budget");
                assert_eq!(restarts, 5);
            }
            other => panic!("expected PlayerFailed, got {other:?}"),
        }
    }

    #[test]
    fn playback_progress_events_are_dropped_until_phase_9_3() {
        // Adversarial: lock the "no wire event for these" contract so
        // Phase 9.3 can't accidentally start spamming the broadcast
        // bus with position ticks before the IPC schema gains a
        // dedicated event.
        for event in [
            PlayerEvent::PlaybackStarted {
                uri: "spotify:track:abc".to_string(),
                position_ms: 0,
            },
            PlayerEvent::PlaybackPaused,
            PlayerEvent::PlaybackResumed,
            PlayerEvent::TrackChanged {
                uri: "spotify:track:def".to_string(),
                position_ms: 0,
            },
            PlayerEvent::PositionTick {
                position_ms: 12_000,
            },
            PlayerEvent::EndOfTrack {
                uri: "spotify:track:ghi".to_string(),
            },
            PlayerEvent::PreloadNext {
                uri: "spotify:track:jkl".to_string(),
            },
        ] {
            assert!(
                translate_player_event(event.clone()).is_none(),
                "{event:?} should not produce a wire event in 9.1"
            );
        }
    }
}
