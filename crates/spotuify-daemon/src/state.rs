use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures::StreamExt;
use parking_lot::RwLock;
use spotuify_core::{
    Device, MusicProvider, ProviderError, ProviderId, Queue, RemoteTransport, ResourceUri,
};
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

#[cfg(test)]
mod provider_reload_transaction_tests {
    #![allow(clippy::panic, clippy::unwrap_used)]

    use std::ffi::OsString;
    use std::sync::Arc;
    use std::time::Duration;

    use super::{AudioOutputFaults, DaemonState};

    use spotuify_protocol::{
        MutationId, Operation, OperationId, OperationKind, OperationSource, OperationStatus,
        Receipt, ReceiptId, ReceiptStatus, Request,
    };

    struct TestEnv {
        temp: tempfile::TempDir,
        old: Vec<(&'static str, Option<OsString>)>,
    }

    impl TestEnv {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("tempdir");
            let keys = [
                "SPOTUIFY_FAKE_SPOTIFY",
                "SPOTUIFY_CONFIG",
                "SPOTUIFY_CONFIG_DIR",
                "SPOTUIFY_CACHE_DB",
                "SPOTUIFY_ANALYTICS_DB",
                "SPOTUIFY_SEARCH_INDEX",
                "SPOTUIFY_RUNTIME_DIR",
            ];
            let old = keys
                .into_iter()
                .map(|key| (key, std::env::var_os(key)))
                .collect();
            std::env::remove_var("SPOTUIFY_FAKE_SPOTIFY");
            std::env::set_var("SPOTUIFY_CONFIG", temp.path().join("spotuify.toml"));
            std::env::set_var("SPOTUIFY_CONFIG_DIR", temp.path().join("config"));
            std::env::set_var("SPOTUIFY_CACHE_DB", temp.path().join("cache.sqlite3"));
            std::env::set_var(
                "SPOTUIFY_ANALYTICS_DB",
                temp.path().join("analytics.sqlite3"),
            );
            std::env::set_var("SPOTUIFY_SEARCH_INDEX", temp.path().join("search-index"));
            std::env::set_var("SPOTUIFY_RUNTIME_DIR", temp.path().join("runtime"));
            Self { temp, old }
        }

        fn write(&self, dataset: &str) {
            self.write_raw(&format!(
                r#"
[providers]
default = "primary"

[providers.primary]
type = "fake"
dataset = "{dataset}"
"#
            ));
        }

        fn write_raw(&self, config: &str) {
            std::fs::write(self.temp.path().join("spotuify.toml"), config)
                .expect("write provider config");
        }
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            for (key, value) in &self.old {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[tokio::test]
    async fn malformed_adapter_reload_keeps_the_working_registry() {
        let _guard = crate::ENV_LOCK.lock().await;
        let env = TestEnv::new();
        env.write("standard");
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        let original = state.providers().await.expect("initial provider registry");

        env.write("not-a-dataset");
        let loaded = spotuify_config::load().expect("provider-neutral config parse");
        let error = state
            .apply_runtime_config(&loaded.config)
            .await
            .expect_err("adapter-specific validation must reject reload");
        assert!(format!("{error:#}").contains("unknown fake dataset"));

        let retained = state.providers().await.expect("working registry retained");
        assert!(Arc::ptr_eq(&original, &retained));

        state.request_shutdown();
        state.shutdown_player().await;
        state.shutdown_search().await;
        state
            .shutdown_background_tasks(Duration::from_millis(100))
            .await;
    }

    #[tokio::test]
    async fn accepted_reload_uses_validated_snapshot_after_disk_changes() {
        let _guard = crate::ENV_LOCK.lock().await;
        let env = TestEnv::new();
        env.write("standard");
        let state = DaemonState::new().await.expect("daemon state");
        let original = state.providers().await.expect("initial provider registry");
        env.write("empty");
        let loaded = spotuify_config::load().expect("provider-neutral config parse");

        state
            .apply_runtime_config(&loaded.config)
            .await
            .expect("valid reload accepted");
        env.write("not-a-dataset");

        let reloaded = state
            .providers()
            .await
            .expect("registry must build from accepted snapshot, not changed disk");
        assert!(!Arc::ptr_eq(&original, &reloaded));
        let standard_track = spotuify_core::ResourceUri::new(
            spotuify_core::UriScheme::new("primary").expect("configured provider URI scheme"),
            spotuify_core::MediaKind::Track,
            "track-1",
        )
        .expect("valid fake track URI");
        let item = reloaded
            .default_provider()
            .music()
            .media_item(spotuify_core::RequestContext::default(), &standard_track)
            .await
            .expect("empty dataset lookup succeeds without a match");
        assert!(
            item.is_none(),
            "accepted empty dataset must not expose standard fixture tracks"
        );

        state.request_shutdown();
        state.shutdown_player().await;
        state.shutdown_search().await;
        state
            .shutdown_background_tasks(Duration::from_millis(100))
            .await;
    }

    #[tokio::test]
    async fn provider_init_failure_retries_before_processing_and_generic_receipt_recovery() {
        let _guard = crate::ENV_LOCK.lock().await;
        let env = TestEnv::new();
        env.write("not-a-dataset");
        std::env::set_var("SPOTUIFY_FAKE_SPOTIFY", "1");
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        std::env::remove_var("SPOTUIFY_FAKE_SPOTIFY");
        let receipt_id = ReceiptId::new_v7();
        let operation_id = OperationId::new_v7();
        let receipt = Receipt {
            receipt_id,
            action: "library-save".to_string(),
            status: ReceiptStatus::Pending,
            message: "queued".to_string(),
            started_at_ms: 10,
            finished_at_ms: None,
            error: None,
        };
        let operation = Operation {
            operation_id,
            kind: OperationKind::LibrarySave,
            occurred_at_ms: 10,
            finished_at_ms: None,
            source: OperationSource::Cli,
            requester: None,
            subject_uris: vec!["spotify:track:track-1".to_string()],
            reversible: true,
            reversal_plan: None,
            pre_state: None,
            status: OperationStatus::Pending,
            receipt_id: Some(receipt_id),
            subject_op_id: None,
            undone_by_op_id: None,
            redone_by_op_id: None,
            error_message: None,
        };
        let request_json = serde_json::to_string(&Request::LibrarySave {
            uri: Some("spotify:track:track-1".to_string()),
            current: false,
        })
        .unwrap();
        state
            .store()
            .claim_mutation(
                MutationId::new_v7(),
                "provider-init-recovery",
                &request_json,
                &receipt,
                &operation,
                10,
            )
            .await
            .unwrap();

        assert!(state.providers().await.is_err());
        assert_eq!(
            state
                .store()
                .processing_mutation_claims()
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            state.store().get_receipt(receipt_id).await.unwrap().status,
            ReceiptStatus::Pending
        );

        std::env::set_var("SPOTUIFY_FAKE_SPOTIFY", "1");
        state
            .providers()
            .await
            .expect("provider initialization must retry after the fault clears");
        assert_eq!(
            crate::handler::recover_processing_mutations(&state)
                .await
                .unwrap(),
            (1, 0)
        );
        state
            .recover_pending_receipts_after_startup()
            .await
            .unwrap();
        assert!(state
            .store()
            .processing_mutation_claims()
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            state.store().get_receipt(receipt_id).await.unwrap().status,
            ReceiptStatus::Failed
        );
        assert!(state
            .store()
            .provider_reconciliation_exists(receipt_id)
            .await
            .unwrap());

        state.request_shutdown();
        state.shutdown_player().await;
        state.shutdown_search().await;
        state
            .shutdown_background_tasks(Duration::from_millis(100))
            .await;
    }

    #[tokio::test]
    async fn unaccepted_disk_changes_do_not_retarget_auth_or_player() {
        let _guard = crate::ENV_LOCK.lock().await;
        let env = TestEnv::new();
        env.write_raw(
            r#"
[providers]
default = "primary"

[providers.primary]
type = "fake"
dataset = "standard"

[providers.primary.player]
device_name = "Accepted Device"
"#,
        );
        let state = DaemonState::new().await.expect("daemon state");

        env.write_raw(
            r#"
[providers]
default = "primary"

[providers.primary]
type = "fake"
dataset = "standard"

[providers.primary.player]
device_name = "Rejected Device"

[providers.unaccepted-cloud]
type = "spotify"
client_id = "unaccepted"
redirect_uri = "http://127.0.0.1:8898/login"
"#,
        );

        let target = state
            .configured_health_auth_target()
            .await
            .expect("accepted auth target");
        assert_eq!(target.provider_id.as_str(), "primary");
        assert_eq!(
            target.strategy,
            crate::provider_factory::ProviderAuthStrategy::None
        );
        assert_eq!(state.configured_device_name(), "Accepted Device");

        state.request_shutdown();
        state.shutdown_player().await;
        state.shutdown_search().await;
        state
            .shutdown_background_tasks(Duration::from_millis(100))
            .await;
    }

    #[tokio::test]
    async fn live_audio_output_updates_accepted_snapshot_and_direct_edits_are_rejected() {
        let _guard = crate::ENV_LOCK.lock().await;
        let env = TestEnv::new();
        let config_with_output = |output: &str| {
            format!(
                r#"
[providers]
default = "primary"

[providers.primary]
type = "fake"
dataset = "standard"

[providers.primary.player]
audio_output_device = "{output}"
"#
            )
        };
        env.write_raw(&config_with_output("Output A"));
        let state = DaemonState::new().await.expect("daemon state");

        let injected = state
            .set_player_audio_output_with_persistence(
                Some("Transient Output".to_string()),
                |path, persisted| {
                    spotuify_config::set_config_value(&path, persisted.as_deref().unwrap_or(""))?;
                    anyhow::bail!("injected post-rename persistence failure")
                },
            )
            .await
            .expect_err("post-rename error must be reported");
        assert!(injected
            .to_string()
            .contains("injected post-rename persistence failure"));
        assert_eq!(
            state
                .accepted_player_settings()
                .audio_output_device
                .as_deref(),
            Some("Output A")
        );
        assert_eq!(
            spotuify_config::load()
                .expect("rolled-back config")
                .config
                .default_provider()
                .expect("default provider")
                .player_settings()
                .expect("rolled-back settings")
                .audio_output_device
                .as_deref(),
            Some("Output A")
        );

        let recovery_join = state
            .set_player_audio_output_with_persistence_and_faults(
                Some("Recovery Task Output".to_string()),
                |path, persisted| {
                    spotuify_config::set_config_value(&path, persisted.as_deref().unwrap_or(""))?;
                    anyhow::bail!("injected persistence failure before recovery panic")
                },
                AudioOutputFaults {
                    recovery_task_panics: true,
                    ..AudioOutputFaults::default()
                },
            )
            .await
            .expect_err("recovery join failure must be reported after fallback rollback");
        assert!(recovery_join
            .to_string()
            .contains("initial recovery task failed"));
        assert_eq!(
            state
                .accepted_player_settings()
                .audio_output_device
                .as_deref(),
            Some("Output A")
        );
        assert_eq!(
            spotuify_config::load()
                .expect("fallback-rolled-back config")
                .config
                .default_provider()
                .expect("default provider")
                .player_settings()
                .expect("fallback-rolled-back settings")
                .audio_output_device
                .as_deref(),
            Some("Output A")
        );

        let actor_reconcile = state
            .set_player_audio_output_with_persistence_and_faults(
                Some("Actor Failure Output".to_string()),
                |path, persisted| {
                    spotuify_config::set_config_value(&path, persisted.as_deref().unwrap_or(""))?;
                    anyhow::bail!("injected persistence failure before actor reconciliation")
                },
                AudioOutputFaults {
                    recovery_task_panics: true,
                    recovery_preserves_disk: true,
                    reconcile_actor_fails: true,
                },
            )
            .await
            .expect_err("actor reconciliation failure must be reported");
        assert!(actor_reconcile
            .to_string()
            .contains("accepted state reconciled to disk"));
        assert!(actor_reconcile
            .to_string()
            .contains("initial recovery task failed"));
        assert!(actor_reconcile
            .to_string()
            .contains("rollback write failed"));
        assert_eq!(
            state
                .accepted_player_settings()
                .audio_output_device
                .as_deref(),
            Some("Actor Failure Output")
        );
        assert_eq!(
            state
                .accepted_provider_config()
                .await
                .expect("actor-failure accepted snapshot")
                .default_provider()
                .expect("default provider")
                .player_settings()
                .expect("actor-failure accepted settings")
                .audio_output_device
                .as_deref(),
            Some("Actor Failure Output")
        );
        assert_eq!(
            spotuify_config::load()
                .expect("actor-failure rolled-back config")
                .config
                .default_provider()
                .expect("default provider")
                .player_settings()
                .expect("actor-failure settings")
                .audio_output_device
                .as_deref(),
            Some("Actor Failure Output")
        );

        state
            .set_player_audio_output(Some("Output B".to_string()))
            .await
            .expect("live output update");
        let accepted = state.accepted_provider_config().await.expect("snapshot");
        assert_eq!(
            accepted
                .default_provider()
                .expect("default provider")
                .player_settings()
                .expect("player settings")
                .audio_output_device
                .as_deref(),
            Some("Output B")
        );
        assert_eq!(
            state
                .accepted_player_settings()
                .audio_output_device
                .as_deref(),
            Some("Output B")
        );

        env.write_raw(&config_with_output("Output B"));
        let matching = spotuify_config::load().expect("matching persisted output");
        state
            .apply_runtime_config(&matching.config)
            .await
            .expect("matching live output must not poison reload");

        let (left, right) = tokio::join!(
            state.set_player_audio_output(Some("Output C".to_string())),
            state.set_player_audio_output(Some("Output D".to_string())),
        );
        left.expect("first concurrent output update");
        right.expect("second concurrent output update");
        let live_output = state
            .accepted_player_settings()
            .audio_output_device
            .expect("live output");
        let accepted_output = state
            .accepted_provider_config()
            .await
            .expect("accepted config")
            .default_provider()
            .expect("default provider")
            .player_settings()
            .expect("accepted player settings")
            .audio_output_device
            .expect("accepted output");
        let disk_output = spotuify_config::load()
            .expect("persisted config")
            .config
            .default_provider()
            .expect("persisted default provider")
            .player_settings()
            .expect("persisted player settings")
            .audio_output_device
            .expect("persisted output");
        assert_eq!(accepted_output, live_output);
        assert_eq!(disk_output, live_output);

        env.write_raw(&config_with_output("Direct Edit"));
        let direct = spotuify_config::load().expect("direct output edit");
        let error = state
            .apply_runtime_config(&direct.config)
            .await
            .expect_err("direct output edit must require the live command");
        assert!(matches!(
            error.downcast_ref::<spotuify_core::ProviderError>(),
            Some(spotuify_core::ProviderError::InvalidInput { field, .. })
                if field == "player.audio_output_device"
        ));

        state.request_shutdown();
        state.shutdown_player().await;
        state.shutdown_search().await;
        state
            .shutdown_background_tasks(Duration::from_millis(100))
            .await;
    }
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

use spotuify_protocol::{
    DaemonEvent, DaemonStatus, IpcMessage, IpcPayload, Request, IPC_PROTOCOL_VERSION,
};

use crate::viz_coordinator::VizCoordinator;
use spotuify_search::{
    SearchIndex, SearchIndexRebuild, SearchIndexRebuildReason, SearchServiceHandle,
};
use spotuify_spotify::auth::StoredToken;
use spotuify_spotify::client::{MediaItem, SchemaCompatReporter};
use spotuify_store::Store;

type PlayerBox = Box<dyn PlayerBackend>;
type PlayerEventStream = tokio_stream::wrappers::UnboundedReceiverStream<PlayerEvent>;
type PlayerTokenSlot = Arc<RwLock<Option<String>>>;
const PENDING_QUEUE_APPEND_TTL_MS: i64 = 5_000;

async fn open_search_service(store: &Store) -> Result<(SearchServiceHandle, JoinHandle<()>)> {
    let opened = SearchIndex::open_with_rebuild_status(store.index_path())?;
    let rebuild = opened.rebuild;
    let (search, worker) = SearchServiceHandle::start(opened.index);

    let rebuild = match rebuild {
        Some(rebuild) => Some(rebuild),
        None => {
            let expected = store.media_items_count(None).await?;
            let actual = search.num_docs().await?;
            (actual != expected).then(|| SearchIndexRebuild {
                reason: SearchIndexRebuildReason::DocumentCountMismatch,
                detail: format!(
                    "SQLite contains {expected} media rows but Tantivy contains {actual} documents"
                ),
            })
        }
    };
    let Some(rebuild) = rebuild else {
        return Ok((search, worker));
    };

    let started_at_ms = spotuify_store::now_ms();
    let stats = match spotuify_search::reindex::reindex(store, &search).await {
        Ok(stats) => stats,
        Err(err) => {
            let _ = search.request_shutdown().await;
            return Err(err).context("failed to repopulate rebuilt search index from cache");
        }
    };
    let reason = rebuild.reason.as_str();
    tracing::warn!(
        reason,
        detail = %rebuild.detail,
        indexed = stats.indexed,
        index_documents = stats.index_documents,
        "rebuilt search index and repopulated it from SQLite cache"
    );

    let event_domain = format!("search_index_repair/{reason}");
    if let Err(err) = store
        .record_sync_event(&event_domain, started_at_ms, "ok", stats.indexed, None)
        .await
    {
        tracing::warn!(
            reason,
            error = %err,
            "failed to record search index startup repair"
        );
    }

    Ok((search, worker))
}

#[derive(Clone, Debug)]
struct PendingQueueAppend {
    provider: ProviderId,
    item: MediaItem,
    required_occurrence: usize,
    added_at_ms: i64,
}

fn pending_queue_appends_for(
    provider: &ProviderId,
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
                provider: provider.clone(),
                item: item.clone(),
                required_occurrence: *count,
                added_at_ms,
            }
        })
        .collect()
}

fn merge_queue_pending_appends(
    provider: &ProviderId,
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
    for entry in pending.iter().filter(|entry| &entry.provider == provider) {
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
    Install {
        backend: PlayerBox,
        resp: oneshot::Sender<std::result::Result<(), (spotuify_player::PlayerError, PlayerBox)>>,
    },
    /// Roll back an installation that could not attach its event stream.
    /// This is intentionally daemon-private: normal provider changes require
    /// restart, while a failed install must return ownership to the registry.
    Uninstall {
        resp: oneshot::Sender<Option<PlayerBox>>,
    },
    RegisterDevice {
        name: String,
        resp: oneshot::Sender<PlayerResult<DeviceId>>,
    },
    Reconnect {
        name: String,
        /// When the session dropped while we were the active player, the
        /// `(uri, position_ms)` to resume after re-registering so audio
        /// continues instead of coming up idle. `None` = re-register only.
        resume: Option<(String, u32)>,
        resp: oneshot::Sender<PlayerResult<DeviceId>>,
    },
    SetAudioOutput {
        device: Option<String>,
        resp: oneshot::Sender<()>,
    },
    IsConnected {
        resp: oneshot::Sender<bool>,
    },
    /// Tear down the live librespot session (keeps the actor running) so
    /// it stops minting after logout. See `DaemonState::drop_player_session`.
    DropSession {
        resp: oneshot::Sender<PlayerResult<()>>,
    },
    Shutdown {
        resp: oneshot::Sender<()>,
    },
}

#[derive(Clone, Copy, Default)]
struct AudioOutputFaults {
    #[cfg(test)]
    recovery_task_panics: bool,
    #[cfg(test)]
    recovery_preserves_disk: bool,
    #[cfg(test)]
    reconcile_actor_fails: bool,
}

fn read_player_audio_output() -> Result<Option<String>> {
    spotuify_config::load()
        .map_err(anyhow::Error::from)
        .and_then(|loaded| {
            loaded
                .config
                .default_provider()
                .context("persisted config has no default provider")?
                .player_settings()
                .map(|settings| settings.audio_output_device)
                .map_err(anyhow::Error::from)
        })
}

fn rollback_and_read_player_audio_output(
    path: spotuify_config::ConfigPath,
    previous: Option<String>,
) -> (Result<()>, Result<Option<String>>) {
    let rollback = spotuify_config::set_config_value(&path, previous.as_deref().unwrap_or(""))
        .map_err(anyhow::Error::from);
    let observed = read_player_audio_output();
    (rollback, observed)
}

struct PlayerTransportCommand {
    cmd: TransportCmd,
    resp: oneshot::Sender<PlayerResult<()>>,
}

#[derive(Debug, Clone)]
pub(crate) enum TransportCmd {
    PlayUri {
        uri: String,
        position_ms: u32,
    },
    /// Load a collection context (album/playlist URI, or an explicit
    /// ordered track list) and start at `start_uri`. Resolved daemon-side
    /// before dispatch so the player actor receives the ready track list.
    PlayContext {
        context_uri: Option<String>,
        tracks: Option<Vec<String>>,
        start_uri: String,
        position_ms: u32,
    },
    Pause,
    Resume,
    Next,
    Previous,
    Seek {
        position_ms: u32,
    },
    Volume {
        percent: u8,
    },
    Shuffle {
        on: bool,
    },
    Repeat {
        mode: RepeatMode,
    },
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
    /// Last audio-flow watchdog verdict: whether the sink's PCM counter was
    /// advancing. `false` while connected+playing is the "playing but silent"
    /// signature (keepalive / audio-route failure vs a plain network drop).
    pub samples_advancing: bool,
    /// `now_ms` of the last detected audio stall (counter flat while playing).
    pub last_stall_ms: Option<i64>,
    /// Running count of auto-reconnects scheduled (any path).
    pub reconnect_attempts: u32,
    /// Backoff applied to the most recent reconnect, in ms.
    pub current_backoff_ms: u64,
}

/// Stop auto-reconnecting after this many consecutive failed probes to
/// avoid a reconnect storm against a persistently unreachable Spotify.
/// At the 60s probe cadence this is ~5 minutes of retries.
pub(crate) const PLAYER_RECONNECT_GIVE_UP_AFTER: u32 = 5;

/// Base delay before an auto-reconnect attempt. Matches the historical fixed
/// 1s so the first drop still reconnects promptly.
pub(crate) const PLAYER_RECONNECT_BASE_BACKOFF: Duration = Duration::from_secs(1);
/// Ceiling for the exponential reconnect backoff.
pub(crate) const PLAYER_RECONNECT_MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Exponential backoff for repeated reconnect attempts so frequent session
/// drops don't churn (reconnect → drop → reconnect with no spacing). `0`/`1`
/// failures → base (1s); then `base * 2^(failures-1)`, capped at
/// [`PLAYER_RECONNECT_MAX_BACKOFF`]. Overflow-safe.
pub(crate) fn reconnect_backoff(consecutive_failures: u32) -> Duration {
    let base_ms = PLAYER_RECONNECT_BASE_BACKOFF.as_millis() as u64;
    let max_ms = PLAYER_RECONNECT_MAX_BACKOFF.as_millis() as u64;
    let shift = consecutive_failures.saturating_sub(1).min(32);
    let factor = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
    let ms = base_ms.saturating_mul(factor).min(max_ms);
    Duration::from_millis(ms)
}

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

/// Verdict from the audio-flow watchdog comparing the clock's `is_playing`
/// against whether the sink's PCM sample counter is actually advancing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioFlowVerdict {
    /// Samples advancing (or just reset on a fresh sink start) — healthy.
    Flowing,
    /// Not playing, or no counter (non-embedded backend) — watchdog inert.
    NotPlaying,
    /// Flat samples while playing, but within the grace window (track
    /// transition / pre-roll buffering) — tolerate, don't act yet.
    Buffering,
    /// Flat samples while playing past the grace window — the audio pipeline
    /// has stalled even though the clock says we're playing.
    Stalled,
}

/// Pure decision for the audio-flow watchdog. `current_samples == None` means
/// a non-embedded backend (no counter) → inert.
///
/// CRITICAL: the shared sink counter `reset()`s to 0 on every sink `start()`
/// (per track / reconnect — see `librespot_sink_chain.rs`), so a *decrease* in
/// the reading means audio just (re)started and is `Flowing`, NOT stalled.
pub(crate) fn classify_audio_flow(
    is_playing: bool,
    current_samples: Option<u64>,
    last_samples: Option<u64>,
    stalled_for_ms: i64,
    stall_threshold_ms: i64,
) -> AudioFlowVerdict {
    if !is_playing {
        return AudioFlowVerdict::NotPlaying;
    }
    let Some(current) = current_samples else {
        return AudioFlowVerdict::NotPlaying;
    };
    match last_samples {
        // First observation, or the counter advanced/reset → audio is moving.
        None => AudioFlowVerdict::Flowing,
        Some(last) if current != last => AudioFlowVerdict::Flowing,
        // Flat while playing: tolerate until the grace window elapses.
        Some(_) => {
            if stalled_for_ms >= stall_threshold_ms {
                AudioFlowVerdict::Stalled
            } else {
                AudioFlowVerdict::Buffering
            }
        }
    }
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

/// Claim-style rate gate for on-demand Web-API refreshes. `try_claim`
/// succeeds at most once per `min_gap_ms`; concurrent callers race on
/// a CAS so exactly one wins the fetch and the rest skip.
#[derive(Default)]
pub(crate) struct RefreshGate {
    last_attempt_ms: std::sync::atomic::AtomicI64,
}

impl RefreshGate {
    pub(crate) fn try_claim(&self, now_ms: i64, min_gap_ms: i64) -> bool {
        let last = self.last_attempt_ms.load(Ordering::Acquire);
        if now_ms.saturating_sub(last) < min_gap_ms {
            return false;
        }
        self.last_attempt_ms
            .compare_exchange(last, now_ms, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Record a fetch that bypassed the gate (mutation reconciles) so
    /// gated callers right after it still coalesce.
    pub(crate) fn stamp(&self, now_ms: i64) {
        self.last_attempt_ms.store(now_ms, Ordering::Release);
    }
}

#[allow(dead_code)] // Stage A: activated as handlers migrate to provider routing.
#[derive(Clone, Debug, Eq, PartialEq)]
enum ProviderRegistryKey {
    Injected,
    Fake,
    Configured,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProviderTopology {
    default_provider: Option<ProviderId>,
    providers: Vec<(ProviderId, String)>,
    default_player: Option<(ProviderId, spotuify_config::PlayerSettings)>,
}

impl ProviderTopology {
    fn from_config(config: &spotuify_config::AppConfig) -> Self {
        let mut providers = config
            .providers
            .iter()
            .map(|provider| (provider.id.clone(), provider.kind.clone()))
            .collect::<Vec<_>>();
        providers.sort_by(|left, right| left.0.cmp(&right.0));
        Self {
            default_provider: config.default_provider.clone(),
            providers,
            default_player: config.default_provider().and_then(|provider| {
                matches!(provider.kind.as_str(), "spotify" | "fake")
                    .then(|| provider.player_settings().ok())
                    .flatten()
                    .map(|mut settings| {
                        // Audio output is a live setting applied through
                        // SetAudioOutput. Persisting it must not turn every
                        // later config reload into a restart-required change.
                        settings.audio_output_device = None;
                        (provider.id.clone(), settings)
                    })
            }),
        }
    }

    fn from_registry(registry: &crate::provider_registry::ProviderRegistry) -> Self {
        let catalog = registry.catalog();
        let mut providers = catalog
            .providers
            .into_iter()
            .map(|provider| (provider.id, provider.uri_scheme.to_string()))
            .collect::<Vec<_>>();
        providers.sort_by(|left, right| left.0.cmp(&right.0));
        Self {
            default_provider: catalog.default_provider,
            providers,
            default_player: None,
        }
    }
}

#[allow(dead_code)] // Stage A: activated as handlers migrate to provider routing.
struct ProviderRegistryCache {
    key: Option<ProviderRegistryKey>,
    registry: Option<Arc<crate::provider_registry::ProviderRegistry>>,
    generation: u64,
}

impl ProviderRegistryCache {
    fn new(injected: Option<Arc<crate::provider_registry::ProviderRegistry>>) -> Self {
        Self {
            key: injected.as_ref().map(|_| ProviderRegistryKey::Injected),
            registry: injected,
            generation: 0,
        }
    }
}

#[derive(Clone)]
struct ProviderSyncLocks {
    fast: Arc<Mutex<()>>,
    slow: Arc<Mutex<()>>,
}

impl ProviderSyncLocks {
    fn new() -> Self {
        Self {
            fast: Arc::new(Mutex::new(())),
            slow: Arc::new(Mutex::new(())),
        }
    }

    fn for_target(&self, target: spotuify_protocol::SyncTargetData) -> Vec<Arc<Mutex<()>>> {
        use spotuify_protocol::SyncTargetData;
        match target {
            // Acquire the infrequent slow lane first. If a scheduled library
            // sync owns it, a manual `All` request must not hold the fast lane
            // while waiting and freeze playback/queue/device reconciliation.
            SyncTargetData::All => vec![self.slow.clone(), self.fast.clone()],
            SyncTargetData::Playlists | SyncTargetData::Library => vec![self.slow.clone()],
            SyncTargetData::Playback
            | SyncTargetData::Queue
            | SyncTargetData::Devices
            | SyncTargetData::Recent => vec![self.fast.clone()],
        }
    }
}

#[cfg(test)]
mod provider_sync_lock_tests {
    #![allow(clippy::panic, clippy::unwrap_used)]

    use super::ProviderSyncLocks;
    use spotuify_protocol::SyncTargetData;

    #[tokio::test]
    async fn all_waits_for_slow_lane_without_holding_fast_lane() {
        let locks = ProviderSyncLocks::new();
        let slow_guard = locks.slow.clone().lock_owned().await;
        let fast = locks.fast.clone();
        let ordered = locks.for_target(SyncTargetData::All);
        let waiter = tokio::spawn(async move {
            let mut guards = Vec::new();
            for lock in ordered {
                guards.push(lock.lock_owned().await);
            }
            guards
        });

        tokio::task::yield_now().await;
        assert!(
            fast.try_lock().is_ok(),
            "manual all sync must not reserve the fast lane while slow is busy"
        );

        waiter.abort();
        let _ = waiter.await;
        drop(slow_guard);
    }
}

enum EventLogCommand {
    Push(spotuify_protocol::LoggedEvent),
    Snapshot(oneshot::Sender<Vec<spotuify_protocol::LoggedEvent>>),
}

#[derive(Clone)]
struct EventLogWriter {
    tx: mpsc::UnboundedSender<EventLogCommand>,
}

impl EventLogWriter {
    fn spawn(mut shutdown_rx: watch::Receiver<bool>) -> (Self, JoinHandle<()>) {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let worker = tokio::spawn(async move {
            let mut log = spotuify_protocol::EventLog::new(128);
            loop {
                tokio::select! {
                    biased;
                    command = rx.recv() => match command {
                        Some(EventLogCommand::Push(event)) => log.push(event),
                        Some(EventLogCommand::Snapshot(response)) => {
                            let _ = response.send(log.snapshot());
                        }
                        None => break,
                    },
                    changed = shutdown_rx.changed() => {
                        if changed.is_err() || *shutdown_rx.borrow_and_update() {
                            while let Ok(command) = rx.try_recv() {
                                match command {
                                    EventLogCommand::Push(event) => log.push(event),
                                    EventLogCommand::Snapshot(response) => {
                                        let _ = response.send(log.snapshot());
                                    }
                                }
                            }
                            break;
                        }
                    }
                }
            }
        });
        (Self { tx }, worker)
    }

    fn push(&self, event: spotuify_protocol::LoggedEvent) {
        let _ = self.tx.send(EventLogCommand::Push(event));
    }

    async fn snapshot(&self) -> Vec<spotuify_protocol::LoggedEvent> {
        let (response, receiver) = oneshot::channel();
        if self.tx.send(EventLogCommand::Snapshot(response)).is_err() {
            return Vec::new();
        }
        receiver.await.unwrap_or_default()
    }
}

#[derive(Clone)]
struct DaemonEventEmitter {
    event_tx: broadcast::Sender<IpcMessage>,
    event_log: EventLogWriter,
    system_integration: Arc<spotuify_system::SystemIntegration>,
    /// One synchronous linearization point for log enqueue + broadcast.
    order: Arc<parking_lot::Mutex<()>>,
}

impl DaemonEventEmitter {
    fn emit(&self, event: DaemonEvent) {
        let _order = self.order.lock();
        let event = spotuify_protocol::sanitize_daemon_event(event);
        if let Some(logged) =
            spotuify_protocol::LoggedEvent::from(&event, crate::analytics::now_ms())
        {
            self.event_log.push(logged);
        }
        let system = self.system_integration.clone();
        let event_for_system = event.clone();
        tokio::spawn(async move {
            system.handle_event(&event_for_system).await;
        });
        let _ = self.event_tx.send(IpcMessage {
            id: 0,
            source: None,
            mutation_id: None,
            payload: IpcPayload::Event(event),
        });
    }
}

#[derive(Clone)]
struct ActivePlayerPolicy {
    reason: String,
    generation: u64,
}

#[derive(Clone)]
struct PlayerPolicyBarrier {
    provider: ProviderId,
    generation: Option<u64>,
}

#[derive(Default)]
struct PlayerPolicyState {
    active: std::collections::BTreeMap<ProviderId, ActivePlayerPolicy>,
    recent: HashMap<(ProviderId, String), Instant>,
    next_generation: u64,
}

#[derive(Clone)]
struct PlayerPolicyEventEmitter {
    events: DaemonEventEmitter,
    embedded_provider_id: Arc<RwLock<Option<ProviderId>>>,
    /// Recent policies per provider. A backend may both emit a
    /// `PlayerEvent::ProviderPolicy` and return `PlayerError::ProviderPolicy`
    /// for the same operation; the short window collapses that race while
    /// allowing a later recurrence to surface after recovery.
    state: Arc<parking_lot::Mutex<PlayerPolicyState>>,
}

impl PlayerPolicyEventEmitter {
    fn emit_for_provider(&self, provider: ProviderId, reason: &str) -> bool {
        const DEDUP_WINDOW: Duration = Duration::from_secs(30);
        let reason = spotuify_protocol::sanitize_provider_policy_reason(reason);
        let should_emit = {
            let now = Instant::now();
            let mut state = self.state.lock();
            state
                .recent
                .retain(|_, seen| now.saturating_duration_since(*seen) < DEDUP_WINDOW);
            let key = (provider.clone(), reason.clone());
            let duplicate = match state.recent.entry(key) {
                std::collections::hash_map::Entry::Occupied(_) => true,
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(now);
                    false
                }
            };
            if duplicate {
                if state
                    .active
                    .get(&provider)
                    .is_some_and(|active| active.reason == reason)
                {
                    state.next_generation = state.next_generation.wrapping_add(1);
                    let generation = state.next_generation;
                    state
                        .active
                        .insert(provider, ActivePlayerPolicy { reason, generation });
                }
                false
            } else {
                state.next_generation = state.next_generation.wrapping_add(1);
                let generation = state.next_generation;
                state.active.insert(
                    provider.clone(),
                    ActivePlayerPolicy {
                        reason: reason.clone(),
                        generation,
                    },
                );
                self.events
                    .emit(DaemonEvent::ProviderPolicy { provider, reason });
                true
            }
        };
        should_emit
    }

    fn barrier_for_provider(&self, provider: ProviderId) -> PlayerPolicyBarrier {
        let generation = self
            .state
            .lock()
            .active
            .get(&provider)
            .map(|active| active.generation);
        PlayerPolicyBarrier {
            provider,
            generation,
        }
    }

    fn barrier_for_current_provider(&self) -> Option<PlayerPolicyBarrier> {
        self.embedded_provider_id
            .read()
            .clone()
            .map(|provider| self.barrier_for_provider(provider))
    }

    fn clear_if_unchanged(&self, barrier: &PlayerPolicyBarrier) -> bool {
        let mut state = self.state.lock();
        let Some(active) = state.active.get(&barrier.provider) else {
            return false;
        };
        if Some(active.generation) != barrier.generation {
            return false;
        }
        let reason = state
            .active
            .remove(&barrier.provider)
            .expect("active policy checked above")
            .reason;
        state
            .recent
            .retain(|(seen_provider, _), _| seen_provider != &barrier.provider);
        self.events.emit(DaemonEvent::ProviderPolicyCleared {
            provider: barrier.provider.clone(),
            reason,
        });
        true
    }

    fn active(&self) -> Vec<spotuify_protocol::ProviderPolicyNotice> {
        self.state
            .lock()
            .active
            .iter()
            .map(
                |(provider, active)| spotuify_protocol::ProviderPolicyNotice {
                    provider: provider.clone(),
                    reason: active.reason.clone(),
                },
            )
            .collect()
    }

    fn emit_error(&self, error: &spotuify_player::PlayerError) -> bool {
        let spotuify_player::PlayerError::ProviderPolicy(reason) = error else {
            return false;
        };
        let Some(provider) = self.embedded_provider_id.read().clone() else {
            tracing::warn!(
                "player returned a provider-policy error before ownership was installed"
            );
            return false;
        };
        self.emit_for_provider(provider, reason)
    }
}

pub(crate) fn player_error_for_display(error: &spotuify_player::PlayerError) -> String {
    match error {
        spotuify_player::PlayerError::ProviderPolicy(reason) => format!(
            "provider policy prevents playback: {}",
            spotuify_protocol::sanitize_provider_policy_reason(reason)
        ),
        other => other.to_string(),
    }
}

#[derive(Debug, thiserror::Error)]
#[error("provider policy prevents local playback: {reason}")]
pub(crate) struct ProviderPolicyRequestError {
    pub(crate) provider: ProviderId,
    pub(crate) reason: String,
}

fn player_request_error(
    error: spotuify_player::PlayerError,
    provider: ProviderId,
) -> anyhow::Error {
    match error {
        spotuify_player::PlayerError::ProviderPolicy(reason) => ProviderPolicyRequestError {
            provider,
            reason: spotuify_protocol::sanitize_provider_policy_reason(&reason),
        }
        .into(),
        error => anyhow::anyhow!(player_error_for_display(&error)),
    }
}

pub(crate) struct DaemonState {
    started_at: Instant,
    shutdown_tx: watch::Sender<bool>,
    provider_revision_tx: watch::Sender<u64>,
    pub(crate) event_tx: broadcast::Sender<IpcMessage>,
    store: Store,
    search: SearchServiceHandle,
    search_worker: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    background_tasks: StdMutex<Vec<JoinHandle<()>>>,
    token_cache: Arc<Mutex<Option<StoredToken>>>,
    auth_sessions: crate::auth_sessions::AuthSessions,
    /// Lazily-created concrete adapter factory. The factory privately owns
    /// the shared Spotify HTTP/backpressure runtime; daemon state only sees
    /// provider-neutral registries and auth outcomes.
    provider_factory: Mutex<Option<crate::provider_factory::ProviderFactory>>,
    /// Exact provider config snapshot accepted at startup or by runtime
    /// reload. Lazy registry builds consume this snapshot, never mutable disk.
    provider_config_snapshot: Mutex<Option<spotuify_config::AppConfig>>,
    /// Player settings accepted at startup, plus explicitly applied live
    /// audio-output changes. Runtime recovery never rereads mutable disk.
    player_settings: RwLock<spotuify_config::PlayerSettings>,
    /// Serializes lazy registry construction. Provider clients may take an
    /// auth/network round trip to build, so a separate lock avoids stale
    /// out-of-order installs without holding the cache mutex across awaits.
    provider_build_lock: Mutex<()>,
    provider_commit_lock: Mutex<()>,
    /// Provider-neutral routing. Production construction is lazy so the
    /// Spotify adapter captures the live embedded-device identity after
    /// player registration. A connection-state transition rebuilds the one
    /// registry entry with the same token cache and HTTP/backpressure runtime.
    #[allow(dead_code)] // Stage A: activated as handlers migrate to provider routing.
    providers: Mutex<ProviderRegistryCache>,
    /// Provider that owns the current global transport view. Retained across
    /// empty/no-session readbacks so reconciliation does not jump back to the
    /// configured default immediately after controlling another provider.
    active_transport_provider: Arc<RwLock<Option<ProviderId>>>,
    /// Provider that owns the installed local-player facet. Populated only
    /// after the actor accepts that facet, independent of adapter kind.
    embedded_provider_id: Arc<RwLock<Option<ProviderId>>>,
    provider_topology: ProviderTopology,
    transport_mutation_lock: Arc<Mutex<()>>,
    library_mutation_lock: Arc<Mutex<()>>,
    operation_mutation_lock: Arc<Mutex<()>>,
    /// Provider-scoped fast/slow sync locks. Unrelated providers never block
    /// one another; a full refresh acquires both lanes for its provider.
    sync_locks: StdMutex<HashMap<String, ProviderSyncLocks>>,
    playlist_mutation_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Once-per-process latch so the scope-drift banner fires at most once
    /// even when registry/auth probes repeat.
    scope_reauth_emitted: std::sync::atomic::AtomicBool,
    /// Once-per-process latch for the first-party-only migration advisory
    /// broadcast, so the `AuthMigrationRecommended` banner is broadcast at
    /// most once per daemon run. Late-subscribing clients still receive it
    /// via the subscribe snapshot (see `auth_migration_advisory`).
    auth_migration_emitted: std::sync::atomic::AtomicBool,
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
    event_log: EventLogWriter,
    event_emitter: DaemonEventEmitter,
    player_policy_events: PlayerPolicyEventEmitter,

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
    session_bearer: Arc<RwLock<Option<Arc<dyn spotuify_spotify::WebApiBearerProvider>>>>,
    player_event_stream_tx: mpsc::UnboundedSender<(PlayerEventStream, bool, ProviderId)>,
    player_install_lock: Mutex<()>,
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
    /// Web-API fetch gates for the on-demand refresh spawners. Every
    /// `PlaybackGet` used to fire a `/me/player` poll on top of the
    /// sync loop's cadence — with the TUI heartbeat, the macOS app,
    /// and CLI `status` calls that added up to ~13k calls/day and
    /// recurring hour-long 429 penalties. The gates coalesce bursts
    /// and defer to librespot's local truth while our device plays.
    pub(crate) playback_refresh_gate: RefreshGate,
    pub(crate) queue_refresh_gate: RefreshGate,
    pub(crate) devices_refresh_gate: RefreshGate,
    /// Per-domain dedup for `DaemonEvent::RateLimited` notices.
    rate_limit_notice_gates: parking_lot::Mutex<HashMap<String, i64>>,
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
    /// Sink-tap sample counter for the embedded backend (ground truth that
    /// audio is actually flowing). `None` for non-embedded backends, which
    /// makes the audio-flow watchdog inert. Shared clone of the handle the
    /// session tracker uses.
    audio_counter:
        Arc<RwLock<Option<Arc<spotuify_player::backends::audio_counter_tap::AudioCounterHandle>>>>,
    /// Update-awareness — the latest GitHub release observed by the periodic
    /// check (see `crate::update`). `None` until the first check resolves.
    /// Read by `Request::CheckUpdate`; written by the update loop.
    latest_release: Arc<parking_lot::Mutex<Option<crate::update::CachedRelease>>>,
    /// Provider-scoped cross-show episode feeds. The raw merged sets are
    /// cached; `Request::EpisodeFeed` applies sort + limit per call.
    episode_feed: Arc<parking_lot::Mutex<HashMap<ProviderId, CachedEpisodeFeed>>>,
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
        Self::new_with_provider_registry(None).await
    }

    /// Test-only provider injection avoids process-global environment races.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) async fn new_with_providers(
        providers: crate::provider_registry::ProviderRegistry,
    ) -> Result<Self> {
        Self::new_with_provider_registry(Some(Arc::new(providers))).await
    }

    async fn new_with_provider_registry(
        injected_providers: Option<Arc<crate::provider_registry::ProviderRegistry>>,
    ) -> Result<Self> {
        let (shutdown_tx, _) = watch::channel(false);
        let (provider_revision_tx, _) = watch::channel(0_u64);
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
        // Scope-drift detection used to fire here as a proactive
        // credential read. With the old Keychain-backed auth that triggered a
        // "spotuify wants to access the keychain" prompt at every cold
        // start, on top of the prompts the lazy `access_token_cached`
        // path already caused. Net effect: 3–5 prompts on every fresh
        // launch.
        //
        // Recovery: defer the scope-drift check to the first real API
        // call. The provider factory's cached auth probe already loads the
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
        let (search, search_worker) = open_search_service(&store).await?;

        let viz_coordinator = VizCoordinator::new(event_tx.clone());

        // Provider runtimes optionally install a paired local player after the
        // registry validates its provider ID and URI namespace. The actor
        // starts empty so metadata-only providers do not receive a null or
        // foreign backend.
        let token_slot = Arc::new(RwLock::new(None::<String>));
        let (player_event_stream_tx, mut player_event_stream_rx) =
            mpsc::unbounded_channel::<(PlayerEventStream, bool, ProviderId)>();
        let audio_counter = Arc::new(RwLock::new(None));
        let (queue_warm, queue_warm_rx) = QueueWarmScheduler::new();
        let system_config = build_system_config();
        let system_integration = Arc::new(spotuify_system::SystemIntegration::spawn(system_config));
        let (event_log, event_log_worker) = EventLogWriter::spawn(shutdown_tx.subscribe());
        let event_emitter = DaemonEventEmitter {
            event_tx: event_tx.clone(),
            event_log: event_log.clone(),
            system_integration: system_integration.clone(),
            order: Arc::new(parking_lot::Mutex::new(())),
        };

        // Phase 10 (P10.1): SessionTracker writes ListenFact rows to
        // the store and emits ListenQualified into the event broadcast
        // when the qualification rule fires.
        let session_tracker = Arc::new(crate::session_tracker::SessionTracker::with_store(
            Arc::new(store.clone()),
            event_tx.clone(),
            None,
        ));

        // Construct the clock now for the player-event forwarder, but defer
        // durable seeding until a provider registry has validated ownership.
        // Otherwise a removed adapter's last snapshot can leak through a new
        // configuration indefinitely.
        let playback_clock = crate::clock::PlaybackClock::new();

        // Shared embedded-device identity/volume cells: the forwarder task
        // writes the volume from VolumeChanged events; DaemonState reads
        // both for `connected_own_device`. Created here so the same Arcs
        // land in the struct literal below.
        let (provider_topology, provider_config_snapshot) = match injected_providers.as_ref() {
            Some(registry) => (ProviderTopology::from_registry(registry), None),
            None if std::env::var_os("SPOTUIFY_FAKE_SPOTIFY").is_some() => {
                let fake = crate::provider_factory::fake_spotify_provider()?;
                (
                    ProviderTopology {
                        default_provider: Some(MusicProvider::id(&fake).clone()),
                        providers: vec![(
                            MusicProvider::id(&fake).clone(),
                            MusicProvider::uri_scheme(&fake).to_string(),
                        )],
                        default_player: Some((
                            MusicProvider::id(&fake).clone(),
                            spotuify_config::PlayerSettings::default(),
                        )),
                    },
                    None,
                )
            }
            None => {
                let loaded = spotuify_config::load().context("failed to load provider config")?;
                for warning in &loaded.warnings {
                    tracing::warn!(warning = %warning, "deprecated configuration");
                }
                let topology = ProviderTopology::from_config(&loaded.config);
                (topology, Some(loaded.config))
            }
        };
        let player_settings = provider_config_snapshot
            .as_ref()
            .and_then(|config| config.default_provider())
            .and_then(|provider| provider.player_settings().ok())
            .or_else(|| {
                provider_topology
                    .default_player
                    .as_ref()
                    .map(|(_, settings)| settings.clone())
            })
            .unwrap_or_default();
        // Seed the embedded device's name from the accepted startup snapshot
        // so rejected later file edits cannot alter runtime reconnects.
        let initial_own_name =
            cfg!(feature = "embedded-playback").then(|| player_settings.effective_device_name());
        let own_device_name = Arc::new(parking_lot::Mutex::new(initial_own_name));
        let own_device_volume = Arc::new(parking_lot::Mutex::new(None));
        let embedded_provider_id = Arc::new(RwLock::new(None));
        let active_transport_provider = Arc::new(RwLock::new(None));
        let player_policy_events = PlayerPolicyEventEmitter {
            events: event_emitter.clone(),
            embedded_provider_id: embedded_provider_id.clone(),
            state: Arc::new(parking_lot::Mutex::new(PlayerPolicyState::default())),
        };
        let (player_tx, player_transport_tx, player_warm_tx, player_actor) =
            spawn_player_actor(None, player_policy_events.clone());

        let tracker_for_worker = session_tracker.clone();
        let viz_for_worker = viz_coordinator.clone();
        let clock_for_worker = playback_clock.clone();
        let store_for_worker = store.clone();
        let event_emitter_for_worker = event_emitter.clone();
        let player_tx_for_worker = player_tx.clone();
        let own_device_name_for_worker = own_device_name.clone();
        let own_device_volume_for_worker = own_device_volume.clone();
        let reconnect_in_flight = Arc::new(AtomicBool::new(false));
        let reconnect_in_flight_for_worker = reconnect_in_flight.clone();
        let we_are_active = Arc::new(AtomicBool::new(false));
        let we_are_active_for_worker = we_are_active.clone();
        let player_health = Arc::new(parking_lot::Mutex::new(PlayerHealth::default()));
        let player_health_for_worker = player_health.clone();
        let active_transport_provider_for_worker = active_transport_provider.clone();
        let provider_revision_tx_for_worker = provider_revision_tx.clone();
        let mutation_seq = Arc::new(AtomicU64::new(0));
        let mutation_seq_for_worker = mutation_seq.clone();
        let embedded_provider_id_for_worker = embedded_provider_id.clone();
        let player_policy_events_for_worker = player_policy_events.clone();
        let player_worker = tokio::spawn(async move {
            while let Some((player_stream, embedded_sink_on_ready, player_provider_id)) =
                player_event_stream_rx.recv().await
            {
                forward_player_events(
                    player_stream,
                    PlayerEventForwarder {
                        event_emitter: event_emitter_for_worker.clone(),
                        session_tracker: tracker_for_worker.clone(),
                        viz_coordinator: viz_for_worker.clone(),
                        playback_clock: clock_for_worker.clone(),
                        store: store_for_worker.clone(),
                        player_tx: player_tx_for_worker.clone(),
                        own_device_name: own_device_name_for_worker.clone(),
                        own_device_volume: own_device_volume_for_worker.clone(),
                        reconnect_in_flight: reconnect_in_flight_for_worker.clone(),
                        we_are_active: we_are_active_for_worker.clone(),
                        player_health: player_health_for_worker.clone(),
                        embedded_sink_on_ready,
                        player_provider_id,
                        embedded_provider_id: embedded_provider_id_for_worker.clone(),
                        player_policy_events: player_policy_events_for_worker.clone(),
                        active_transport_provider: active_transport_provider_for_worker.clone(),
                        provider_revision_tx: provider_revision_tx_for_worker.clone(),
                        mutation_seq: mutation_seq_for_worker.clone(),
                    },
                )
                .await;
            }
        });

        // Phase 14 (P14-G) — system-integration actor. Reads config
        // for opt-in subsystems; if the config can't be loaded
        // (first-run / missing client_id) we still build the cover
        // cache and a no-op hook dispatcher so the daemon stays up.
        // Phase 17 — apply persisted viz config. Best-effort: missing
        // first-run config leaves the default-off coordinator idle.
        if let Ok(config) = spotuify_config::load() {
            apply_viz_config(&viz_coordinator, &config.config.viz).await;
        }

        Ok(Self {
            started_at: Instant::now(),
            shutdown_tx,
            provider_revision_tx,
            event_tx,
            store,
            search,
            search_worker: tokio::sync::Mutex::new(Some(search_worker)),
            background_tasks: StdMutex::new(vec![event_log_worker]),
            token_cache: Arc::new(Mutex::new(None)),
            auth_sessions: crate::auth_sessions::AuthSessions::new(),
            provider_factory: Mutex::new(None),
            provider_config_snapshot: Mutex::new(provider_config_snapshot),
            player_settings: RwLock::new(player_settings),
            provider_build_lock: Mutex::new(()),
            provider_commit_lock: Mutex::new(()),
            providers: Mutex::new(ProviderRegistryCache::new(injected_providers)),
            active_transport_provider,
            embedded_provider_id,
            provider_topology,
            transport_mutation_lock: Arc::new(Mutex::new(())),
            library_mutation_lock: Arc::new(Mutex::new(())),
            operation_mutation_lock: Arc::new(Mutex::new(())),
            sync_locks: StdMutex::new(HashMap::new()),
            playlist_mutation_locks: Mutex::new(HashMap::new()),
            scope_reauth_emitted: std::sync::atomic::AtomicBool::new(false),
            auth_migration_emitted: std::sync::atomic::AtomicBool::new(false),
            auth_revoked: std::sync::atomic::AtomicBool::new(false),
            auth_required: std::sync::atomic::AtomicBool::new(false),
            schema_compat_seen: Arc::new(parking_lot::Mutex::new(HashSet::new())),
            own_device_name,
            own_device_volume,
            event_log,
            event_emitter,
            player_policy_events,
            first_party_bearer: Arc::new(parking_lot::Mutex::new(None)),
            player_tx,
            player_transport_tx,
            player_warm_tx,
            player_token_slot: token_slot,
            session_bearer: Arc::new(RwLock::new(None)),
            player_event_stream_tx,
            player_install_lock: Mutex::new(()),
            player_actor: tokio::sync::Mutex::new(Some(player_actor)),
            player_worker: tokio::sync::Mutex::new(Some(player_worker)),
            queue_warm,
            queue_warm_rx: StdMutex::new(Some(queue_warm_rx)),
            _session_tracker: session_tracker,
            system_integration,
            viz_coordinator,
            mutation_seq,
            playback_refresh_gate: RefreshGate::default(),
            queue_refresh_gate: RefreshGate::default(),
            devices_refresh_gate: RefreshGate::default(),
            rate_limit_notice_gates: parking_lot::Mutex::new(HashMap::new()),
            pending_queue_appends: Arc::new(parking_lot::Mutex::new(Vec::new())),
            we_are_active,
            reconnect_in_flight,
            player_health,
            audio_counter,
            latest_release: Arc::new(parking_lot::Mutex::new(None)),
            episode_feed: Arc::new(parking_lot::Mutex::new(HashMap::new())),
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

    /// Return the validated provider registry used by daemon clients.
    #[allow(dead_code)] // Stage A accessor; handler migration follows separately.
    pub(crate) async fn providers(
        &self,
    ) -> Result<Arc<crate::provider_registry::ProviderRegistry>> {
        {
            let cache = self.providers.lock().await;
            if cache.key == Some(ProviderRegistryKey::Injected) {
                let registry = cache
                    .registry
                    .clone()
                    .context("injected provider registry is missing")?;
                drop(cache);
                self.install_registry_player(&registry).await?;
                self.seed_playback_clock_from_cache(&registry).await;
                self.seed_active_transport_provider_from_cache(&registry);
                return Ok(registry);
            }
        }
        let _build_guard = self.provider_build_lock.lock().await;
        {
            let cache = self.providers.lock().await;
            if cache.key == Some(ProviderRegistryKey::Injected) {
                let registry = cache
                    .registry
                    .clone()
                    .context("injected provider registry is missing")?;
                drop(cache);
                self.install_registry_player(&registry).await?;
                self.seed_playback_clock_from_cache(&registry).await;
                self.seed_active_transport_provider_from_cache(&registry);
                return Ok(registry);
            }
        }
        loop {
            let key = self.current_provider_registry_key().await;

            let generation = {
                let cache = self.providers.lock().await;
                if cache.key.as_ref() == Some(&key) {
                    let registry = cache
                        .registry
                        .clone()
                        .context("provider registry cache is missing")?;
                    drop(cache);
                    self.install_registry_player(&registry).await?;
                    self.seed_playback_clock_from_cache(&registry).await;
                    self.seed_active_transport_provider_from_cache(&registry);
                    return Ok(registry);
                }
                cache.generation
            };

            let factory = self.shared_provider_factory().await?;
            let built = factory
                .build_default_registry(crate::provider_factory::ProviderBuildInputs {
                    config: self.provider_config_snapshot.lock().await.clone(),
                    auth: self.provider_auth_inputs(),
                    schema_compat_reporter: Arc::new(DaemonSchemaCompatReporter {
                        events: self.event_emitter.clone(),
                        seen: self.schema_compat_seen.clone(),
                    }),
                    player_token_slot: self.player_token_slot.clone(),
                    viz_analyzer: Some(self.viz_coordinator.shared_analyzer()),
                })
                .await?;
            // If player/runtime identity changed while auth/client creation was
            // in flight, discard this build and retry the current desired key.
            if self.current_provider_registry_key().await != key {
                continue;
            }
            let _commit_guard = self.provider_commit_lock.lock().await;
            if self.providers.lock().await.generation != generation {
                continue;
            }
            let registry = Arc::new(built.registry);
            let default_provider = registry.default_id().clone();
            *self.session_bearer.write() = built
                .session_bearer
                .as_ref()
                .filter(|(provider, _)| provider == &default_provider)
                .map(|(_, bearer)| bearer.clone());
            self.apply_provider_auth_outcome(built.auth, false, Some(&default_provider))
                .await?;
            if let Some(registry) = self
                .install_provider_registry(key, generation, registry)
                .await?
            {
                self.install_registry_player(&registry).await?;
                self.seed_playback_clock_from_cache(&registry).await;
                self.seed_active_transport_provider_from_cache(&registry);
                return Ok(registry);
            }
        }
    }

    fn seed_active_transport_provider_from_cache(
        &self,
        registry: &crate::provider_registry::ProviderRegistry,
    ) {
        if self.active_transport_provider().is_some() {
            return;
        }
        let playback = self.snapshot_playback();
        if playback.source != Some(spotuify_core::PlaybackStateSource::Cache) {
            return;
        }
        let Some(resource) = playback
            .item
            .as_ref()
            .and_then(|item| ResourceUri::parse(&item.uri).ok())
        else {
            return;
        };
        let Ok(runtime) = registry.provider_for_uri(&resource) else {
            return;
        };
        if runtime.transport().is_ok() {
            self.set_active_transport_provider(runtime.id().clone());
        }
    }

    async fn seed_playback_clock_from_cache(
        &self,
        registry: &crate::provider_registry::ProviderRegistry,
    ) {
        let current = self.playback_clock.snapshot();
        if current.item.is_some()
            || current.device.is_some()
            || current.is_playing
            || current.sampled_at_ms.is_some_and(|sampled| sampled != 0)
        {
            return;
        }
        let Ok(Some(cached)) = self.store.latest_playback().await else {
            return;
        };
        let Some(item) = cached.item.as_ref() else {
            return;
        };
        let Ok(uri) = ResourceUri::parse(&item.uri) else {
            return;
        };
        let Ok(runtime) = registry.provider_for_uri(&uri) else {
            tracing::debug!(
                uri = item.uri,
                "ignoring cached playback from removed provider"
            );
            return;
        };
        if runtime.transport().is_err() {
            tracing::debug!(
                uri = item.uri,
                "ignoring cached playback for transportless provider"
            );
            return;
        }
        self.playback_clock.seed_from_cache(
            cached,
            spotuify_core::PlaybackStateSource::Cache,
            spotuify_core::now_ms(),
        );
    }

    async fn install_registry_player(
        &self,
        registry: &crate::provider_registry::ProviderRegistry,
    ) -> Result<()> {
        let _install_guard = self.player_install_lock.lock().await;
        let Some(provider_id) = registry.embedded_player_provider_id().cloned() else {
            return Ok(());
        };
        if self.embedded_provider_id.read().as_ref() == Some(&provider_id) {
            return Ok(());
        }
        if let Some(installed) = self.embedded_provider_id.read().as_ref() {
            anyhow::bail!(
                "player provider changed from `{installed}` to `{provider_id}`; restart required"
            );
        }
        let runtime = registry.provider(&provider_id)?;
        let Some(player) = runtime.take_player()? else {
            anyhow::bail!(
                "provider player `{provider_id}` was consumed without completing installation"
            );
        };
        let crate::provider_registry::ProviderPlayer { backend, events } = player;
        let audio_counter = backend.audio_counter();
        let embedded_sink = audio_counter.is_some();
        let (resp, rx) = oneshot::channel();
        if let Err(error) = self
            .player_tx
            .send(PlayerCommand::Install { backend, resp })
            .await
        {
            let PlayerCommand::Install { backend, .. } = error.0 else {
                unreachable!("send error retained a different player command")
            };
            runtime.restore_player(crate::provider_registry::ProviderPlayer::new(
                backend, events,
            ))?;
            anyhow::bail!("player actor stopped before installation");
        }
        match rx
            .await
            .map_err(|error| anyhow::anyhow!("player actor stopped: {error}"))?
        {
            Ok(()) => {}
            Err((error, backend)) => {
                runtime.restore_player(crate::provider_registry::ProviderPlayer::new(
                    backend, events,
                ))?;
                return Err(error.into());
            }
        }
        // Publish the validated owner before handing the stream to the worker:
        // a backend may already have queued Ready/ProviderPolicy events, and
        // those must neither be dropped nor attributed to the configured
        // default. Roll this marker back if the worker handoff fails.
        *self.embedded_provider_id.write() = Some(provider_id.clone());
        if let Err(error) =
            self.player_event_stream_tx
                .send((events, embedded_sink, provider_id.clone()))
        {
            *self.embedded_provider_id.write() = None;
            let (events, _, _) = error.0;
            let rollback_error = "player event worker stopped";
            let (resp, rx) = oneshot::channel();
            self.player_tx
                .send(PlayerCommand::Uninstall { resp })
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "{rollback_error}; player actor also stopped during install rollback"
                    )
                })?;
            let backend = rx
                .await
                .map_err(|_| anyhow::anyhow!("{rollback_error}; rollback response was lost"))?
                .ok_or_else(|| {
                    anyhow::anyhow!("{rollback_error}; installed backend was not recoverable")
                })?;
            runtime.restore_player(crate::provider_registry::ProviderPlayer::new(
                backend, events,
            ))?;
            anyhow::bail!(rollback_error);
        }
        *self.audio_counter.write() = audio_counter.clone();
        self._session_tracker.set_audio_counter(audio_counter);
        Ok(())
    }

    async fn current_provider_registry_key(&self) -> ProviderRegistryKey {
        if std::env::var_os("SPOTUIFY_FAKE_SPOTIFY").is_some() {
            ProviderRegistryKey::Fake
        } else {
            ProviderRegistryKey::Configured
        }
    }

    async fn install_provider_registry(
        &self,
        key: ProviderRegistryKey,
        expected_generation: u64,
        registry: Arc<crate::provider_registry::ProviderRegistry>,
    ) -> Result<Option<Arc<crate::provider_registry::ProviderRegistry>>> {
        let replaced = {
            let mut cache = self.providers.lock().await;
            if cache.generation != expected_generation {
                return Ok(None);
            }
            if cache.key.as_ref() == Some(&key) {
                return Ok(Some(
                    cache
                        .registry
                        .clone()
                        .context("provider registry cache is missing")?,
                ));
            }
            let replaced = cache.registry.is_some();
            cache.key = Some(key);
            cache.registry = Some(registry.clone());
            replaced
        };
        let owner_changed = self.reconcile_active_transport_provider(&registry);
        if replaced || owner_changed {
            self.provider_revision_tx
                .send_modify(|revision| *revision = revision.wrapping_add(1));
        }
        Ok(Some(registry))
    }

    fn reconcile_active_transport_provider(
        &self,
        registry: &crate::provider_registry::ProviderRegistry,
    ) -> bool {
        let current = self.active_transport_provider();
        let current_is_valid = current.as_ref().is_some_and(|provider| {
            registry
                .provider(provider)
                .is_ok_and(|runtime| runtime.transport().is_ok())
        });
        let desired = if current_is_valid {
            current
        } else if registry.default_provider().transport().is_ok() {
            Some(registry.default_id().clone())
        } else {
            None
        };
        let changed = {
            let mut active = self.active_transport_provider.write();
            if *active == desired {
                false
            } else {
                *active = desired.clone();
                true
            }
        };
        if changed {
            if desired.as_ref() != self.embedded_provider_id.read().as_ref() {
                self.we_are_active.store(false, Ordering::Release);
            }
            // Invalidate transport polls issued under the removed adapter.
            self.bump_mutation_seq();
        }
        changed
    }

    /// Resolve an auth target from configured provider identity without
    /// touching the auth-gated registry construction path.
    pub(crate) async fn configured_auth_target(
        &self,
        requested: Option<&str>,
    ) -> Result<crate::provider_factory::ProviderAuthTarget> {
        let injected = {
            let cache = self.providers.lock().await;
            if cache.key == Some(ProviderRegistryKey::Injected) {
                Some(
                    cache
                        .registry
                        .clone()
                        .context("injected provider registry is missing")?,
                )
            } else {
                None
            }
        };
        if let Some(registry) = injected {
            let provider_id = match requested {
                Some(value) => {
                    ProviderId::new(value).map_err(|error| ProviderError::InvalidInput {
                        field: "provider".to_string(),
                        message: error.to_string(),
                    })?
                }
                None => registry.default_id().clone(),
            };
            registry
                .provider(&provider_id)
                .map_err(|_| ProviderError::InvalidInput {
                    field: "provider".to_string(),
                    message: format!("provider `{provider_id}` is not configured"),
                })?;
            return Ok(crate::provider_factory::ProviderAuthTarget {
                provider_id,
                strategy: crate::provider_factory::ProviderAuthStrategy::None,
            });
        }

        let config = self
            .provider_config_snapshot
            .lock()
            .await
            .clone()
            .context("accepted provider config snapshot is missing")?;
        crate::provider_factory::ProviderFactory::auth_target_from_config(&config, requested)
    }

    /// Resolve the provider whose credentials drive daemon auth health.
    /// Injected registries are deliberately self-contained/no-auth; normal
    /// configured registries may have a no-auth default plus one secondary
    /// Spotify adapter, which is the target that must be reloaded.
    pub(crate) async fn configured_health_auth_target(
        &self,
    ) -> Result<crate::provider_factory::ProviderAuthTarget> {
        let injected = self.providers.lock().await.key == Some(ProviderRegistryKey::Injected);
        if injected {
            self.configured_auth_target(None).await
        } else {
            let config = self
                .provider_config_snapshot
                .lock()
                .await
                .clone()
                .context("accepted provider config snapshot is missing")?;
            crate::provider_factory::ProviderFactory::health_auth_target_from_config(&config)
        }
    }

    pub(crate) async fn accepted_provider_config(&self) -> Result<spotuify_config::AppConfig> {
        self.provider_config_snapshot
            .lock()
            .await
            .clone()
            .context("accepted provider config snapshot is missing")
    }

    pub(crate) async fn auth_migration_advisory(&self, provider: &ProviderId) -> Option<bool> {
        let config = self.accepted_provider_config().await.ok()?;
        auth_migration_advisory(provider, &config)
    }

    #[allow(dead_code)]
    pub(crate) async fn default_provider(&self) -> Result<Arc<dyn MusicProvider>> {
        Ok(self.providers().await?.default_provider().music())
    }

    #[allow(dead_code)]
    pub(crate) async fn provider(
        &self,
        provider_id: &ProviderId,
    ) -> Result<Arc<dyn MusicProvider>> {
        let providers = self.providers().await?;
        Ok(providers.provider(provider_id)?.music())
    }

    pub(crate) async fn provider_or_default(
        &self,
        provider_id: Option<&ProviderId>,
    ) -> Result<(ProviderId, Arc<dyn MusicProvider>)> {
        let providers = self.providers().await?;
        let runtime = providers.provider_or_default(provider_id)?;
        Ok((runtime.id().clone(), runtime.music()))
    }

    #[allow(dead_code)]
    pub(crate) async fn provider_for_uri(
        &self,
        uri: &ResourceUri,
    ) -> Result<Arc<dyn MusicProvider>> {
        let providers = self.providers().await?;
        Ok(providers.provider_for_uri(uri)?.music())
    }

    #[allow(dead_code)]
    pub(crate) async fn provider_transport(
        &self,
        provider_id: &ProviderId,
    ) -> Result<Arc<dyn RemoteTransport>> {
        let providers = self.providers().await?;
        Ok(providers.provider(provider_id)?.transport()?)
    }

    pub(crate) fn active_transport_provider(&self) -> Option<ProviderId> {
        self.active_transport_provider.read().clone()
    }

    pub(crate) fn set_active_transport_provider(&self, provider: ProviderId) -> bool {
        let owns_embedded_player = self.embedded_provider_id.read().as_ref() == Some(&provider);
        let changed = {
            let mut active = self.active_transport_provider.write();
            if active.as_ref() == Some(&provider) {
                false
            } else {
                *active = Some(provider);
                true
            }
        };
        if changed {
            if !owns_embedded_player {
                self.we_are_active.store(false, Ordering::Release);
            }
            self.provider_revision_tx
                .send_modify(|revision| *revision = revision.wrapping_add(1));
        }
        changed
    }

    pub(crate) fn provider_owns_embedded_player(&self, provider: &ProviderId) -> bool {
        self.embedded_provider_id.read().as_ref() == Some(provider)
    }

    /// Whether this provider owns the shared credential slots used by the
    /// embedded player. Before the registry is first installed there is no
    /// runtime owner yet, so fall back to the validated startup topology.
    fn provider_owns_player_auth_slot(&self, provider: &ProviderId) -> bool {
        self.provider_owns_embedded_player(provider)
            || self
                .provider_topology
                .default_player
                .as_ref()
                .is_some_and(|(configured, _)| configured == provider)
    }

    pub(crate) fn has_embedded_player_provider(&self) -> bool {
        self.embedded_provider_id.read().is_some()
    }

    fn require_embedded_player_provider(&self) -> Result<ProviderId> {
        self.embedded_provider_id.read().clone().ok_or_else(|| {
            ProviderError::unsupported(
                "embedded player (configured default provider has no embedded transport)",
            )
            .into()
        })
    }

    #[allow(dead_code)]
    pub(crate) async fn default_transport(&self) -> Result<Arc<dyn RemoteTransport>> {
        Ok(self.providers().await?.default_provider().transport()?)
    }

    pub(crate) fn auth_sessions(&self) -> &crate::auth_sessions::AuthSessions {
        &self.auth_sessions
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

    fn transport_update_is_current(&self, provider: &ProviderId, captured_seq: u64) -> bool {
        self.may_apply_state_update(captured_seq)
            && self
                .active_transport_provider()
                .as_ref()
                .is_none_or(|active| active == provider)
    }

    pub(crate) async fn persist_fresh_queue(
        &self,
        provider: &ProviderId,
        queue: &Queue,
        captured_seq: u64,
    ) -> Result<bool> {
        let _guard = self.transport_mutation_lock.lock().await;
        if !self.transport_update_is_current(provider, captured_seq) {
            return Ok(false);
        }
        self.store.persist_provider_queue(provider, queue).await?;
        Ok(true)
    }

    pub(crate) async fn persist_fresh_devices(
        &self,
        provider: &ProviderId,
        devices: &[Device],
        captured_seq: u64,
    ) -> Result<bool> {
        let _guard = self.transport_mutation_lock.lock().await;
        if !self.transport_update_is_current(provider, captured_seq) {
            return Ok(false);
        }
        self.store
            .replace_provider_devices(provider, devices)
            .await?;
        Ok(true)
    }

    pub(crate) fn track_pending_queue_appends(
        &self,
        provider: &ProviderId,
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
                provider,
                live_uris,
                queued_items,
                added_at_ms,
            ));
    }

    pub(crate) fn overlay_pending_queue_appends(
        &self,
        provider: &ProviderId,
        queue: Queue,
        now_ms: i64,
    ) -> Queue {
        let (queue, _) = merge_queue_pending_appends(
            provider,
            queue,
            &mut self.pending_queue_appends.lock(),
            now_ms,
        );
        queue
    }

    pub(crate) async fn mutation_lane(&self, request: &Request) -> Option<Arc<Mutex<()>>> {
        match mutation_lane_kind(request) {
            Some(MutationLaneKind::Transport) => Some(self.transport_mutation_lock.clone()),
            Some(MutationLaneKind::Playlist) => {
                Some(self.playlist_lane("__all_playlist_mutations__").await)
            }
            Some(MutationLaneKind::Library) => Some(self.library_mutation_lock.clone()),
            Some(MutationLaneKind::Operation) => Some(self.operation_mutation_lock.clone()),
            None => None,
        }
    }

    async fn playlist_lane(&self, key: &str) -> Arc<Mutex<()>> {
        let mut lanes = self.playlist_mutation_locks.lock().await;
        lanes
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub(crate) async fn apply_runtime_config(
        &self,
        config: &spotuify_config::AppConfig,
    ) -> Result<()> {
        if ProviderTopology::from_config(config) != self.provider_topology {
            return Err(ProviderError::InvalidInput {
                field: "providers".to_string(),
                message: "provider topology or default provider changed; restart the daemon to apply this configuration"
                    .to_string(),
            }
            .into());
        }
        let incoming_player_settings = config
            .default_provider()
            .context("provider config does not select a default provider")?
            .player_settings()?;
        self.shared_provider_factory()
            .await?
            .validate_config(config)?;
        let _build_guard = self.provider_build_lock.lock().await;
        if incoming_player_settings.audio_output_device
            != self.player_settings.read().audio_output_device
        {
            return Err(ProviderError::InvalidInput {
                field: "player.audio_output_device".to_string(),
                message: "audio output changes must use `spotuify audio-output set` so the live player is rebound"
                    .to_string(),
            }
            .into());
        }
        apply_viz_config(&self.viz_coordinator, &config.viz).await;
        let auth_config_changed = {
            let current = self.provider_config_snapshot.lock().await;
            current.as_ref().is_some_and(|current| {
                current.path != config.path || current.providers != config.providers
            })
        };
        let _auth_guard = if auth_config_changed {
            Some(self.auth_sessions.config_reload_guard().await)
        } else {
            None
        };
        let _commit_guard = self.provider_commit_lock.lock().await;
        *self.provider_config_snapshot.lock().await = Some(config.clone());
        *self.token_cache.lock().await = None;
        let mut cache = self.providers.lock().await;
        if cache.key != Some(ProviderRegistryKey::Injected) {
            cache.generation = cache.generation.wrapping_add(1);
            cache.key = None;
            cache.registry = None;
            drop(cache);
            self.provider_revision_tx
                .send_modify(|revision| *revision = revision.wrapping_add(1));
        }
        Ok(())
    }

    /// Register the daemon's Connect device. Idempotent — calling
    /// twice with the same name is safe (backends short-circuit).
    /// Emits `DaemonEvent::PlayerReady` on success or `PlayerFailed`
    /// on terminal error (the event-forward task does the
    /// translation; we just propagate Result here).
    pub(crate) async fn ensure_player_ready(&self, name: &str) -> Result<DeviceId> {
        let _ = self.providers().await?;
        let provider = self.require_embedded_player_provider()?;
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
        let result = rx
            .await
            .map_err(|err| anyhow::anyhow!("player actor stopped: {err}"))?;
        let device_id = result.map_err(|error| player_request_error(error, provider))?;
        if self.audio_counter.read().is_some() {
            self.viz_coordinator.set_legacy_backend_label("embedded");
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
    /// Our embedded device is the live source iff the clock shows our
    /// own device id. While paused, polling `/me/player` every 3s is
    /// still redundant; slow reconciliation catches external handoff.
    /// True at most once a minute per domain — gates the
    /// `DaemonEvent::RateLimited` notice so cooldown-skipped refreshes
    /// don't spam subscribers on every attempt.
    pub(crate) fn should_notify_rate_limit(&self, domain: &str, now_ms: i64) -> bool {
        let mut gates = self.rate_limit_notice_gates.lock();
        let last = gates.get(domain).copied().unwrap_or(0);
        if now_ms.saturating_sub(last) < 60_000 {
            return false;
        }
        gates.insert(domain.to_string(), now_ms);
        true
    }

    pub(crate) fn embedded_owns_playback(&self) -> bool {
        if !self.embedded_owns_global_transport() {
            return false;
        }
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

    pub(crate) fn embedded_owns_global_transport(&self) -> bool {
        let embedded = self.embedded_provider_id.read();
        let active = self.active_transport_provider.read();
        matches!((embedded.as_ref(), active.as_ref()), (Some(embedded), Some(active)) if embedded == active)
    }

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
        let was_active = self.we_are_active.swap(active, Ordering::AcqRel);
        if active && !was_active {
            self.forgive_give_up_on_reactivation();
        }
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
    pub(crate) fn cached_episode_feed(&self, provider: &ProviderId) -> Option<CachedEpisodeFeed> {
        self.episode_feed.lock().get(provider).cloned()
    }

    /// Cache the merged episode feed with its fetch timestamp.
    pub(crate) fn set_cached_episode_feed(
        &self,
        provider: ProviderId,
        episodes: Vec<spotuify_core::MediaItem>,
        fetched_at_ms: i64,
    ) {
        self.episode_feed
            .lock()
            .insert(provider, (episodes, fetched_at_ms));
    }

    /// Whether an active device reported by Spotify is our own embedded
    /// device. Matches by id when Spotify provides one; falls back to the
    /// device *name* when the id is absent — car head units and other
    /// restricted Connect devices commonly report `device.id: null` in
    /// `/me/player`, and treating those as "unknown" left `we_are_active`
    /// stale-true, letting the audio-flow watchdog steal their playback
    /// (observed 2026-06-29: watchdog yanked a car session to the Mac).
    pub(crate) fn device_is_ours(&self, device: &spotuify_core::Device) -> bool {
        let own_name = self.own_device_name.lock().clone();
        device_matches_own(device, self.own_device_id().as_deref(), own_name.as_deref())
    }

    /// Whether the snapshot names an active device that is *not* ours.
    /// `device == None` (unknown) is not provably foreign → `false`.
    pub(crate) fn active_device_is_foreign(&self, playback: &spotuify_core::Playback) -> bool {
        playback
            .device
            .as_ref()
            .is_some_and(|device| !self.device_is_ours(device))
    }

    /// Reconcile `we_are_active` against an authoritative playback snapshot: set
    /// when our own device is the active one, clear when a *different* device is
    /// active (the user handed off — e.g. to their phone or car). Leaves the
    /// flag unchanged when no device is active, to avoid flapping during
    /// silence. Devices without an id are matched by name (see
    /// [`Self::device_is_ours`]) so an id-less hand-off still clears the flag.
    pub(crate) fn note_active_device(&self, playback: &spotuify_core::Playback) {
        let Some(device) = playback.device.as_ref() else {
            return;
        };
        if self.device_is_ours(device) {
            let was_active = self.we_are_active.swap(true, Ordering::AcqRel);
            if !was_active {
                self.forgive_give_up_on_reactivation();
            }
        } else {
            self.we_are_active.store(false, Ordering::Release);
        }
    }

    /// The own-device row for device lists: the live entry when the
    /// embedded player is connected, otherwise an inactive entry
    /// synthesized from the embedded device's known name. The embedded
    /// device must stay visible (and targetable) while the player idles
    /// after a session drop — the post-drop policy deliberately does not
    /// auto-reconnect while another device is active, and without this
    /// entry no client could transfer playback back without a manual
    /// `spotuify reconnect`. The transfer handler reconnects on demand
    /// when this device is picked. Uses `own_device_name` (not the config
    /// fallback) so its id/name always match [`Self::device_is_ours`];
    /// that name is seeded at daemon construction in embedded builds and
    /// is `None` only when there is no embedded device.
    pub(crate) async fn own_device_entry(&self) -> Option<Device> {
        if let Some(device) = self.connected_own_device().await {
            return Some(device);
        }
        let name = self.own_device_name.lock().clone()?;
        Some(Device {
            id: Some(derive_device_id_for_name(&name)),
            name,
            kind: "Speaker".to_string(),
            is_active: false,
            is_restricted: false,
            volume_percent: *self.own_device_volume.lock(),
            supports_volume: true,
        })
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

    pub(crate) fn accepted_player_settings(&self) -> spotuify_config::PlayerSettings {
        self.player_settings.read().clone()
    }

    pub(crate) fn configured_device_name(&self) -> String {
        self.player_settings.read().effective_device_name()
    }

    pub(crate) async fn reconnect_player(&self, name: &str) -> Result<DeviceId> {
        let _ = self.providers().await?;
        let provider = self.require_embedded_player_provider()?;
        // A manual reconnect is the user's explicit "I want this device now":
        // forgive any prior give-up so the health-loop backstop resumes even if
        // this particular attempt fails.
        if self.reset_give_up() {
            tracing::info!("manual reconnect cleared prior player reconnect give-up");
        }
        *self.own_device_name.lock() = Some(name.to_string());
        let (resp, rx) = oneshot::channel();
        self.player_tx
            .send(PlayerCommand::Reconnect {
                name: name.to_string(),
                resume: None,
                resp,
            })
            .await
            .map_err(|err| anyhow::anyhow!("player actor stopped: {err}"))?;
        let result = rx
            .await
            .map_err(|err| anyhow::anyhow!("player actor stopped: {err}"))?;
        let device_id = result.map_err(|error| player_request_error(error, provider))?;
        if self.audio_counter.read().is_some() {
            self.viz_coordinator.set_legacy_backend_label("embedded");
            self.viz_coordinator.set_sink_available(true).await;
        }
        Ok(device_id)
    }

    /// Update the player backend's local audio output selection. Takes
    /// effect on the next reconnect (the sink chain is rebuilt then), so
    /// callers pair this with `reconnect_player`.
    pub(crate) async fn set_player_audio_output(&self, device: Option<String>) -> Result<()> {
        self.set_player_audio_output_with_persistence(device, |path, persisted| {
            spotuify_config::set_config_value(&path, persisted.as_deref().unwrap_or(""))
                .map_err(anyhow::Error::from)
        })
        .await
    }

    async fn set_player_audio_output_with_persistence<F>(
        &self,
        device: Option<String>,
        persist: F,
    ) -> Result<()>
    where
        F: FnOnce(spotuify_config::ConfigPath, Option<String>) -> Result<()> + Send + 'static,
    {
        self.set_player_audio_output_with_persistence_and_faults(
            device,
            persist,
            AudioOutputFaults::default(),
        )
        .await
    }

    #[cfg_attr(test, allow(clippy::panic))]
    async fn set_player_audio_output_with_persistence_and_faults<F>(
        &self,
        device: Option<String>,
        persist: F,
        faults: AudioOutputFaults,
    ) -> Result<()>
    where
        F: FnOnce(spotuify_config::ConfigPath, Option<String>) -> Result<()> + Send + 'static,
    {
        #[cfg(not(test))]
        let _ = faults;
        let _build_guard = self.provider_build_lock.lock().await;
        let previous = self.player_settings.read().audio_output_device.clone();
        let mut updated_config = self.provider_config_snapshot.lock().await.clone();
        if let Some(config) = updated_config.as_mut() {
            config.set_default_player_audio_output(device.clone())?;
        }
        let persistence_path = updated_config
            .as_ref()
            .map(|config| {
                let provider_id = config
                    .default_provider
                    .as_ref()
                    .context("accepted provider config has no default provider")?;
                spotuify_config::ConfigPath::parse(&format!(
                    "providers.{provider_id}.player.audio_output_device"
                ))
                .map_err(anyhow::Error::from)
            })
            .transpose()?;
        self.apply_player_audio_output(device.clone()).await?;
        let persistence_error = if let Some(path) = persistence_path.as_ref() {
            let path = path.clone();
            let persisted = device.clone();
            match tokio::task::spawn_blocking(move || persist(path, persisted)).await {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(format!("failed to persist audio output: {error}")),
                Err(error) => Some(format!("audio output persistence task failed: {error}")),
            }
        } else {
            None
        };
        if let Some(error) = persistence_error {
            let recovery = match persistence_path.clone() {
                Some(path) => {
                    let rollback_value = previous.clone();
                    let recovery_task = tokio::task::spawn_blocking(move || {
                        #[cfg(test)]
                        if faults.recovery_task_panics {
                            std::panic::panic_any("injected audio output recovery task panic");
                        }
                        #[cfg(test)]
                        if faults.recovery_preserves_disk {
                            return (
                                Err(anyhow::anyhow!("injected audio output rollback failure")),
                                read_player_audio_output(),
                            );
                        }
                        rollback_and_read_player_audio_output(path, rollback_value)
                    })
                    .await;
                    match recovery_task {
                        Ok((rollback, observed)) => (rollback, observed, None),
                        Err(join) => {
                            let fallback_path = match persistence_path.clone() {
                                Some(path) => path,
                                None => {
                                    return Err(anyhow::anyhow!(
                                        "{error}; recovery task failed without a persistence path: {join}"
                                    ));
                                }
                            };
                            let fallback_previous = previous.clone();
                            #[cfg(test)]
                            let fallback_preserves_disk = faults.recovery_preserves_disk;
                            #[cfg(not(test))]
                            let fallback_preserves_disk = false;
                            match tokio::task::spawn_blocking(move || {
                                if fallback_preserves_disk {
                                    (
                                        Err(anyhow::anyhow!(
                                            "injected fallback audio output rollback failure"
                                        )),
                                        read_player_audio_output(),
                                    )
                                } else {
                                    rollback_and_read_player_audio_output(
                                        fallback_path,
                                        fallback_previous,
                                    )
                                }
                            })
                            .await
                            {
                                Ok((rollback, observed)) => (
                                    rollback.map_err(|rollback| {
                                        anyhow::anyhow!(
                                            "audio output recovery task failed ({join}); fallback rollback failed: {rollback}"
                                        )
                                    }),
                                    observed,
                                    Some(format!(
                                        "initial recovery task failed ({join}); fallback recovery ran"
                                    )),
                                ),
                                Err(fallback_join) => {
                                    let actor_rollback =
                                        self.apply_player_audio_output(previous.clone()).await;
                                    return Err(anyhow::anyhow!(
                                        "{error}; audio output recovery task failed: {join}; fallback recovery task failed: {fallback_join}; actor rollback: {}",
                                        actor_rollback.err().map_or_else(
                                            || "ok".to_string(),
                                            |failure| failure.to_string()
                                        )
                                    ));
                                }
                            }
                        }
                    }
                }
                None => (Ok(()), Ok(previous.clone()), None),
            };
            let (rollback, observed, recovery_issue) = recovery;
            let observed = match observed {
                Ok(observed) => observed,
                Err(read_error) => {
                    let actor_rollback = self.apply_player_audio_output(previous).await;
                    return Err(anyhow::anyhow!(
                        "{error}; could not verify disk after rollback: {read_error}; actor rollback: {}",
                        actor_rollback
                            .err()
                            .map_or_else(|| "ok".to_string(), |failure| failure.to_string())
                    ));
                }
            };
            let rolled_back = observed == previous;
            let mut reconciled_config = self.provider_config_snapshot.lock().await.clone();
            if let Some(config) = reconciled_config.as_mut() {
                config.set_default_player_audio_output(observed.clone())?;
            }
            *self.provider_config_snapshot.lock().await = reconciled_config;
            self.player_settings.write().audio_output_device = observed.clone();
            let mut rollback_note = match (rollback, rolled_back) {
                (Ok(()), true) => "rollback verified".to_string(),
                (Ok(()), false) => "rollback write succeeded but disk retained another value; runtime reconciled to disk".to_string(),
                (Err(failure), _) => format!("rollback write failed ({failure}); runtime reconciled to disk"),
            };
            if let Some(recovery_issue) = recovery_issue {
                rollback_note.push_str(&format!("; {recovery_issue}"));
            }
            #[cfg(test)]
            let actor_reconcile = if faults.reconcile_actor_fails {
                Err(anyhow::anyhow!(
                    "injected audio output actor reconciliation failure"
                ))
            } else {
                self.apply_player_audio_output(observed.clone()).await
            };
            #[cfg(not(test))]
            let actor_reconcile = self.apply_player_audio_output(observed.clone()).await;
            if let Err(actor_error) = actor_reconcile {
                return Err(anyhow::anyhow!(
                    "{error}; disk reconciliation: {rollback_note}; accepted state reconciled to disk but player reconciliation failed: {actor_error}"
                ));
            }
            anyhow::bail!("{error}; disk reconciliation: {rollback_note}");
        }
        *self.provider_config_snapshot.lock().await = updated_config;
        self.player_settings.write().audio_output_device = device;
        Ok(())
    }

    async fn apply_player_audio_output(&self, device: Option<String>) -> Result<()> {
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
        // "Wants this device" = tracked-active OR the clock shows we were
        // playing on our own device (robust when `we_are_active` lags). The
        // resume target is reused below so a backstop reconnect also continues
        // playback when the clock is still fresh enough to know the position.
        let own_name = self.own_device_name.lock().clone();
        let owns_global_transport = self.embedded_owns_global_transport();
        let resume = owns_global_transport
            .then(|| {
                resume_target_after_drop(
                    &self.playback_clock.snapshot(),
                    self.own_device_id().as_deref(),
                    own_name.as_deref(),
                )
            })
            .flatten();
        let active = owns_global_transport && (self.is_we_are_active() || resume.is_some());
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
            // Back off against the failure count (decided BEFORE this failure,
            // matching the give-up convention) so repeated drops space out.
            let backoff = reconnect_backoff(snapshot.consecutive_failures.saturating_sub(1));
            {
                let mut health = self.player_health.lock();
                health.reconnect_attempts = health.reconnect_attempts.saturating_add(1);
                health.current_backoff_ms = backoff.as_millis() as u64;
            }
            tracing::warn!(
                consecutive_failures = snapshot.consecutive_failures,
                backoff_ms = backoff.as_millis() as u64,
                "player session is down while active; auto-reconnecting"
            );
            let device_name = own_name.unwrap_or_else(|| {
                spotuify_config::PlayerSettings::default().effective_device_name()
            });
            schedule_player_reconnect(
                self.player_tx.clone(),
                self.reconnect_in_flight.clone(),
                resume,
                backoff,
                device_name,
            );
        }
        snapshot
    }

    /// Current player-session health snapshot for diagnostics.
    pub(crate) fn player_health_snapshot(&self) -> PlayerHealth {
        *self.player_health.lock()
    }

    /// Forgive a prior reconnect give-up: clear `gave_up` and zero the
    /// consecutive-failure count so the health-loop backstop
    /// ([`should_auto_reconnect_player`]) resumes. Without this the daemon
    /// latches permanently — once `consecutive_failures` hits
    /// [`PLAYER_RECONNECT_GIVE_UP_AFTER`] it stops reconnecting and the count
    /// only ever grows, so it never auto-recovers even when the user clearly
    /// still wants the device. Called on explicit fresh intent (a manual
    /// `reconnect`, or `we_are_active` transitioning to active). Returns whether
    /// a give-up was actually cleared, for logging.
    pub(crate) fn reset_give_up(&self) -> bool {
        let mut health = self.player_health.lock();
        let cleared = health.gave_up;
        health.gave_up = false;
        health.consecutive_failures = 0;
        cleared
    }

    /// Clear a prior give-up when the user (re)activates this device, logging
    /// only when something was actually latched.
    fn forgive_give_up_on_reactivation(&self) {
        if self.reset_give_up() {
            tracing::info!(
                "device re-activated; resuming player auto-reconnect after prior give-up"
            );
        }
    }

    /// Total PCM samples the embedded sink has emitted (ground truth that audio
    /// is flowing). `None` for non-embedded backends → watchdog stays inert.
    pub(crate) fn audio_samples(&self) -> Option<u64> {
        self.audio_counter.read().as_ref().map(|c| c.samples())
    }

    /// Record an audio-flow watchdog observation into `PlayerHealth` for
    /// diagnostics. Locks only `player_health` (no clock lock held) to keep
    /// the existing lock ordering.
    pub(crate) fn record_audio_flow(&self, advancing: bool, stalled_at_ms: Option<i64>) {
        let mut health = self.player_health.lock();
        health.samples_advancing = advancing;
        if stalled_at_ms.is_some() {
            health.last_stall_ms = stalled_at_ms;
        }
    }

    /// Recovery path for the audio-flow watchdog: the session looks connected
    /// (TCP alive) but no PCM is flowing, so `probe_player_health` (which keys
    /// off connectivity) won't act. Reconnect through the shared throttle when
    /// we still want this device, resuming where it stalled. Returns whether a
    /// reconnect was scheduled.
    pub(crate) fn trigger_audio_stall_recovery(&self, now_ms: i64) -> bool {
        if !self.embedded_owns_global_transport() {
            return false;
        }
        let own_name = self.own_device_name.lock().clone();
        let resume = resume_target_after_drop(
            &self.playback_clock.snapshot(),
            self.own_device_id().as_deref(),
            own_name.as_deref(),
        );
        if !(self.is_we_are_active() || resume.is_some()) {
            return false;
        }
        if self.reconnect_in_flight.load(Ordering::Acquire) {
            return false;
        }
        let (failures, backoff) = {
            let mut health = self.player_health.lock();
            health.last_reconnect_ms = Some(now_ms);
            health.reconnect_attempts = health.reconnect_attempts.saturating_add(1);
            let backoff = reconnect_backoff(health.consecutive_failures);
            health.current_backoff_ms = backoff.as_millis() as u64;
            (health.consecutive_failures, backoff)
        };
        tracing::warn!(
            consecutive_failures = failures,
            backoff_ms = backoff.as_millis() as u64,
            "audio stalled while playing; recovering embedded player"
        );
        let device_name = own_name
            .unwrap_or_else(|| spotuify_config::PlayerSettings::default().effective_device_name());
        schedule_player_reconnect(
            self.player_tx.clone(),
            self.reconnect_in_flight.clone(),
            resume,
            backoff,
            device_name,
        );
        true
    }

    /// Record the playback context (playlist/album/artist URI) the next
    /// started track plays from, for playlist-level listen analytics.
    pub(crate) fn set_playback_context(&self, context_uri: Option<String>) {
        self._session_tracker.set_current_context(context_uri);
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
        let result = rx.await.unwrap_or_else(|_| {
            Err(spotuify_player::PlayerError::Playback(
                "player actor stopped".to_string(),
            ))
        });
        result
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
            Ok(Ok(result)) => result.map(|()| FastTransportStatus::Applied),
            Ok(Err(_)) => Err(spotuify_player::PlayerError::Playback(
                "player actor stopped".to_string(),
            )),
            Err(_) => Ok(FastTransportStatus::Dispatched { ack: rx }),
        }
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

    /// SQLite queue snapshots are always marked inactive because the store
    /// cannot attest to live session state. While playback is live or inside
    /// the clock's no-session confirmation window, keep cached queue content
    /// renderable for QueueGet, ClientSeed, and subscribe snapshots. Once the
    /// clock confirms durable inactivity it switches to RecentFallback and
    /// the inactive bit is allowed through.
    pub(crate) fn queue_snapshot_for_clients(&self, mut queue: Queue) -> Queue {
        let has_content = queue.currently_playing.is_some() || !queue.items.is_empty();
        let durably_inactive = matches!(
            self.snapshot_playback().source,
            Some(spotuify_core::PlaybackStateSource::RecentFallback)
        );
        if has_content && !durably_inactive {
            queue.session_active = true;
        }
        queue
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

    pub(crate) fn auth_gate_error(&self) -> Option<ProviderError> {
        if self.auth_revoked() {
            Some(ProviderError::AuthRevoked)
        } else if self.auth_required() {
            Some(ProviderError::AuthRequired)
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

        let _build_guard = self.provider_build_lock.lock().await;
        let target = self.configured_health_auth_target().await?;
        if target.strategy == crate::provider_factory::ProviderAuthStrategy::None {
            return Ok(());
        }
        let provider = target.provider_id;

        if let Some(err) = self.auth_gate_error() {
            let credential_provider = provider.to_string();
            let credentials_present = tokio::task::spawn_blocking(move || {
                spotuify_spotify::auth::credential_inventory_for(&credential_provider)
                    .map(|inventory| inventory.dev_app.is_some() || inventory.first_party.is_some())
            })
            .await
            .context("auth health credential inventory task failed")??;
            if !credentials_present {
                return Err(anyhow::Error::new(err));
            }
            self.clear_auth_gate_for_disk_recovery(&provider).await;
        }

        let generation = self.providers.lock().await.generation;
        let config = self
            .provider_config_snapshot
            .lock()
            .await
            .clone()
            .context("accepted provider config snapshot is missing")?;
        let outcome = self
            .shared_provider_factory()
            .await?
            .probe_auth(&config, self.provider_auth_inputs(), Some(&provider))
            .await?;
        let _commit_guard = self.provider_commit_lock.lock().await;
        if self.providers.lock().await.generation != generation {
            tracing::debug!("discarding stale provider auth-health probe");
            return Ok(());
        }
        self.apply_provider_auth_outcome(outcome, true, Some(&provider))
            .await
    }

    async fn shared_provider_factory(&self) -> Result<crate::provider_factory::ProviderFactory> {
        let mut cached = self.provider_factory.lock().await;
        if let Some(factory) = cached.as_ref() {
            return Ok(factory.clone());
        }
        let factory = crate::provider_factory::ProviderFactory::new()?;
        *cached = Some(factory.clone());
        Ok(factory)
    }

    fn provider_auth_inputs(&self) -> crate::provider_factory::ProviderAuthInputs {
        #[cfg(feature = "embedded-playback")]
        let first_party_bearer = self
            .session_bearer
            .read()
            .clone()
            .and_then(|session_bearer| {
                self.embedded_provider_id
                    .read()
                    .as_ref()
                    .map(|provider_id| {
                        (provider_id.clone(), {
                            Arc::new(FirstPartyBearerProvider {
                                provider_id: provider_id.to_string(),
                                session_bearer: session_bearer.clone(),
                                token_slot: self.player_token_slot.clone(),
                                cache: self.first_party_bearer.clone(),
                            })
                                as Arc<dyn spotuify_spotify::WebApiBearerProvider>
                        })
                    })
            });
        #[cfg(not(feature = "embedded-playback"))]
        let first_party_bearer = None;

        crate::provider_factory::ProviderAuthInputs {
            token_cache: self.token_cache.clone(),
            first_party_bearer,
        }
    }

    async fn apply_provider_auth_outcome(
        &self,
        outcome: crate::provider_factory::ProviderAuthOutcome,
        strict: bool,
        provider: Option<&ProviderId>,
    ) -> Result<()> {
        use crate::provider_factory::ProviderAuthOutcome;

        match outcome {
            ProviderAuthOutcome::NotRequired => Ok(()),
            ProviderAuthOutcome::Authenticated {
                access_token,
                first_party,
            } => {
                if !first_party
                    && provider.is_none_or(|provider| self.provider_owns_player_auth_slot(provider))
                {
                    self.update_player_token(Some(access_token));
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
                    if emit_scope_reauth_event_if_needed(
                        cached.as_ref(),
                        &self.event_tx,
                        provider.cloned(),
                    ) {
                        tracing::info!(
                            "stored Spotify token is missing required scopes; emitted ScopeReauthRequired event"
                        );
                    }
                }
                if first_party
                    && !self
                        .auth_migration_emitted
                        .load(std::sync::atomic::Ordering::Acquire)
                    && emit_auth_migration_event_if_needed(
                        match provider {
                            Some(provider) => self.auth_migration_advisory(provider).await,
                            None => None,
                        },
                        &self.auth_migration_emitted,
                        &self.event_tx,
                    )
                {
                    tracing::info!(
                        "resolved to first-party-only auth; emitted AuthMigrationRecommended advisory"
                    );
                }
                Ok(())
            }
            ProviderAuthOutcome::Unavailable { error, first_party } => match error {
                ProviderError::AuthRevoked => {
                    self.mark_auth_revoked(&ProviderError::AuthRevoked, provider)
                        .await;
                    if strict {
                        Err(ProviderError::AuthRevoked.into())
                    } else {
                        Ok(())
                    }
                }
                ProviderError::AuthRequired => {
                    self.mark_auth_required(provider).await;
                    if strict {
                        Err(ProviderError::AuthRequired.into())
                    } else {
                        Ok(())
                    }
                }
                error if strict => Err(error.into()),
                error => {
                    tracing::debug!(
                        error = %error,
                        first_party,
                        "provider auth probe unavailable for player bridge"
                    );
                    Ok(())
                }
            },
        }
    }

    pub(crate) async fn mark_auth_revoked(
        &self,
        err: &ProviderError,
        provider: Option<&ProviderId>,
    ) {
        let first = !self
            .auth_revoked
            .swap(true, std::sync::atomic::Ordering::AcqRel);
        self.auth_required
            .store(false, std::sync::atomic::Ordering::Release);
        {
            let mut cache = self.token_cache.lock().await;
            *cache = None;
        }
        if provider.is_none_or(|provider| self.provider_owns_player_auth_slot(provider)) {
            self.update_player_token(None);
        }

        if first {
            tracing::warn!(
                error = %err,
                error_chain = ?err,
                "Spotify refresh token revoked — emitting AuthError(InvalidGrant); re-login required"
            );
            self.emit_event(DaemonEvent::AuthError {
                kind: spotuify_protocol::AuthErrorKind::InvalidGrant,
                provider: provider.cloned(),
            });
        }
    }

    pub(crate) async fn mark_auth_required(&self, provider: Option<&ProviderId>) {
        let first = !self
            .auth_required
            .swap(true, std::sync::atomic::Ordering::AcqRel);
        {
            let mut cache = self.token_cache.lock().await;
            *cache = None;
        }
        if provider.is_none_or(|provider| self.provider_owns_player_auth_slot(provider)) {
            self.update_player_token(None);
        }

        if first {
            tracing::warn!("Spotify credentials missing — emitting AuthError(NotLoggedIn)");
            self.emit_event(DaemonEvent::AuthError {
                kind: spotuify_protocol::AuthErrorKind::NotLoggedIn,
                provider: provider.cloned(),
            });
        }
    }

    /// Drop the daemon's in-memory token cache and clear the
    /// `auth_revoked` latch so the next provider construction re-reads fresh
    /// credentials from the auth file. Called by daemon-owned auth session
    /// completion and by the compatibility `Request::ReloadAuth` handler.
    ///
    /// Idempotent — calling this when no token is cached and no latch
    /// is set is a no-op.
    pub(crate) async fn reload_auth(&self, requested: Option<&ProviderId>) -> Result<()> {
        let target = self
            .configured_auth_target(requested.map(ProviderId::as_str))
            .await?;
        let requires_auth =
            target.strategy == crate::provider_factory::ProviderAuthStrategy::SpotifyOauth;
        let owns_embedded_player = self.provider_owns_embedded_player(&target.provider_id);
        let owns_player_auth_slot = self.provider_owns_player_auth_slot(&target.provider_id);
        let provider = target.provider_id.to_string();
        let credentials_present = if requires_auth {
            let inventory = tokio::task::spawn_blocking(move || {
                spotuify_spotify::auth::credential_inventory_for(&provider)
            })
            .await
            .context("auth reload status task failed")??;
            inventory.dev_app.is_some() || inventory.first_party.is_some()
        } else {
            false
        };
        self.invalidate_provider_registry().await;
        if !requires_auth {
            return Ok(());
        }
        {
            let mut cache = self.token_cache.lock().await;
            *cache = None;
        }
        if owns_player_auth_slot {
            self.update_player_token(None);
        }
        // Drop the cached first-party bearer so a logout isn't papered
        // over by the short bearer-cache TTL.
        if owns_player_auth_slot {
            self.first_party_bearer.lock().take();
        }
        self.auth_revoked
            .store(false, std::sync::atomic::Ordering::Release);
        self.auth_required.store(
            requires_auth && !credentials_present,
            std::sync::atomic::Ordering::Release,
        );
        // If all credentials are now gone (logout), tear down the live
        // librespot session so it can't keep minting login5 bearers from
        // its in-memory connection until the next daemon restart.
        if owns_embedded_player && !credentials_present {
            self.drop_player_session().await?;
        }
        Ok(())
    }

    /// Complete daemon-owned logout after the credential store has been
    /// atomically purged. Player shutdown is bounded and failures propagate so
    /// callers never receive a false-success logout receipt.
    pub(crate) async fn finish_logout(&self, provider: &ProviderId) -> Result<()> {
        let owns_embedded_player = self.provider_owns_embedded_player(provider);
        let owns_player_auth_slot = self.provider_owns_player_auth_slot(provider);
        self.invalidate_provider_registry().await;
        {
            let mut cache = self.token_cache.lock().await;
            *cache = None;
        }
        if owns_player_auth_slot {
            self.update_player_token(None);
            self.first_party_bearer.lock().take();
        }
        self.auth_revoked
            .store(false, std::sync::atomic::Ordering::Release);
        self.mark_auth_required(Some(provider)).await;
        if owns_embedded_player {
            self.drop_player_session().await?;
        }
        Ok(())
    }

    async fn clear_auth_gate_for_disk_recovery(&self, provider: &ProviderId) {
        let owns_player_auth_slot = self.provider_owns_player_auth_slot(provider);
        self.invalidate_provider_registry().await;
        {
            let mut cache = self.token_cache.lock().await;
            *cache = None;
        }
        if owns_player_auth_slot {
            self.update_player_token(None);
            self.first_party_bearer.lock().take();
        }
        self.auth_revoked
            .store(false, std::sync::atomic::Ordering::Release);
        self.auth_required
            .store(false, std::sync::atomic::Ordering::Release);
    }

    async fn invalidate_provider_registry(&self) {
        let _commit_guard = self.provider_commit_lock.lock().await;
        let mut cache = self.providers.lock().await;
        if cache.key != Some(ProviderRegistryKey::Injected) {
            cache.generation = cache.generation.wrapping_add(1);
            cache.key = None;
            cache.registry = None;
            drop(cache);
            self.provider_revision_tx
                .send_modify(|revision| *revision = revision.wrapping_add(1));
        }
    }

    /// Shut down the embedded librespot session (without stopping the
    /// player actor) so it stops minting from cached credentials. The
    /// next playback command re-registers the device from fresh creds.
    async fn drop_player_session(&self) -> Result<()> {
        let (resp, rx) = oneshot::channel();
        tokio::time::timeout(
            Duration::from_secs(5),
            self.player_tx.send(PlayerCommand::DropSession { resp }),
        )
        .await
        .context("timed out sending player session drop")?
        .context("player actor unavailable during logout")?;
        let result = tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .context("timed out waiting for player session drop")?
            .context("player session drop response channel closed")?;
        result.map_err(|error| {
            anyhow::anyhow!(
                "player session drop failed: {}",
                player_error_for_display(&error)
            )
        })?;
        Ok(())
    }

    pub(crate) fn emit_event(&self, event: DaemonEvent) {
        self.event_emitter.emit(event);
    }

    pub(crate) async fn recover_pending_receipts_after_startup(&self) -> Result<usize> {
        recover_pending_receipts(&self.store, &self.event_tx, spotuify_core::now_ms()).await
    }

    /// Phase 6.9 — snapshot of the event ring for doctor reporting.
    pub(crate) async fn event_log_snapshot(&self) -> Vec<spotuify_protocol::LoggedEvent> {
        self.event_log.snapshot().await
    }

    pub(crate) fn active_provider_policies(&self) -> Vec<spotuify_protocol::ProviderPolicyNotice> {
        self.player_policy_events.active()
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
            // Only the embedded backend owns the sink counter. `audio_samples()`
            // is `None` for other backends, leaving `audio_health` `None`.
            audio_health: self.audio_samples().map(|_| {
                let health = self.player_health_snapshot();
                spotuify_protocol::AudioHealth {
                    connected: health.connected,
                    is_playing: self.playback_clock.snapshot().is_playing,
                    we_are_active: self.is_we_are_active(),
                    samples_advancing: health.samples_advancing,
                    reconnect_attempts: health.reconnect_attempts,
                    current_backoff_ms: health.current_backoff_ms,
                    last_stall_ms: health.last_stall_ms,
                }
            }),
        }
    }

    /// Mint a first-party Web API bearer for a CLI-direct client (doctor,
    /// onboarding's initial sync) over IPC — those processes have no
    /// librespot session, so only the daemon can mint in first-party
    /// mode. Returns `None` in legacy mode or when the daemon can't mint
    /// (not logged in / no session). `force` re-mints after a 401.
    pub(crate) async fn web_api_bearer(&self, force: bool) -> Option<String> {
        if self.auth_required() || self.auth_revoked() {
            return None;
        }
        let _ = force;
        #[cfg(feature = "embedded-playback")]
        {
            use spotuify_spotify::WebApiBearerProvider;
            let provider_id = self.embedded_provider_id.read().clone()?;
            let session_bearer = self.session_bearer.read().clone()?;
            let provider = FirstPartyBearerProvider {
                provider_id: provider_id.to_string(),
                session_bearer,
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
}

#[derive(Debug, PartialEq, Eq)]
enum MutationLaneKind {
    Transport,
    Playlist,
    Library,
    Operation,
}

fn mutation_lane_kind(request: &Request) -> Option<MutationLaneKind> {
    match request {
        Request::PlaybackCommand { .. }
        | Request::DeviceTransfer { .. }
        | Request::QueueAdd { .. }
        | Request::QueueAddMany { .. } => Some(MutationLaneKind::Transport),
        Request::RadioStart { dry_run, .. } if !dry_run => Some(MutationLaneKind::Transport),
        Request::PlaylistAddItems { .. }
        | Request::PlaylistRemoveItems { .. }
        | Request::PlaylistTracks { .. }
        | Request::PlaylistUnfollow { .. }
        | Request::PlaylistSetImage { .. } => Some(MutationLaneKind::Playlist),
        Request::PlaylistCreate { .. } => Some(MutationLaneKind::Playlist),
        Request::LibrarySave { .. }
        | Request::LibraryUnsave { .. }
        | Request::ArtistFollow { .. }
        | Request::ArtistUnfollow { .. } => Some(MutationLaneKind::Library),
        Request::OpsUndo { .. } | Request::OpsRedo { .. } => Some(MutationLaneKind::Operation),
        _ => None,
    }
}

#[cfg(test)]
mod mutation_lane_tests {
    #![allow(clippy::panic, clippy::unwrap_used)]

    use super::{mutation_lane_kind, MutationLaneKind};
    use spotuify_protocol::Request;

    #[test]
    fn protected_mutations_use_their_serialization_lane() {
        let playlist = "spotify:playlist:1".to_string();
        let cases = [
            (
                Request::QueueAddMany {
                    uris: vec!["spotify:track:1".into()],
                },
                MutationLaneKind::Transport,
            ),
            (
                Request::RadioStart {
                    seed_uri: "spotify:track:seed".into(),
                    dry_run: false,
                },
                MutationLaneKind::Transport,
            ),
            (
                Request::PlaylistUnfollow {
                    playlist: playlist.clone(),
                    provider: None,
                },
                MutationLaneKind::Playlist,
            ),
            (
                Request::PlaylistSetImage {
                    playlist: playlist.clone(),
                    image_base64: "image".into(),
                    provider: None,
                },
                MutationLaneKind::Playlist,
            ),
            (
                Request::ArtistFollow {
                    artist: "spotify:artist:1".into(),
                },
                MutationLaneKind::Library,
            ),
            (
                Request::ArtistUnfollow {
                    artist: "spotify:artist:1".into(),
                },
                MutationLaneKind::Library,
            ),
        ];
        for (request, expected) in cases {
            assert_eq!(mutation_lane_kind(&request), Some(expected));
        }
    }

    #[test]
    fn playlist_aliases_share_one_mutation_lane() {
        for playlist in ["mix-id", "spotify:playlist:mix-id", "My Mix"] {
            let request = Request::PlaylistAddItems {
                playlist: playlist.to_string(),
                uris: vec!["spotify:track:1".into()],
                provider: None,
            };
            assert_eq!(
                mutation_lane_kind(&request),
                Some(MutationLaneKind::Playlist)
            );
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
/// Passed into the provider factory when running in keymaster mode. Mints the
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
    provider_id: String,
    session_bearer: Arc<dyn spotuify_spotify::WebApiBearerProvider>,
    token_slot: PlayerTokenSlot,
    cache: Arc<parking_lot::Mutex<Option<(String, Instant)>>>,
}

#[cfg(feature = "embedded-playback")]
impl FirstPartyBearerProvider {
    async fn mint(&self) -> Option<String> {
        self.session_bearer.bearer(false).await.ok()
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
        let creds = spotuify_spotify::auth::load_first_party_credentials_for(&self.provider_id)?
            .ok_or(SpotifyError::AuthRequired)?;
        let oauth =
            spotuify_player::backends::first_party_auth::refresh_oauth(&creds.refresh_token)
                .await
                .map_err(first_party_refresh_error)?;
        // PKCE refresh tokens rotate; persist the new one or the stored
        // credential goes stale and the next refresh fails.
        if !oauth.refresh_token.is_empty() && oauth.refresh_token != creds.refresh_token {
            let refresh =
                spotuify_player::backends::first_party_auth::refresh_material_from_oauth_token(
                    &oauth,
                );
            let rotated = spotuify_spotify::first_party::FirstPartyCredentials::new(
                refresh.refresh_token,
                refresh.scopes,
            );
            let persisted = spotuify_spotify::auth::save_rotated_first_party_credentials_for(
                &self.provider_id,
                &creds.refresh_token,
                &rotated,
            )?;
            if !persisted {
                return Err(SpotifyError::AuthRequired);
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
    let text = player_error_for_display(&err);
    let lower = text.to_lowercase();
    if lower.contains("invalid_grant") || lower.contains("revoked") {
        spotuify_spotify::SpotifyError::AuthRevoked
    } else {
        spotuify_spotify::SpotifyError::from(anyhow::anyhow!(
            "first-party OAuth refresh failed: {text}"
        ))
    }
}

fn spawn_player_actor(
    mut player: Option<PlayerBox>,
    player_policy_events: PlayerPolicyEventEmitter,
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
                    if let Some(player) = player.as_mut() {
                        handle_transport_command(player, transport, &player_policy_events).await;
                    } else {
                        let _ = transport.resp.send(Err(
                            spotuify_player::PlayerError::Unsupported(
                                "provider has no local player".to_string(),
                            ),
                        ));
                    }
                }
                command = rx.recv(), if command_open => {
                    let Some(command) = command else {
                        command_open = false;
                        continue;
                    };
                    let command = match command {
                        PlayerCommand::Install { backend, resp } => {
                            if player.is_some() {
                                let _ = resp.send(Err((
                                    spotuify_player::PlayerError::InvalidArg(
                                        "a provider player is already installed".to_string(),
                                    ),
                                    backend,
                                )));
                            } else {
                                player = Some(backend);
                                let _ = resp.send(Ok(()));
                            }
                            continue;
                        }
                        PlayerCommand::Uninstall { resp } => {
                            let _ = resp.send(player.take());
                            continue;
                        }
                        command => command,
                    };
                    let Some(player) = player.as_mut() else {
                        if respond_player_unavailable(command) {
                            break;
                        }
                        continue;
                    };
                    match command {
                        PlayerCommand::Install { .. } => unreachable!("install handled above"),
                        PlayerCommand::Uninstall { .. } => {
                            unreachable!("uninstall handled above")
                        }
                        PlayerCommand::RegisterDevice { name, resp } => {
                            let result = player.register_device(&name).await;
                            if let Err(error) = &result {
                                player_policy_events.emit_error(error);
                            }
                            let _ = resp.send(result);
                        }
                        PlayerCommand::Reconnect { name, resume, resp } => {
                            let policy_barrier =
                                player_policy_events.barrier_for_current_provider();
                            if let Err(err) = player.shutdown().await {
                                player_policy_events.emit_error(&err);
                                tracing::warn!(
                                    error = %player_error_for_display(&err),
                                    "player shutdown during reconnect failed; attempting register anyway"
                                );
                            }
                            let mut result = player.register_device(&name).await;
                            let mut playback_succeeded = false;
                            // Resume playback where it dropped so a silent
                            // session loss doesn't leave the device idle.
                            if result.is_ok() {
                                if let Some((uri, position_ms)) = resume {
                                    let resume_result = match player_resource_uri(&uri) {
                                        Ok(resource) => player.play_uri(&resource, position_ms).await,
                                        Err(error) => Err(error),
                                    };
                                    match resume_result {
                                        Ok(()) => {
                                            playback_succeeded = true;
                                            tracing::info!(
                                                uri,
                                                position_ms,
                                                "resumed playback after reconnect"
                                            );
                                        }
                                        Err(err @ spotuify_player::PlayerError::ProviderPolicy(_)) => {
                                            tracing::warn!(
                                                error = %player_error_for_display(&err),
                                                uri,
                                                "resume after reconnect blocked by provider policy"
                                            );
                                            result = Err(err);
                                        }
                                        Err(err) => tracing::warn!(
                                            error = %err,
                                            uri,
                                            "resume after reconnect failed"
                                        ),
                                    }
                                }
                            }
                            match &result {
                                Ok(_) if playback_succeeded => {
                                    if let Some(barrier) = policy_barrier.as_ref() {
                                        player_policy_events.clear_if_unchanged(barrier);
                                    }
                                }
                                Ok(_) => {}
                                Err(error) => {
                                    player_policy_events.emit_error(error);
                                }
                            }
                            let _ = resp.send(result);
                        }
                        PlayerCommand::SetAudioOutput { device, resp } => {
                            player.set_audio_output_device(device);
                            let _ = resp.send(());
                        }
                        PlayerCommand::IsConnected { resp } => {
                            let _ = resp.send(player.is_connected().await);
                        }
                        PlayerCommand::DropSession { resp } => {
                            let result = player.shutdown().await;
                            if let Err(error) = &result {
                                player_policy_events.emit_error(error);
                            }
                            let _ = resp.send(result);
                        }
                        PlayerCommand::Shutdown { resp } => {
                            if let Err(err) = player.shutdown().await {
                                player_policy_events.emit_error(&err);
                                tracing::warn!(
                                    error = %player_error_for_display(&err),
                                    "player backend shutdown failed"
                                );
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
                            let Some(player) = player.as_mut() else {
                                continue;
                            };
                            let result = match player_resource_uri(&uri) {
                                Ok(uri) => player.preload_uri(&uri).await,
                                Err(error) => Err(error),
                            };
                            match result {
                                Ok(()) => tracing::trace!(uri, "audio prewarm queued"),
                                Err(err) => {
                                    player_policy_events.emit_error(&err);
                                    tracing::debug!(
                                        error = %player_error_for_display(&err),
                                        uri,
                                        "audio prewarm failed"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    });
    (tx, transport_tx, warm_tx, handle)
}

fn respond_player_unavailable(command: PlayerCommand) -> bool {
    match command {
        PlayerCommand::Install { .. } | PlayerCommand::Uninstall { .. } => {
            unreachable!("player ownership commands handled before availability gate")
        }
        PlayerCommand::RegisterDevice { resp, .. } | PlayerCommand::Reconnect { resp, .. } => {
            let _ = resp.send(Err(spotuify_player::PlayerError::Unsupported(
                "provider has no local player backend".to_string(),
            )));
        }
        PlayerCommand::SetAudioOutput { resp, .. } => {
            let _ = resp.send(());
        }
        PlayerCommand::IsConnected { resp } => {
            let _ = resp.send(false);
        }
        PlayerCommand::DropSession { resp } => {
            let _ = resp.send(Err(spotuify_player::PlayerError::Unsupported(
                "provider has no local player".to_string(),
            )));
        }
        PlayerCommand::Shutdown { resp } => {
            let _ = resp.send(());
            return true;
        }
    }
    false
}

async fn handle_transport_command(
    player: &mut PlayerBox,
    command: PlayerTransportCommand,
    player_policy_events: &PlayerPolicyEventEmitter,
) {
    let policy_barrier = player_policy_events.barrier_for_current_provider();
    let clears_provider_policy = matches!(
        &command.cmd,
        TransportCmd::PlayUri { .. } | TransportCmd::PlayContext { .. } | TransportCmd::Resume
    );
    let result = match command.cmd {
        TransportCmd::PlayUri { uri, position_ms } => match player_resource_uri(&uri) {
            Ok(uri) => player.play_uri(&uri, position_ms).await,
            Err(error) => Err(error),
        },
        TransportCmd::PlayContext {
            context_uri,
            tracks,
            start_uri,
            position_ms,
        } => match player_play_context_request(context_uri, tracks, start_uri, position_ms) {
            Ok(request) => player.play_context(request).await,
            Err(error) => Err(error),
        },
        TransportCmd::Pause => player.pause().await,
        TransportCmd::Resume => player.resume().await,
        TransportCmd::Next => player.next().await,
        TransportCmd::Previous => player.previous().await,
        TransportCmd::Seek { position_ms } => player.seek(position_ms).await,
        TransportCmd::Volume { percent } => player.volume(percent).await,
        TransportCmd::Shuffle { on } => player.shuffle(on).await,
        TransportCmd::Repeat { mode } => player.repeat(mode).await,
    };
    match &result {
        Ok(()) if clears_provider_policy => {
            if let Some(barrier) = policy_barrier.as_ref() {
                player_policy_events.clear_if_unchanged(barrier);
            }
        }
        Err(error) => {
            player_policy_events.emit_error(error);
        }
        _ => {}
    }
    let _ = command.resp.send(result);
}

fn player_resource_uri(value: &str) -> PlayerResult<ResourceUri> {
    ResourceUri::parse(value).map_err(|error| {
        spotuify_player::PlayerError::InvalidArg(format!("invalid resource URI `{value}`: {error}"))
    })
}

fn player_play_context_request(
    context_uri: Option<String>,
    tracks: Option<Vec<String>>,
    start_uri: String,
    position_ms: u32,
) -> PlayerResult<spotuify_player::PlayContextRequest> {
    let start_uri = player_resource_uri(&start_uri)?;
    let source = match (context_uri, tracks) {
        (_, Some(tracks)) => spotuify_player::PlaySource::Ordered(
            tracks
                .iter()
                .map(|uri| player_resource_uri(uri))
                .collect::<PlayerResult<Vec<_>>>()?,
        ),
        (Some(context_uri), None) => {
            spotuify_player::PlaySource::Context(player_resource_uri(&context_uri)?)
        }
        (None, None) => spotuify_player::PlaySource::Single,
    };
    let request = spotuify_player::PlayContextRequest {
        source,
        start_uri,
        position_ms,
    };
    request.validate()?;
    Ok(request)
}

async fn apply_viz_config(
    viz_coordinator: &Arc<VizCoordinator>,
    config: &spotuify_config::VizConfig,
) {
    viz_coordinator.set_target_fps(config.target_fps);
    viz_coordinator.set_analyzer_params(config.smoothing, config.noise_gate);
    viz_coordinator
        .set_source(spotuify_protocol::VizSourceKindData::parse(&config.source))
        .await;
    viz_coordinator.set_enabled(config.enabled).await;
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
    if let Ok(loaded) = spotuify_config::load() {
        let config = loaded.config;
        system.cover_cache.ttl = Duration::from_secs(
            config
                .cache
                .cover_cache_ttl_days
                .saturating_mul(24 * 60 * 60),
        );
        system.cover_cache.max_bytes = config.cache.cover_cache_mb.saturating_mul(1024 * 1024);
        system.hooks =
            config
                .analytics
                .hook_command
                .clone()
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

struct DaemonSchemaCompatReporter {
    events: DaemonEventEmitter,
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
        self.events.emit(DaemonEvent::SchemaCompat {
            endpoint: endpoint.to_string(),
            missing_keys: normalized,
        });
    }
}

async fn recover_pending_receipts(
    store: &Store,
    event_tx: &broadcast::Sender<IpcMessage>,
    finished_at_ms: i64,
) -> Result<usize> {
    let pending = store.list_pending_receipts().await?;
    let processing_receipts = store
        .processing_mutation_claims()
        .await?
        .into_iter()
        .map(|claim| claim.receipt_id)
        .collect::<HashSet<_>>();
    let mut recovered = 0;
    for receipt in pending {
        if processing_receipts.contains(&receipt.receipt_id) {
            continue;
        }
        let provider = match store.receipt_request_json(receipt.receipt_id).await {
            Ok(request_json) => pending_receipt_provider(store, &request_json).await,
            Err(_) => None,
        };
        let message = format!(
            "{} failed because the daemon stopped before the provider confirmed it",
            receipt.action
        );
        let error = spotuify_protocol::ApiErrorSummary {
            kind: spotuify_protocol::IpcErrorKind::Internal,
            message: message.clone(),
            retry_after_secs: None,
            provider,
            detail: Some(message.clone()),
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
            mutation_id: None,
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

async fn pending_receipt_provider(store: &Store, request_json: &str) -> Option<ProviderId> {
    let request = serde_json::from_str::<Request>(request_json).ok()?;
    match &request {
        Request::PlaylistCreate {
            provider: Some(provider),
            ..
        }
        | Request::PlaylistAddItems {
            provider: Some(provider),
            ..
        }
        | Request::PlaylistRemoveItems {
            provider: Some(provider),
            ..
        }
        | Request::PlaylistUnfollow {
            provider: Some(provider),
            ..
        }
        | Request::PlaylistSetImage {
            provider: Some(provider),
            ..
        } => return Some(provider.clone()),
        _ => {}
    }
    let uris = match request {
        Request::QueueAdd { uri } => vec![uri],
        Request::QueueAddMany { uris } => uris,
        Request::RadioStart { seed_uri, .. } => vec![seed_uri],
        Request::PlaylistAddItems { playlist, .. }
        | Request::PlaylistRemoveItems { playlist, .. }
        | Request::PlaylistUnfollow { playlist, .. }
        | Request::PlaylistSetImage { playlist, .. } => vec![playlist],
        Request::LibrarySave { uri: Some(uri), .. }
        | Request::LibraryUnsave { uri }
        | Request::ArtistFollow { artist: uri }
        | Request::ArtistUnfollow { artist: uri } => vec![uri],
        _ => Vec::new(),
    };
    if uris.is_empty() {
        return None;
    }
    let items = store.media_items_by_uris(&uris).await.ok()?;
    let providers = items
        .iter()
        .filter_map(|item| match item.source.as_ref() {
            Some(spotuify_core::ItemSource::Provider(provider)) => ProviderId::new(provider).ok(),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    (providers.len() == 1)
        .then(|| providers.into_iter().next())
        .flatten()
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
    provider: Option<ProviderId>,
) -> bool {
    if !spotuify_spotify::auth::token_needs_scope_reauth(token) {
        return false;
    }
    let _ = event_tx.send(IpcMessage {
        id: 0,
        source: None,
        mutation_id: None,
        payload: IpcPayload::Event(DaemonEvent::AuthError {
            kind: spotuify_protocol::AuthErrorKind::ScopeReauthRequired,
            provider,
        }),
    });
    true
}

/// When the daemon resolves to first-party-only Spotify auth (the
/// chronically rate-limited state), returns `Some(can_login_dev_app)` so
/// clients can render the migration banner. Returns `None` for dev-app,
/// hybrid, and env-forced-dev-app modes — none of which warrant the
/// advisory. Mirrors [`Config::is_first_party`] but does not require a
/// loaded config, so it still fires for a first-party-only user with no
/// BYO `client_id` (whose `Config::load()` fails). Disk/config only; never
/// reads or serializes any token material.
///
/// `can_login_dev_app` is `true` when a dev-app `client_id` is configured
/// (so `spotuify login --dev-app` works). `Config::load()` errors on an
/// empty `client_id`, which is exactly the "recommend `spotuify onboard`
/// instead" case, so its success is the signal.
fn auth_migration_advisory(
    provider: &ProviderId,
    config: &spotuify_config::AppConfig,
) -> Option<bool> {
    let stored_first_party_only =
        spotuify_spotify::auth::stored_first_party_only_for(provider.as_str());
    let resolved_first_party = match spotuify_spotify::config::first_party_env_override() {
        Some(explicit) => explicit,
        None => stored_first_party_only,
    };
    (resolved_first_party && stored_first_party_only)
        .then(|| provider_client_id_configured(config, provider))
}

fn provider_client_id_configured(
    config: &spotuify_config::AppConfig,
    provider_id: &ProviderId,
) -> bool {
    config.providers.iter().any(|provider| {
        &provider.id == provider_id
            && provider.kind == "spotify"
            && provider
                .raw_table()
                .get("client_id")
                .and_then(|value| value.as_str())
                .is_some_and(|value| !value.trim().is_empty())
    })
}

/// Broadcast the first-party-only migration advisory at most once per
/// daemon run. `advisory` is [`auth_migration_advisory`]'s result:
/// `None` (dev-app / hybrid / env-forced-dev-app) is a no-op, `Some(_)`
/// emits `AuthMigrationRecommended` unless the `latch` already fired.
/// Returns `true` when an event was sent. The advisory carries only a
/// bool — never any credential material.
fn emit_auth_migration_event_if_needed(
    advisory: Option<bool>,
    latch: &std::sync::atomic::AtomicBool,
    event_tx: &broadcast::Sender<IpcMessage>,
) -> bool {
    let Some(can_login_dev_app) = advisory else {
        return false;
    };
    if latch.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return false;
    }
    let _ = event_tx.send(IpcMessage {
        id: 0,
        source: None,
        mutation_id: None,
        payload: IpcPayload::Event(DaemonEvent::AuthMigrationRecommended { can_login_dev_app }),
    });
    true
}

// Drain the player's PlayerEvent stream and translate each event to
// the wire-level DaemonEvent. Lives on its own task so the player
// can emit asynchronously without blocking commands.
struct PlayerEventForwarder {
    event_emitter: DaemonEventEmitter,
    session_tracker: Arc<crate::session_tracker::SessionTracker>,
    viz_coordinator: Arc<VizCoordinator>,
    playback_clock: Arc<crate::clock::PlaybackClock>,
    store: Store,
    player_tx: mpsc::Sender<PlayerCommand>,
    own_device_name: Arc<parking_lot::Mutex<Option<String>>>,
    own_device_volume: Arc<parking_lot::Mutex<Option<u8>>>,
    reconnect_in_flight: Arc<AtomicBool>,
    we_are_active: Arc<AtomicBool>,
    /// Shared with the health loop so the event-driven reconnect uses the same
    /// consecutive-failure count and backoff curve (one throttle, all paths).
    player_health: Arc<parking_lot::Mutex<PlayerHealth>>,
    embedded_sink_on_ready: bool,
    /// Identity captured from the validated registry entry that supplied this
    /// event stream. This cannot drift if the configured default changes.
    player_provider_id: ProviderId,
    embedded_provider_id: Arc<RwLock<Option<ProviderId>>>,
    player_policy_events: PlayerPolicyEventEmitter,
    active_transport_provider: Arc<RwLock<Option<ProviderId>>>,
    provider_revision_tx: watch::Sender<u64>,
    mutation_seq: Arc<AtomicU64>,
}

async fn forward_player_events(
    mut stream: tokio_stream::wrappers::UnboundedReceiverStream<PlayerEvent>,
    ctx: PlayerEventForwarder,
) {
    while let Some(event) = stream.next().await {
        let reclaims_global_transport = matches!(&event, PlayerEvent::PlaybackStarted { .. })
            || (matches!(&event, PlayerEvent::TrackChanged { .. })
                && ctx.we_are_active.load(Ordering::Acquire));
        if reclaims_global_transport {
            let embedded_provider_id = ctx.embedded_provider_id.read().clone();
            if let Some(embedded_provider_id) = embedded_provider_id.as_ref() {
                let changed = {
                    let mut active = ctx.active_transport_provider.write();
                    if active.as_ref() == Some(embedded_provider_id) {
                        false
                    } else {
                        *active = Some(embedded_provider_id.clone());
                        true
                    }
                };
                if changed {
                    ctx.mutation_seq.fetch_add(1, Ordering::AcqRel);
                    ctx.provider_revision_tx
                        .send_modify(|revision| *revision = revision.wrapping_add(1));
                }
            }
        }
        let embedded_provider_id = ctx.embedded_provider_id.read().clone();
        let owns_global_transport = embedded_provider_id.as_ref().is_some_and(|embedded| {
            ctx.active_transport_provider
                .read()
                .as_ref()
                .is_none_or(|active| active == embedded)
        });
        // Listening facts are part of the same daemon-owned transport view as
        // playback/queue. After a hand-off, late embedded ticks, pauses, track
        // changes, and terminal events must not accrue or finalize sessions
        // for the provider that no longer owns that view. A genuine new
        // embedded PlaybackStarted reclaims ownership above before observing.
        if owns_global_transport {
            ctx.session_tracker.observe(&event).await;
        }
        // Phase 8 — feed the playback clock. PlayerEvent is the
        // highest-trust source: ~sub-100ms after the audio actually
        // changed state. Web API polls become reconciliation only.
        if owns_global_transport {
            ctx.playback_clock
                .apply_player_event(&event, spotuify_core::now_ms());
        }
        if owns_global_transport {
            if let Some(uri) = player_event_media_uri(&event) {
                if let Some(item) =
                    lookup_player_event_media_item(&ctx.store, embedded_provider_id.as_ref(), &uri)
                        .await
                {
                    if ctx.playback_clock.enrich_current_item(&item) {
                        tracing::debug!(uri, "enriched playback clock item from local metadata");
                    }
                }
            }
        }
        match &event {
            PlayerEvent::Ready { .. } if ctx.embedded_sink_on_ready => {
                ctx.viz_coordinator.set_sink_available(true).await;
            }
            PlayerEvent::PlaybackStarted { .. }
            | PlayerEvent::PlaybackResumed
            | PlayerEvent::TrackChanged { .. }
                if owns_global_transport =>
            {
                ctx.viz_coordinator.set_playing(true)
            }
            PlayerEvent::PlaybackPaused
            | PlayerEvent::EndOfTrack { .. }
            | PlayerEvent::SessionDisconnected { .. }
            | PlayerEvent::Failed { .. }
                if owns_global_transport =>
            {
                ctx.viz_coordinator.set_playing(false)
            }
            PlayerEvent::VolumeChanged { percent } => {
                // The embedded device is the only source of its own volume
                // (Web API reports `null`). Record it for
                // `connected_own_device` and fold it into the clock so the
                // now-playing volume row is correct and rate-limit-proof.
                *ctx.own_device_volume.lock() = Some(*percent);
                let percent = *percent;
                let name = ctx.own_device_name.lock().clone();
                if owns_global_transport {
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
            }
            _ => {}
        }
        // Phase 8 — for events that translate to a `PlaybackChanged`,
        // embed the freshly-updated clock snapshot so subscribers get
        // local-event truth in one IPC.
        let snapshot_for_push = (owns_global_transport
            && matches!(
                &event,
                PlayerEvent::PlaybackStarted { .. }
                    | PlayerEvent::PlaybackPaused
                    | PlayerEvent::PlaybackResumed
                    | PlayerEvent::TrackChanged { .. }
                    | PlayerEvent::EndOfTrack { .. }
                    | PlayerEvent::VolumeChanged { .. }
            ))
        .then(|| ctx.playback_clock.snapshot());
        // Our embedded device is producing audio → the user intends this
        // device to be the active target. (Cleared by the Web API poll when a
        // different device becomes active — see `note_active_device`.)
        if owns_global_transport
            && matches!(
                &event,
                PlayerEvent::PlaybackStarted { .. }
                    | PlayerEvent::PlaybackResumed
                    | PlayerEvent::TrackChanged { .. }
            )
        {
            ctx.we_are_active.store(true, Ordering::Release);
        }
        let should_reconnect = owns_global_transport
            && matches!(
                &event,
                PlayerEvent::SessionDisconnected { .. } | PlayerEvent::Failed { .. }
            );
        if !owns_global_transport
            && matches!(
                &event,
                PlayerEvent::PlaybackPaused
                    | PlayerEvent::PlaybackResumed
                    | PlayerEvent::EndOfTrack { .. }
                    | PlayerEvent::VolumeChanged { .. }
                    | PlayerEvent::SessionDisconnected { .. }
                    | PlayerEvent::Failed { .. }
                    | PlayerEvent::Degraded { .. }
            )
        {
            continue;
        }
        let daemon_event =
            translate_player_event_with_snapshot(event, snapshot_for_push, &ctx.player_provider_id);
        let Some(daemon_event) = daemon_event else {
            continue;
        };
        match daemon_event {
            DaemonEvent::ProviderPolicy { provider, reason } => {
                ctx.player_policy_events
                    .emit_for_provider(provider, &reason);
            }
            daemon_event => ctx.event_emitter.emit(daemon_event),
        }
        // Only auto-reconnect when the user still wants this device active.
        // After a hand-off to another device, `we_are_active` is false, so a
        // session drop leaves us idle instead of re-registering and letting
        // librespot grab playback back. The next user transport re-registers.
        if should_reconnect {
            // Snapshot resume intent now, while the clock is still fresh: the
            // Web API "no active session" confirmation hasn't landed yet, so
            // is_playing/position still reflect what we were doing at drop time.
            let own_name = ctx.own_device_name.lock().clone();
            let own_id = own_name.as_deref().map(derive_device_id_for_name);
            let resume = resume_target_after_drop(
                &ctx.playback_clock.snapshot(),
                own_id.as_deref(),
                own_name.as_deref(),
            );
            // Reconnect when the user still wants this device active: either
            // it's the tracked active target, or the clock shows we were
            // playing on it (a robust fallback for when `we_are_active` lags
            // behind the macOS app's play path). A genuine hand-off to another
            // device leaves both false, so a drop correctly leaves us idle.
            if ctx.we_are_active.load(Ordering::Acquire) || resume.is_some() {
                // Share the health loop's failure count so event- and
                // health-driven reconnects use one backoff curve.
                let backoff = {
                    let mut health = ctx.player_health.lock();
                    let backoff = reconnect_backoff(health.consecutive_failures.saturating_sub(1));
                    health.reconnect_attempts = health.reconnect_attempts.saturating_add(1);
                    health.current_backoff_ms = backoff.as_millis() as u64;
                    backoff
                };
                schedule_player_reconnect(
                    ctx.player_tx.clone(),
                    ctx.reconnect_in_flight.clone(),
                    resume,
                    backoff,
                    own_name.unwrap_or_else(|| {
                        spotuify_config::PlayerSettings::default().effective_device_name()
                    }),
                );
            }
        }
    }
}

fn player_event_media_uri(event: &PlayerEvent) -> Option<String> {
    match event {
        PlayerEvent::PlaybackStarted { uri, .. } | PlayerEvent::TrackChanged { uri, .. } => {
            Some(uri.as_uri())
        }
        _ => None,
    }
}

async fn lookup_player_event_media_item(
    store: &Store,
    provider: Option<&ProviderId>,
    uri: &str,
) -> Option<MediaItem> {
    let queue = match provider {
        Some(provider) => store.latest_provider_queue(500, provider).await,
        None => store.latest_queue(500).await,
    };
    if let Ok(Some(queue)) = queue {
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

/// Whether a reported active device is our own embedded device. Matches by id
/// when both sides have one; falls back to the device *name* otherwise — car
/// head units and other restricted Connect devices commonly report
/// `device.id: null` in `/me/player`, and an id-only match classified them as
/// "unknown, assume ours", letting stall recovery steal their playback
/// (observed 2026-06-29: the watchdog yanked an in-car session to the Mac).
fn device_matches_own(
    device: &spotuify_core::Device,
    own_device_id: Option<&str>,
    own_device_name: Option<&str>,
) -> bool {
    match (device.id.as_deref(), own_device_id) {
        (Some(active), Some(own)) => active == own,
        _ => own_device_name.is_some_and(|own| own == device.name),
    }
}

/// Decide whether an auto-reconnect should resume playback, and from where.
///
/// Returns `Some((uri, position_ms))` only when we were actively playing on our
/// own device (or on no recorded device — the silent-drop case). A genuine
/// hand-off to a *different* active device — including one that reports no
/// device id, matched by name — returns `None`, so the reconnect re-registers
/// our device without stealing playback back from the phone/car/etc.
fn resume_target_after_drop(
    playback: &spotuify_core::Playback,
    own_device_id: Option<&str>,
    own_device_name: Option<&str>,
) -> Option<(String, u32)> {
    if !playback.is_playing {
        return None;
    }
    let item = playback.item.as_ref()?;
    if item.uri.is_empty() {
        return None;
    }
    let on_other_device = playback
        .device
        .as_ref()
        .is_some_and(|device| !device_matches_own(device, own_device_id, own_device_name));
    if on_other_device {
        return None;
    }
    Some((
        item.uri.clone(),
        playback.progress_ms.min(u32::MAX as u64) as u32,
    ))
}

fn schedule_player_reconnect(
    player_tx: mpsc::Sender<PlayerCommand>,
    reconnect_in_flight: Arc<AtomicBool>,
    resume: Option<(String, u32)>,
    backoff: Duration,
    device_name: String,
) {
    if reconnect_in_flight.swap(true, Ordering::AcqRel) {
        return;
    }
    tokio::spawn(async move {
        tokio::time::sleep(backoff).await;
        let (resp, rx) = oneshot::channel();
        let sent = player_tx
            .send(PlayerCommand::Reconnect {
                name: device_name,
                resume,
                resp,
            })
            .await;
        if sent.is_ok() {
            match tokio::time::timeout(Duration::from_secs(10), rx).await {
                Ok(Ok(Ok(_))) => tracing::info!("player auto-reconnect succeeded"),
                Ok(Ok(Err(err))) => {
                    tracing::warn!(
                        error = %player_error_for_display(&err),
                        "player auto-reconnect failed"
                    );
                }
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
    player_provider_id: &ProviderId,
) -> Option<DaemonEvent> {
    let mut translated = translate_player_event(event, player_provider_id)?;
    if let DaemonEvent::PlaybackChanged { playback, .. } = &mut translated {
        if playback.is_none() {
            *playback = snapshot;
        }
    }
    Some(translated)
}

fn translate_player_event(
    event: PlayerEvent,
    player_provider_id: &ProviderId,
) -> Option<DaemonEvent> {
    match event {
        PlayerEvent::Ready { device_id, name } => Some(DaemonEvent::PlayerReady {
            device_id: device_id.0,
            name,
        }),
        PlayerEvent::Degraded { reason } => Some(DaemonEvent::PlayerDegraded { reason }),
        PlayerEvent::ProviderPolicy { reason } => Some(DaemonEvent::ProviderPolicy {
            provider: player_provider_id.clone(),
            reason: spotuify_protocol::sanitize_provider_policy_reason(&reason),
        }),
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
    fn sync_provider_revision_receiver(&self) -> Option<watch::Receiver<u64>> {
        Some(self.provider_revision_tx.subscribe())
    }
    fn store(&self) -> &spotuify_store::Store {
        &self.store
    }
    fn emit_event(&self, event: spotuify_protocol::DaemonEvent) {
        DaemonState::emit_event(self, event);
    }
    fn sync_locks_for(
        &self,
        provider_id: &str,
        target: spotuify_protocol::SyncTargetData,
    ) -> Vec<Arc<Mutex<()>>> {
        let locks = self
            .sync_locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(provider_id.to_string())
            .or_insert_with(ProviderSyncLocks::new)
            .clone();
        locks.for_target(target)
    }
    async fn sync_providers(&self) -> anyhow::Result<Vec<spotuify_sync::SyncProvider>> {
        let providers = DaemonState::providers(self).await?;
        let selected_transport = self
            .active_transport_provider()
            .filter(|provider| providers.provider(provider).is_ok())
            .unwrap_or_else(|| providers.default_id().clone());
        let mut sync_providers = Vec::with_capacity(providers.len());
        for (provider_id, runtime) in providers.iter() {
            // Background transport state is one global daemon view. Only the
            // selected/default provider may update it; secondary providers
            // still participate in provider-scoped library/playlist sync.
            let transport = if provider_id == &selected_transport {
                match runtime.transport() {
                    Ok(transport) => Some(transport),
                    Err(ProviderError::Unsupported { .. }) => None,
                    Err(err) => return Err(err.into()),
                }
            } else {
                None
            };
            sync_providers.push(spotuify_sync::SyncProvider::new(
                runtime.music(),
                transport,
            )?);
        }
        Ok(sync_providers)
    }
    fn observe_mutation_seq(&self) -> u64 {
        DaemonState::current_mutation_seq(self)
    }
    fn may_apply_transport_update(&self, provider: &ProviderId, captured_seq: u64) -> bool {
        DaemonState::may_apply_state_update(self, captured_seq)
            && self
                .active_transport_provider()
                .as_ref()
                .is_none_or(|active| active == provider)
    }
    async fn prepare_and_persist_playback_poll_if_current(
        &self,
        provider: &ProviderId,
        playback: &spotuify_core::Playback,
        captured_seq: u64,
        sampled_at_ms: i64,
        provider_timestamp_ms: Option<i64>,
    ) -> anyhow::Result<Option<(u32, spotuify_core::Playback)>> {
        // Transport mutations hold this same lane while executing and
        // persisting their command result. The sequence bump happens before
        // they wait for the lane: either we see it and discard, or our older
        // write completes first and the command's newer write follows.
        let _guard = self.transport_mutation_lock.lock().await;
        if !<Self as spotuify_sync::SyncContext>::may_apply_transport_update(
            self,
            provider,
            captured_seq,
        ) {
            return Ok(None);
        }
        let Some(candidate) = self.playback_clock.prepare_web_api_poll(
            playback,
            sampled_at_ms,
            provider_timestamp_ms,
        ) else {
            return Ok(None);
        };
        let written = self
            .store
            .persist_provider_playback_bulk(provider, &candidate)
            .await?;
        Ok(Some((written, candidate)))
    }
    async fn persist_queue_poll_if_current(
        &self,
        provider: &ProviderId,
        queue: &spotuify_core::Queue,
        captured_seq: u64,
    ) -> anyhow::Result<Option<u32>> {
        let _guard = self.transport_mutation_lock.lock().await;
        if !self.transport_update_is_current(provider, captured_seq) {
            return Ok(None);
        }
        Ok(Some(
            self.store
                .persist_provider_queue_bulk(provider, queue)
                .await?,
        ))
    }
    async fn persist_devices_poll_if_current(
        &self,
        provider: &ProviderId,
        devices: &[spotuify_core::Device],
        captured_seq: u64,
    ) -> anyhow::Result<Option<u32>> {
        let _guard = self.transport_mutation_lock.lock().await;
        if !self.transport_update_is_current(provider, captured_seq) {
            return Ok(None);
        }
        Ok(Some(
            self.store
                .replace_provider_devices(provider, devices)
                .await?,
        ))
    }
    fn background_runtime(&self) -> Option<RuntimeHandle> {
        Some(self.bg_runtime_handle())
    }
    async fn index_media_items(
        &self,
        provider_id: &str,
        items: &[MediaItem],
        saved: bool,
    ) -> anyhow::Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let entries = items
            .iter()
            .cloned()
            .map(|item| {
                ResourceUri::parse(&item.uri)?;
                let search_origin = item
                    .source
                    .as_ref()
                    .map_or(provider_id, spotuify_core::ItemSource::as_str)
                    .to_string();
                anyhow::Ok(spotuify_store::IndexedMediaItem {
                    item,
                    provider: provider_id.to_string(),
                    liked: saved,
                    saved,
                    added_at_ms: Some(spotuify_store::now_ms()),
                    search_origin,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        self.search
            .apply_batch(spotuify_search::SearchUpdateBatch {
                entries,
                removed_uris: Vec::new(),
            })
            .await
    }
    async fn remove_indexed_media_items(&self, uris: &[String]) -> anyhow::Result<()> {
        if uris.is_empty() {
            return Ok(());
        }
        self.search
            .apply_batch(spotuify_search::SearchUpdateBatch {
                entries: Vec::new(),
                removed_uris: uris.to_vec(),
            })
            .await
    }
    fn warm_queue(&self, queue: &spotuify_spotify::client::Queue) {
        DaemonState::warm_queue(self, queue);
    }
    fn apply_playback_poll(
        &self,
        provider: &ProviderId,
        playback: &spotuify_core::Playback,
        captured_seq: u64,
        state_seq: u64,
        sampled_at_ms: i64,
        provider_timestamp_ms: Option<i64>,
    ) -> bool {
        if !self
            .active_transport_provider()
            .as_ref()
            .is_none_or(|active| active == provider)
        {
            return false;
        }
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
    fn prepare_playback_poll(
        &self,
        playback: &spotuify_core::Playback,
        sampled_at_ms: i64,
        provider_timestamp_ms: Option<i64>,
    ) -> Option<spotuify_core::Playback> {
        self.playback_clock
            .prepare_web_api_poll(playback, sampled_at_ms, provider_timestamp_ms)
    }
    fn snapshot_playback(&self) -> spotuify_core::Playback {
        DaemonState::snapshot_playback(self)
    }
    fn embedded_is_active_playback(&self) -> bool {
        DaemonState::embedded_owns_playback(self)
    }
    async fn snapshot_queue(&self, provider: &ProviderId) -> spotuify_spotify::client::Queue {
        let queue = self
            .store
            .latest_provider_queue(500, provider)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        self.queue_snapshot_for_clients(queue)
    }
    fn overlay_pending_queue_appends(
        &self,
        provider: &ProviderId,
        queue: spotuify_spotify::client::Queue,
        now_ms: i64,
    ) -> spotuify_spotify::client::Queue {
        DaemonState::overlay_pending_queue_appends(self, provider, queue, now_ms)
    }
    async fn snapshot_devices(&self, provider: &ProviderId) -> Vec<spotuify_core::Device> {
        self.store
            .list_provider_devices(provider)
            .await
            .unwrap_or_default()
    }
    fn event_subscriber_count(&self) -> usize {
        self.event_tx.receiver_count()
    }
}

#[cfg(test)]
mod queue_pending_tests {
    #![allow(clippy::panic, clippy::unwrap_used)]

    use super::{
        merge_queue_pending_appends, pending_queue_appends_for, PENDING_QUEUE_APPEND_TTL_MS,
    };
    use spotuify_core::{MediaItem, MediaKind, ProviderId, Queue, ResourceUri};

    fn track(uri: &str, name: &str) -> MediaItem {
        MediaItem {
            id: ResourceUri::parse(uri)
                .ok()
                .map(|resource| resource.bare_id().to_string()),
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
        let provider = ProviderId::new("spotify").unwrap();
        let live: std::collections::HashSet<String> =
            std::iter::once(existing.uri.clone()).collect();
        let mut pending =
            pending_queue_appends_for(&provider, &live, std::slice::from_ref(&queued), 100);

        let (merged, changed) = merge_queue_pending_appends(
            &provider,
            queue(vec![existing.clone()], 2),
            &mut pending,
            200,
        );
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
            &provider,
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
            merge_queue_pending_appends(&provider, queue(vec![existing], 4), &mut pending, 400);
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
        let provider = ProviderId::new("spotify").unwrap();
        let live: std::collections::HashSet<String> =
            std::iter::once(existing.uri.clone()).collect();
        let mut pending = pending_queue_appends_for(&provider, &live, &[queued], 100);

        let (merged, changed) = merge_queue_pending_appends(
            &provider,
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
    #![allow(clippy::panic, clippy::unwrap_used)]

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
mod search_startup_repair_tests {
    #![allow(clippy::panic, clippy::unwrap_used)]

    use super::open_search_service;
    use spotuify_core::{MediaItem, MediaKind, ProviderId};
    use spotuify_protocol::SearchScopeData;
    use spotuify_search::SearchIndex;
    use spotuify_store::Store;

    #[tokio::test]
    async fn startup_repair_repopulates_cached_media_and_records_reason() {
        let temp = tempfile::tempdir().expect("tempdir");
        let db_path = temp.path().join("cache.sqlite");
        let index_path = temp.path().join("search-index");
        let store = Store::open(&db_path, &index_path).await.expect("store");
        store
            .cache_provider_search_results(
                &ProviderId::new("spotify").expect("valid provider id"),
                "luther vandross",
                SearchScopeData::Track,
                "spotify",
                &[MediaItem {
                    id: Some("1".to_string()),
                    uri: "spotify:track:1".to_string(),
                    name: "Never Too Much".to_string(),
                    subtitle: "Luther Vandross".to_string(),
                    context: "Album".to_string(),
                    duration_ms: 180_000,
                    kind: MediaKind::Track,
                    source: Some("spotify".into()),
                    ..Default::default()
                }],
            )
            .await
            .expect("cache media");

        drop(
            SearchIndex::open(&index_path)
                .expect("initial search index")
                .index,
        );
        std::fs::write(index_path.join("meta.json"), b"corrupt metadata")
            .expect("corrupt metadata");

        let (search, worker) = open_search_service(&store)
            .await
            .expect("startup search repair");

        let hits = search
            .search("luther", SearchScopeData::Track, 10)
            .await
            .expect("search repaired index");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].uri, "spotify:track:1");
        assert_eq!(search.num_docs().await.expect("document count"), 1);
        let repairs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sync_events
             WHERE domain = 'search_index_repair/startup_repair' AND status = 'ok'",
        )
        .fetch_one(store.reader())
        .await
        .expect("repair event");
        assert_eq!(repairs, 1);

        search.request_shutdown().await.expect("search shutdown");
        tokio::time::timeout(std::time::Duration::from_secs(2), worker)
            .await
            .expect("worker shutdown timeout")
            .expect("worker join");
    }

    #[tokio::test]
    async fn startup_repair_recovers_valid_schema_with_partial_document_count() {
        let temp = tempfile::tempdir().expect("tempdir");
        let db_path = temp.path().join("cache.sqlite");
        let index_path = temp.path().join("search-index");
        let store = Store::open(&db_path, &index_path).await.expect("store");
        store
            .cache_provider_search_results(
                &ProviderId::new("spotify").expect("valid provider id"),
                "partial",
                SearchScopeData::Track,
                "spotify",
                &[MediaItem {
                    id: Some("partial".to_string()),
                    uri: "spotify:track:partial".to_string(),
                    name: "Partial Reindex".to_string(),
                    subtitle: "Artist".to_string(),
                    context: "Album".to_string(),
                    duration_ms: 180_000,
                    kind: MediaKind::Track,
                    source: Some("spotify".into()),
                    ..Default::default()
                }],
            )
            .await
            .expect("cache media");

        // Current-schema but empty index: the state left by a crash between
        // schema recreation and SQLite repopulation.
        drop(SearchIndex::open(&index_path).expect("empty index").index);

        let (search, worker) = open_search_service(&store).await.expect("count repair");
        assert_eq!(search.num_docs().await.unwrap(), 1);
        let repairs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sync_events
             WHERE domain = 'search_index_repair/document_count_mismatch' AND status = 'ok'",
        )
        .fetch_one(store.reader())
        .await
        .unwrap();
        assert_eq!(repairs, 1);

        search.request_shutdown().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), worker)
            .await
            .expect("worker shutdown timeout")
            .expect("worker join");
    }
}

#[cfg(test)]
mod receipt_recovery {
    #![allow(clippy::panic, clippy::unwrap_used)]

    use super::{
        build_system_config, recover_pending_receipts, DaemonEventEmitter,
        DaemonSchemaCompatReporter, EventLogWriter,
    };
    use std::collections::HashSet;
    use std::sync::Arc;

    use spotuify_core::ProviderId;
    use spotuify_protocol::{
        DaemonEvent, IpcMessage, IpcPayload, MutationId, Operation, OperationId, OperationKind,
        OperationSource, OperationStatus, Receipt, ReceiptId, ReceiptStatus,
    };
    use spotuify_spotify::client::SchemaCompatReporter;
    use spotuify_store::Store;
    use tokio::sync::{broadcast, watch};

    fn schema_reporter_harness() -> (
        DaemonSchemaCompatReporter,
        broadcast::Receiver<IpcMessage>,
        EventLogWriter,
        watch::Sender<bool>,
        tokio::task::JoinHandle<()>,
    ) {
        let (event_tx, event_rx) = broadcast::channel(8);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (event_log, worker) = EventLogWriter::spawn(shutdown_rx);
        let events = DaemonEventEmitter {
            event_tx,
            event_log: event_log.clone(),
            system_integration: Arc::new(spotuify_system::SystemIntegration::spawn(
                build_system_config(),
            )),
            order: Arc::new(parking_lot::Mutex::new(())),
        };
        (
            DaemonSchemaCompatReporter {
                events,
                seen: Arc::new(parking_lot::Mutex::new(HashSet::new())),
            },
            event_rx,
            event_log,
            shutdown_tx,
            worker,
        )
    }

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
    async fn startup_receipt_recovery_skips_processing_mutation_receipts() {
        let store = Store::in_memory().await.expect("in-memory store");
        let independent = receipt("independent", ReceiptStatus::Pending);
        store
            .insert_pending_receipt(&independent, "{}")
            .await
            .unwrap();
        let processing = receipt("processing", ReceiptStatus::Pending);
        let operation = Operation {
            operation_id: OperationId::new_v7(),
            kind: OperationKind::LibrarySave,
            occurred_at_ms: 10,
            finished_at_ms: None,
            source: OperationSource::Cli,
            requester: None,
            subject_uris: vec!["fake:track:one".to_string()],
            reversible: false,
            reversal_plan: None,
            pre_state: None,
            status: OperationStatus::Pending,
            receipt_id: Some(processing.receipt_id),
            subject_op_id: None,
            undone_by_op_id: None,
            redone_by_op_id: None,
            error_message: None,
        };
        store
            .claim_mutation(
                MutationId::new_v7(),
                "fingerprint",
                "{}",
                &processing,
                &operation,
                10,
            )
            .await
            .unwrap();
        let (tx, _rx) = broadcast::channel::<IpcMessage>(8);

        assert_eq!(recover_pending_receipts(&store, &tx, 30).await.unwrap(), 1);
        assert_eq!(
            store
                .get_receipt(independent.receipt_id)
                .await
                .unwrap()
                .status,
            ReceiptStatus::Failed
        );
        assert_eq!(
            store
                .get_receipt(processing.receipt_id)
                .await
                .unwrap()
                .status,
            ReceiptStatus::Pending
        );
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

        let emitted = super::emit_scope_reauth_event_if_needed(
            Some(&stale_token),
            &tx,
            Some(ProviderId::new("spotify").expect("valid provider id")),
        );

        assert!(
            emitted,
            "missing-scope token should trigger the proactive re-auth banner event"
        );
        let event = rx.recv().await.expect("scope-reauth event");
        assert!(matches!(
            event.payload,
            IpcPayload::Event(DaemonEvent::AuthError {
                kind: AuthErrorKind::ScopeReauthRequired,
                provider: Some(provider),
            }) if provider.as_str() == "spotify"
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

        let emitted = super::emit_scope_reauth_event_if_needed(
            Some(&healthy_token),
            &tx,
            Some(ProviderId::new("spotify").expect("valid provider id")),
        );

        assert!(!emitted, "fully-scoped token should not trigger a banner");
        assert!(
            rx.try_recv().is_err(),
            "no AuthError event should be broadcast when scopes are healthy"
        );
    }

    #[tokio::test]
    async fn scope_reauth_event_silent_when_no_token_is_stored_yet() {
        let (tx, mut rx) = broadcast::channel::<IpcMessage>(8);

        let emitted = super::emit_scope_reauth_event_if_needed(
            None,
            &tx,
            Some(ProviderId::new("spotify").expect("valid provider id")),
        );

        assert!(!emitted, "logged-out users should not see a re-auth banner");
        assert!(
            rx.try_recv().is_err(),
            "no event should be broadcast when there is no stored token"
        );
    }

    #[tokio::test]
    async fn auth_migration_advisory_broadcasts_can_login_flag_once_per_run() {
        use std::sync::atomic::AtomicBool;

        let (tx, mut rx) = broadcast::channel::<IpcMessage>(8);
        let latch = AtomicBool::new(false);

        // First-party-only with a configured client_id → advisory Some(true).
        let emitted = super::emit_auth_migration_event_if_needed(Some(true), &latch, &tx);
        assert!(
            emitted,
            "first-party-only mode should broadcast the advisory"
        );

        let event = rx.recv().await.expect("advisory event");
        assert!(matches!(
            event.payload,
            IpcPayload::Event(DaemonEvent::AuthMigrationRecommended {
                can_login_dev_app: true,
            })
        ));

        // Latched: a second call in the same run must not re-broadcast.
        let again = super::emit_auth_migration_event_if_needed(Some(true), &latch, &tx);
        assert!(
            !again,
            "advisory is once-per-run; latch must suppress repeats"
        );
        assert!(
            rx.try_recv().is_err(),
            "no second advisory event should be broadcast"
        );
    }

    #[tokio::test]
    async fn auth_migration_advisory_silent_for_dev_app_and_hybrid_modes() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let (tx, mut rx) = broadcast::channel::<IpcMessage>(8);
        let latch = AtomicBool::new(false);

        // `None` is what `auth_migration_advisory()` returns for dev-app,
        // hybrid, and env-forced-dev-app modes — none warrant the banner.
        let emitted = super::emit_auth_migration_event_if_needed(None, &latch, &tx);

        assert!(!emitted, "non-first-party-only modes must stay silent");
        assert!(
            !latch.load(Ordering::Acquire),
            "the latch must not be consumed when the advisory does not apply"
        );
        assert!(
            rx.try_recv().is_err(),
            "no advisory event should be broadcast for dev-app/hybrid modes"
        );
    }

    #[tokio::test]
    async fn schema_compat_reporter_broadcasts_and_logs_event() {
        let (reporter, mut rx, event_log, shutdown, worker) = schema_reporter_harness();

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
        let snapshot = event_log.snapshot().await;
        assert_eq!(snapshot.len(), 1);
        assert!(matches!(
            snapshot[0].kind,
            spotuify_protocol::LoggedKind::SchemaCompat { .. }
        ));
        let _ = shutdown.send(true);
        worker.await.expect("event log worker");
    }

    #[tokio::test]
    async fn schema_compat_reporter_dedupes_same_endpoint_and_keys() {
        let (reporter, mut rx, event_log, shutdown, worker) = schema_reporter_harness();

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
        assert_eq!(event_log.snapshot().await.len(), 1);
        let _ = shutdown.send(true);
        worker.await.expect("event log worker");
    }
}

#[cfg(test)]
mod auth_revocation_tests {
    #![allow(clippy::panic, clippy::unwrap_used)]

    use std::ffi::OsString;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::time::Duration;

    use spotuify_core::{
        MediaItem, MediaKind, MusicProvider, Playback, ProviderError, ProviderId, UriScheme,
    };
    use spotuify_player::{DeviceId, PlayerError, PlayerEvent};
    use spotuify_protocol::{
        AuthErrorKind, DaemonEvent, IpcMessage, IpcPayload, Request, ResponseData,
    };
    use spotuify_provider_fake::FakeProvider;
    use spotuify_spotify::auth::StoredToken;
    use tempfile::TempDir;
    use tokio::sync::{broadcast, oneshot};

    use crate::provider_registry::{
        ProviderPlayer, ProviderRegistry, ProviderRuntime, TransportRecovery,
    };

    use super::{DaemonState, PlayerCommand, ProviderRegistryKey, TransportCmd};

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
                (
                    "SPOTUIFY_CONFIG_DIR",
                    std::env::var_os("SPOTUIFY_CONFIG_DIR"),
                ),
                ("SPOTUIFY_CONFIG", std::env::var_os("SPOTUIFY_CONFIG")),
            ];

            std::env::set_var("SPOTUIFY_FAKE_SPOTIFY", "1");
            std::env::set_var("SPOTUIFY_CACHE_DB", temp.path().join("cache.sqlite3"));
            std::env::set_var("SPOTUIFY_SEARCH_INDEX", temp.path().join("search-index"));
            std::env::set_var("SPOTUIFY_RUNTIME_DIR", temp.path().join("runtime"));
            std::env::set_var("SPOTUIFY_DATA_DIR", temp.path().join("data"));
            std::env::set_var("SPOTUIFY_CONFIG_DIR", temp.path().join("config"));
            std::env::set_var("SPOTUIFY_CONFIG", temp.path().join("spotuify.toml"));

            Self {
                _temp: temp,
                old_values,
            }
        }

        fn use_spotify_auth_config(&self) {
            std::env::remove_var("SPOTUIFY_FAKE_SPOTIFY");
            std::fs::write(
                self._temp.path().join("spotuify.toml"),
                r#"
[providers]
default = "spotify"

[providers.spotify]
type = "spotify"
client_id = "test-client"
redirect_uri = "http://127.0.0.1:8888/callback"
"#,
            )
            .expect("write isolated Spotify config");
        }

        fn use_fake_default_with_secondary_spotify(&self) {
            std::env::remove_var("SPOTUIFY_FAKE_SPOTIFY");
            std::fs::write(
                self._temp.path().join("spotuify.toml"),
                r#"
[providers]
default = "local"

[providers.local]
type = "fake"

[providers.custom-cloud]
type = "spotify"
client_id = "secondary-client"
redirect_uri = "http://127.0.0.1:8888/callback"
"#,
            )
            .expect("write fake-default secondary-Spotify config");
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

    async fn test_spotify_auth_state() -> (TestEnv, DaemonState) {
        let env = TestEnv::new();
        env.use_spotify_auth_config();
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

    async fn recv_auth_error(
        rx: &mut broadcast::Receiver<IpcMessage>,
        expected: AuthErrorKind,
        expected_provider: &ProviderId,
    ) {
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
                    kind,
                    provider: Some(provider),
                }) if kind == expected && provider == *expected_provider
            ) {
                return;
            }
        }
    }

    fn drain_events(rx: &mut broadcast::Receiver<IpcMessage>) {
        while rx.try_recv().is_ok() {}
    }

    async fn recv_provider_policy(
        rx: &mut broadcast::Receiver<IpcMessage>,
    ) -> (ProviderId, String) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let message = rx.recv().await.expect("provider policy event");
                if let IpcPayload::Event(DaemonEvent::ProviderPolicy { provider, reason }) =
                    message.payload
                {
                    return (provider, reason);
                }
            }
        })
        .await
        .expect("provider policy event timeout")
    }

    #[tokio::test]
    async fn provider_policy_logging_preserves_emit_order_without_delaying_broadcast() {
        let _guard = crate::ENV_LOCK.lock().await;
        let (_env, state) = test_state().await;
        let mut rx = state.event_tx.subscribe();
        drain_events(&mut rx);
        let provider = ProviderId::new("contended-policy").unwrap();

        state.emit_event(DaemonEvent::ProviderPolicy {
            provider: provider.clone(),
            reason: "first restriction".to_string(),
        });
        state.emit_event(DaemonEvent::ProviderPolicy {
            provider: provider.clone(),
            reason: "second restriction".to_string(),
        });
        let message = tokio::time::timeout(Duration::from_millis(250), rx.recv())
            .await
            .expect("broadcast must not wait for the event-log writer")
            .expect("provider policy broadcast");
        assert!(matches!(
            message.payload,
            IpcPayload::Event(DaemonEvent::ProviderPolicy { ref reason, .. })
                if reason == "first restriction"
        ));
        let snapshot = state.event_log_snapshot().await;
        let reasons = snapshot
            .iter()
            .filter_map(|event| match &event.kind {
                spotuify_protocol::LoggedKind::ProviderPolicy {
                    provider: logged,
                    reason,
                } if logged == &provider => Some(reason.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(reasons, vec!["first restriction", "second restriction"]);

        shutdown_state(state).await;
    }

    #[tokio::test]
    async fn mark_auth_revoked_clears_cache_slot_and_emits_once() {
        let _guard = crate::ENV_LOCK.lock().await;
        let (_env, state) = test_spotify_auth_state().await;
        let mut rx = state.event_tx.subscribe();
        drain_events(&mut rx);

        *state.token_cache.lock().await = Some(stored_token());
        state.update_player_token(Some("stale-access".to_string()));

        let provider = ProviderId::new("spotify").expect("valid provider id");
        state
            .mark_auth_revoked(&ProviderError::AuthRevoked, Some(&provider))
            .await;

        assert!(state.auth_revoked());
        assert!(state.token_cache.lock().await.is_none());
        assert!(state.player_token_slot.read().is_none());
        recv_auth_error(&mut rx, AuthErrorKind::InvalidGrant, &provider).await;

        drain_events(&mut rx);
        state
            .mark_auth_revoked(&ProviderError::AuthRevoked, Some(&provider))
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let saw_second_auth = std::iter::from_fn(|| rx.try_recv().ok()).any(|msg| {
            matches!(
                msg.payload,
                IpcPayload::Event(DaemonEvent::AuthError {
                    kind: AuthErrorKind::InvalidGrant,
                    ..
                })
            )
        });
        assert!(!saw_second_auth, "AuthError should be one-shot per latch");

        shutdown_state(state).await;
    }

    #[tokio::test]
    async fn mark_auth_required_clears_cache_slot_and_emits_once() {
        let _guard = crate::ENV_LOCK.lock().await;
        let (_env, state) = test_spotify_auth_state().await;
        let mut rx = state.event_tx.subscribe();
        drain_events(&mut rx);

        *state.token_cache.lock().await = Some(stored_token());
        state.update_player_token(Some("stale-access".to_string()));

        let provider = ProviderId::new("spotify").expect("valid provider id");
        state.mark_auth_required(Some(&provider)).await;

        assert!(state.auth_required());
        assert!(state.token_cache.lock().await.is_none());
        assert!(state.player_token_slot.read().is_none());
        recv_auth_error(&mut rx, AuthErrorKind::NotLoggedIn, &provider).await;

        drain_events(&mut rx);
        state.mark_auth_required(Some(&provider)).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let saw_second_auth = std::iter::from_fn(|| rx.try_recv().ok()).any(|msg| {
            matches!(
                msg.payload,
                IpcPayload::Event(DaemonEvent::AuthError {
                    kind: AuthErrorKind::NotLoggedIn,
                    ..
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
    async fn runtime_registry_key_replacement_advances_provider_revision() {
        let _guard = crate::ENV_LOCK.lock().await;
        let (_env, state) = test_state().await;
        let mut revision = state.provider_revision_tx.subscribe();
        let provider = Arc::new(FakeProvider::isolated("revision-owner").unwrap());
        let runtime = ProviderRuntime::with_transport(provider.clone()).unwrap();
        let registry = Arc::new(
            ProviderRegistry::new(provider.id().clone(), [runtime]).expect("valid registry"),
        );

        state
            .install_provider_registry(ProviderRegistryKey::Fake, 0, registry.clone())
            .await
            .expect("initial install");
        revision.changed().await.expect("initial owner revision");
        assert_eq!(*revision.borrow(), 1);
        state
            .install_provider_registry(ProviderRegistryKey::Configured, 0, registry)
            .await
            .expect("replacement install");
        revision.changed().await.expect("provider revision change");
        assert_eq!(*revision.borrow(), 2);

        shutdown_state(state).await;
    }

    #[tokio::test]
    async fn injected_custom_player_installs_once_and_attributes_events_to_its_provider() {
        let _guard = crate::ENV_LOCK.lock().await;
        let env = TestEnv::new();
        let provider = Arc::new(FakeProvider::with_identity(
            ProviderId::new("custom-player").unwrap(),
            UriScheme::new("custom-media").unwrap(),
            spotuify_provider_fake::FakeDataset::Standard,
        ));
        let (backend, events) =
            spotuify_player::backends::mock::MockPlayerBackend::new_for_provider(
                provider.id().clone(),
                provider.uri_scheme().clone(),
            );
        let runtime = ProviderRuntime::with_player(
            provider.clone(),
            None,
            ProviderPlayer::new(Box::new(backend), events),
            TransportRecovery::RemoteOnly,
        )
        .unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());

        let (first, second) = tokio::join!(state.providers(), state.providers());
        first.expect("first registry install");
        second.expect("concurrent registry install");
        assert!(state.provider_owns_embedded_player(provider.id()));
        state
            .ensure_player_ready("custom-player-device")
            .await
            .expect("custom player ready");
        state
            .transport(TransportCmd::PlayUri {
                uri: "custom-media:track:track-1".to_string(),
                position_ms: 0,
            })
            .await
            .expect("custom player transport");
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let playback = state.snapshot_playback();
                if playback.item.as_ref().map(|item| item.uri.as_str())
                    == Some("custom-media:track:track-1")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("custom player event attribution");
        assert_eq!(
            state.active_transport_provider().as_ref(),
            Some(provider.id())
        );

        state.request_shutdown();
        state.shutdown_player().await;
        state.shutdown_search().await;
        state
            .shutdown_background_tasks(Duration::from_millis(100))
            .await;
        drop(env);
    }

    #[tokio::test]
    async fn prequeued_policy_event_keeps_stream_provider_and_sanitizes_reason() {
        let _guard = crate::ENV_LOCK.lock().await;
        let env = TestEnv::new();
        let provider = Arc::new(FakeProvider::with_identity(
            ProviderId::new("custom-policy").unwrap(),
            UriScheme::new("custom-media").unwrap(),
            spotuify_provider_fake::FakeDataset::Standard,
        ));
        let (backend, events) =
            spotuify_player::backends::mock::MockPlayerBackend::new_for_provider(
                provider.id().clone(),
                provider.uri_scheme().clone(),
            );
        let event_sender = backend.event_sender();
        let alpha_token = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
        event_sender
            .send(PlayerEvent::ProviderPolicy {
                reason: format!("region restricted for {alpha_token} {}", "🎵".repeat(600)),
            })
            .expect("queue policy before player handoff");
        let runtime = ProviderRuntime::with_player(
            provider.clone(),
            None,
            ProviderPlayer::new(Box::new(backend), events),
            TransportRecovery::RemoteOnly,
        )
        .unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = DaemonState::new_with_providers(registry).await.unwrap();
        let mut rx = state.event_tx.subscribe();

        state.providers().await.expect("install custom player");
        let (event_provider, reason) = recv_provider_policy(&mut rx).await;

        assert_eq!(&event_provider, provider.id());
        assert!(!reason.contains(alpha_token));
        assert!(reason.contains("<redacted>"));
        assert_eq!(
            reason.chars().count(),
            spotuify_protocol::PROVIDER_POLICY_REASON_MAX_CHARS
        );
        assert!(reason.ends_with('…'));

        shutdown_state(state).await;
        drop(env);
    }

    #[tokio::test]
    async fn inactive_player_policy_event_is_not_hidden_by_remote_transport_owner() {
        let _guard = crate::ENV_LOCK.lock().await;
        let env = TestEnv::new();
        let provider = Arc::new(FakeProvider::with_identity(
            ProviderId::new("policy-player").unwrap(),
            UriScheme::new("policy-media").unwrap(),
            spotuify_provider_fake::FakeDataset::Standard,
        ));
        let remote_owner = Arc::new(FakeProvider::isolated("remote-owner").unwrap());
        let (backend, events) =
            spotuify_player::backends::mock::MockPlayerBackend::new_for_provider(
                provider.id().clone(),
                provider.uri_scheme().clone(),
            );
        let event_sender = backend.event_sender();
        let player_runtime = ProviderRuntime::with_player(
            provider.clone(),
            None,
            ProviderPlayer::new(Box::new(backend), events),
            TransportRecovery::RemoteOnly,
        )
        .unwrap();
        let remote_runtime = ProviderRuntime::with_transport(remote_owner.clone()).unwrap();
        let registry =
            ProviderRegistry::new(provider.id().clone(), [player_runtime, remote_runtime]).unwrap();
        let state = DaemonState::new_with_providers(registry).await.unwrap();
        let mut rx = state.event_tx.subscribe();

        state.providers().await.expect("install custom player");
        state.set_active_transport_provider(remote_owner.id().clone());
        drain_events(&mut rx);
        event_sender
            .send(PlayerEvent::ProviderPolicy {
                reason: "account tier blocks local playback".to_string(),
            })
            .expect("emit policy from inactive player stream");

        let (event_provider, reason) = recv_provider_policy(&mut rx).await;
        assert_eq!(&event_provider, provider.id());
        assert_eq!(reason, "account tier blocks local playback");

        shutdown_state(state).await;
        drop(env);
    }

    #[tokio::test]
    async fn policy_command_error_emits_once_when_backend_also_emits_event() {
        let _guard = crate::ENV_LOCK.lock().await;
        let env = TestEnv::new();
        let provider = Arc::new(FakeProvider::isolated("policy-error").unwrap());
        let (mut backend, events) =
            spotuify_player::backends::mock::MockPlayerBackend::new_for_provider(
                provider.id().clone(),
                provider.uri_scheme().clone(),
            );
        let event_sender = backend.event_sender();
        backend.prime_play_uri_error(PlayerError::ProviderPolicy(
            "account tier blocks local playback".to_string(),
        ));
        backend.prime_volume_error(PlayerError::ProviderPolicy(
            "regional policy blocks local playback".to_string(),
        ));
        let runtime = ProviderRuntime::with_player(
            provider.clone(),
            None,
            ProviderPlayer::new(Box::new(backend), events),
            TransportRecovery::RemoteOnly,
        )
        .unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = DaemonState::new_with_providers(registry).await.unwrap();
        let mut rx = state.event_tx.subscribe();
        state.providers().await.expect("install custom player");
        state
            .ensure_player_ready("policy-device")
            .await
            .expect("register custom player");
        drain_events(&mut rx);

        let error = state
            .transport(TransportCmd::PlayUri {
                uri: format!("{}:track:track-1", provider.uri_scheme()),
                position_ms: 0,
            })
            .await
            .expect_err("policy error must reach caller");
        assert!(matches!(error, PlayerError::ProviderPolicy(_)));
        let (event_provider, reason) = recv_provider_policy(&mut rx).await;
        assert_eq!(&event_provider, provider.id());
        assert_eq!(reason, "account tier blocks local playback");
        drain_events(&mut rx);

        event_sender
            .send(PlayerEvent::ProviderPolicy {
                reason: "regional policy blocks local playback".to_string(),
            })
            .expect("emit paired backend policy event");
        let error = state
            .transport(TransportCmd::Volume { percent: 50 })
            .await
            .expect_err("paired policy error must reach caller");
        assert!(matches!(error, PlayerError::ProviderPolicy(_)));
        let (event_provider, reason) = recv_provider_policy(&mut rx).await;
        assert_eq!(&event_provider, provider.id());
        assert_eq!(reason, "regional policy blocks local playback");
        tokio::time::sleep(Duration::from_millis(50)).await;
        let duplicates = std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|message| {
                matches!(
                    message.payload,
                    IpcPayload::Event(DaemonEvent::ProviderPolicy { .. })
                )
            })
            .count();
        assert_eq!(duplicates, 0, "paired event and error must be deduplicated");

        shutdown_state(state).await;
        drop(env);
    }

    #[tokio::test]
    async fn stale_duplicate_cannot_replace_newer_active_policy_or_clear_identity() {
        let _guard = crate::ENV_LOCK.lock().await;
        let (_env, state) = test_state().await;
        let provider = ProviderId::new("spotify").unwrap();
        let mut rx = state.event_tx.subscribe();
        drain_events(&mut rx);

        assert!(state
            .player_policy_events
            .emit_for_provider(provider.clone(), "old restriction"));
        assert!(state
            .player_policy_events
            .emit_for_provider(provider.clone(), "new restriction"));
        assert!(!state
            .player_policy_events
            .emit_for_provider(provider.clone(), "old restriction"));
        assert_eq!(
            state.active_provider_policies(),
            vec![spotuify_protocol::ProviderPolicyNotice {
                provider: provider.clone(),
                reason: "new restriction".to_string(),
            }]
        );

        let barrier = state
            .player_policy_events
            .barrier_for_provider(provider.clone());
        assert!(state.player_policy_events.clear_if_unchanged(&barrier));
        let mut cleared = None;
        while let Ok(message) = rx.try_recv() {
            if let IpcPayload::Event(DaemonEvent::ProviderPolicyCleared { provider, reason }) =
                message.payload
            {
                cleared = Some((provider, reason));
            }
        }
        assert_eq!(cleared, Some((provider, "new restriction".to_string())));
        assert!(state.active_provider_policies().is_empty());

        shutdown_state(state).await;
    }

    #[tokio::test]
    async fn operation_success_cannot_clear_policy_newer_than_its_start_barrier() {
        let _guard = crate::ENV_LOCK.lock().await;
        let (_env, state) = test_state().await;
        let provider = ProviderId::new("spotify").unwrap();
        let mut rx = state.event_tx.subscribe();
        drain_events(&mut rx);

        assert!(state
            .player_policy_events
            .emit_for_provider(provider.clone(), "restriction A"));
        let operation_start = state
            .player_policy_events
            .barrier_for_provider(provider.clone());
        assert!(state
            .player_policy_events
            .emit_for_provider(provider.clone(), "restriction B"));

        assert!(
            !state
                .player_policy_events
                .clear_if_unchanged(&operation_start),
            "an older successful operation must not clear a newer policy"
        );
        assert_eq!(
            state.active_provider_policies(),
            vec![spotuify_protocol::ProviderPolicyNotice {
                provider: provider.clone(),
                reason: "restriction B".to_string(),
            }]
        );
        assert!(!std::iter::from_fn(|| rx.try_recv().ok()).any(|message| {
            matches!(
                message.payload,
                IpcPayload::Event(DaemonEvent::ProviderPolicyCleared {
                    provider: cleared_provider,
                    reason,
                }) if cleared_provider == provider && reason == "restriction B"
            )
        }));

        shutdown_state(state).await;
    }

    #[tokio::test]
    async fn ready_event_alone_does_not_clear_active_provider_policy() {
        let _guard = crate::ENV_LOCK.lock().await;
        let env = TestEnv::new();
        let provider = Arc::new(FakeProvider::isolated("policy-ready").unwrap());
        let (backend, events) =
            spotuify_player::backends::mock::MockPlayerBackend::new_for_provider(
                provider.id().clone(),
                provider.uri_scheme().clone(),
            );
        let event_sender = backend.event_sender();
        let runtime = ProviderRuntime::with_player(
            provider.clone(),
            None,
            ProviderPlayer::new(Box::new(backend), events),
            TransportRecovery::RemoteOnly,
        )
        .unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = DaemonState::new_with_providers(registry).await.unwrap();
        let mut rx = state.event_tx.subscribe();
        state.providers().await.expect("install custom player");
        state
            .ensure_player_ready("policy-device")
            .await
            .expect("register custom player");
        drain_events(&mut rx);

        event_sender
            .send(PlayerEvent::ProviderPolicy {
                reason: "restriction remains active".to_string(),
            })
            .expect("emit active policy");
        recv_provider_policy(&mut rx).await;
        event_sender
            .send(PlayerEvent::Ready {
                device_id: DeviceId::new("later-ready"),
                name: "Later Ready".to_string(),
            })
            .expect("emit later readiness");
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if matches!(
                    rx.recv().await.expect("ready event"),
                    IpcMessage {
                        payload: IpcPayload::Event(DaemonEvent::PlayerReady { ref name, .. }),
                        ..
                    } if name == "Later Ready"
                ) {
                    break;
                }
            }
        })
        .await
        .expect("ready event timeout");

        assert_eq!(
            state.active_provider_policies(),
            vec![spotuify_protocol::ProviderPolicyNotice {
                provider: provider.id().clone(),
                reason: "restriction remains active".to_string(),
            }]
        );

        shutdown_state(state).await;
        drop(env);
    }

    #[tokio::test]
    async fn policy_errors_from_register_command_emit_with_installed_provider() {
        let _guard = crate::ENV_LOCK.lock().await;
        let env = TestEnv::new();
        let provider = Arc::new(FakeProvider::isolated("policy-register").unwrap());
        let (mut backend, events) =
            spotuify_player::backends::mock::MockPlayerBackend::new_for_provider(
                provider.id().clone(),
                provider.uri_scheme().clone(),
            );
        backend.prime_register_device_error(PlayerError::ProviderPolicy(
            "registration policy denial".to_string(),
        ));
        let runtime = ProviderRuntime::with_player(
            provider.clone(),
            None,
            ProviderPlayer::new(Box::new(backend), events),
            TransportRecovery::RemoteOnly,
        )
        .unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = DaemonState::new_with_providers(registry).await.unwrap();
        let mut rx = state.event_tx.subscribe();

        state.providers().await.expect("install custom player");
        let error = state
            .ensure_player_ready("policy-device")
            .await
            .expect_err("registration policy must reach caller");
        assert!(error.to_string().contains("registration policy denial"));
        let (event_provider, reason) = recv_provider_policy(&mut rx).await;
        assert_eq!(&event_provider, provider.id());
        assert_eq!(reason, "registration policy denial");

        shutdown_state(state).await;
        drop(env);
    }

    #[tokio::test]
    async fn policy_errors_from_reconnect_resume_and_warm_paths_emit() {
        let _guard = crate::ENV_LOCK.lock().await;
        let env = TestEnv::new();
        let provider = Arc::new(FakeProvider::isolated("policy-background").unwrap());
        let (mut backend, events) =
            spotuify_player::backends::mock::MockPlayerBackend::new_for_provider(
                provider.id().clone(),
                provider.uri_scheme().clone(),
            );
        backend.prime_play_uri_error(PlayerError::ProviderPolicy(
            "resume policy denial".to_string(),
        ));
        backend.prime_preload_uri_error(PlayerError::ProviderPolicy(
            "preload policy denial".to_string(),
        ));
        let runtime = ProviderRuntime::with_player(
            provider.clone(),
            None,
            ProviderPlayer::new(Box::new(backend), events),
            TransportRecovery::RemoteOnly,
        )
        .unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = DaemonState::new_with_providers(registry).await.unwrap();
        let mut rx = state.event_tx.subscribe();
        state.providers().await.expect("install custom player");
        let track_uri = format!("{}:track:track-1", provider.uri_scheme());

        let (resp, response) = oneshot::channel();
        state
            .player_tx
            .send(PlayerCommand::Reconnect {
                name: "policy-device".to_string(),
                resume: Some((track_uri.clone(), 123)),
                resp,
            })
            .await
            .expect("dispatch reconnect");
        let error = response
            .await
            .expect("reconnect response")
            .expect_err("resume policy must reach caller");
        assert!(matches!(error, PlayerError::ProviderPolicy(_)));
        let (event_provider, reason) = recv_provider_policy(&mut rx).await;
        assert_eq!(&event_provider, provider.id());
        assert_eq!(reason, "resume policy denial");

        state.prewarm_next_audio(&track_uri);
        let (event_provider, reason) = recv_provider_policy(&mut rx).await;
        assert_eq!(&event_provider, provider.id());
        assert_eq!(reason, "preload policy denial");

        shutdown_state(state).await;
        drop(env);
    }

    #[tokio::test]
    async fn failed_event_stream_install_restores_registry_player_for_retry() {
        let _guard = crate::ENV_LOCK.lock().await;
        let env = TestEnv::new();
        let provider = Arc::new(FakeProvider::isolated("rollback-player").unwrap());
        let (backend, events) =
            spotuify_player::backends::mock::MockPlayerBackend::new_for_provider(
                provider.id().clone(),
                provider.uri_scheme().clone(),
            );
        let runtime = ProviderRuntime::with_player(
            provider.clone(),
            None,
            ProviderPlayer::new(Box::new(backend), events),
            TransportRecovery::RemoteOnly,
        )
        .unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = DaemonState::new_with_providers(registry).await.unwrap();

        let worker = state
            .player_worker
            .lock()
            .await
            .take()
            .expect("player event worker");
        worker.abort();
        let _ = worker.await;

        for attempt in 1..=2 {
            let error = match state.providers().await {
                Ok(_) => panic!("closed event worker must reject player install"),
                Err(error) => error,
            };
            assert!(
                error.to_string().contains("player event worker stopped"),
                "attempt {attempt} should retry the restored player, got {error:#}"
            );
            assert!(!error
                .to_string()
                .contains("consumed without completing installation"));
        }

        state.request_shutdown();
        state.shutdown_player().await;
        state.shutdown_search().await;
        state
            .shutdown_background_tasks(Duration::from_millis(100))
            .await;
        drop(env);
    }

    #[tokio::test]
    async fn registry_replacement_rehomes_removed_active_transport_owner() {
        let _guard = crate::ENV_LOCK.lock().await;
        let (_env, state) = test_state().await;
        let default = Arc::new(FakeProvider::isolated("owner-a").unwrap());
        let removed = Arc::new(FakeProvider::isolated("owner-b").unwrap());
        let initial = Arc::new(
            ProviderRegistry::new(
                default.id().clone(),
                [
                    ProviderRuntime::with_transport(default.clone()).unwrap(),
                    ProviderRuntime::with_transport(removed.clone()).unwrap(),
                ],
            )
            .unwrap(),
        );
        state.set_active_transport_provider(removed.id().clone());
        state
            .install_provider_registry(ProviderRegistryKey::Fake, 0, initial)
            .await
            .unwrap();
        assert_eq!(
            state.active_transport_provider().as_ref(),
            Some(removed.id())
        );

        let replacement = Arc::new(
            ProviderRegistry::new(
                default.id().clone(),
                [ProviderRuntime::with_transport(default.clone()).unwrap()],
            )
            .unwrap(),
        );
        let before = state.current_mutation_seq();
        state
            .install_provider_registry(ProviderRegistryKey::Configured, 0, replacement)
            .await
            .unwrap();

        assert_eq!(
            state.active_transport_provider().as_ref(),
            Some(default.id())
        );
        assert!(state.current_mutation_seq() > before);
        let seq = state.current_mutation_seq();
        assert!(
            <DaemonState as spotuify_sync::SyncContext>::may_apply_transport_update(
                &state,
                default.id(),
                seq,
            )
        );

        shutdown_state(state).await;
    }

    #[tokio::test]
    async fn stale_playback_poll_cannot_persist_after_newer_transport_result() {
        let _guard = crate::ENV_LOCK.lock().await;
        let (_env, state) = test_state().await;
        let state = Arc::new(state);
        let provider = FakeProvider::from_env().unwrap().id().clone();
        state.set_active_transport_provider(provider.clone());
        let captured_seq = state.current_mutation_seq();
        let stale = Playback {
            item: Some(MediaItem {
                uri: "fake:track:old".to_string(),
                name: "Old".to_string(),
                kind: MediaKind::Track,
                ..Default::default()
            }),
            is_playing: true,
            ..Default::default()
        };
        let current = Playback {
            item: Some(MediaItem {
                uri: "fake:track:new".to_string(),
                name: "New".to_string(),
                kind: MediaKind::Track,
                ..Default::default()
            }),
            is_playing: false,
            ..Default::default()
        };

        let lane = state.transport_mutation_lock.clone().lock_owned().await;
        let poll_state = state.clone();
        let poll_provider = provider.clone();
        let poll = tokio::spawn(async move {
            <DaemonState as spotuify_sync::SyncContext>::prepare_and_persist_playback_poll_if_current(
                &poll_state,
                &poll_provider,
                &stale,
                captured_seq,
                spotuify_core::now_ms(),
                None,
            )
            .await
        });
        tokio::task::yield_now().await;
        state.bump_mutation_seq();
        state
            .store
            .persist_provider_playback(&provider, &current)
            .await
            .unwrap();
        drop(lane);

        assert_eq!(poll.await.unwrap().unwrap(), None);
        let latest = state.store.latest_playback().await.unwrap().unwrap();
        assert_eq!(
            latest.item.as_ref().map(|item| item.uri.as_str()),
            Some("fake:track:new")
        );

        let state = Arc::try_unwrap(state).ok().expect("test owns daemon state");
        shutdown_state(state).await;
    }

    #[tokio::test]
    async fn reload_auth_clears_cache_and_keeps_missing_credentials_gated() {
        let _guard = crate::ENV_LOCK.lock().await;
        let (_env, state) = test_spotify_auth_state().await;

        *state.token_cache.lock().await = Some(stored_token());
        state.update_player_token(Some("stale-access".to_string()));
        state
            .auth_revoked
            .store(true, std::sync::atomic::Ordering::Release);
        state
            .auth_required
            .store(true, std::sync::atomic::Ordering::Release);

        state.reload_auth(None).await.expect("reload auth");

        assert!(!state.auth_revoked());
        assert!(state.auth_required());
        assert!(state.token_cache.lock().await.is_none());
        assert!(state.player_token_slot.read().is_none());

        shutdown_state(state).await;
    }

    #[tokio::test]
    async fn reload_auth_targets_secondary_spotify_behind_no_auth_default() {
        let _guard = crate::ENV_LOCK.lock().await;
        let env = TestEnv::new();
        env.use_fake_default_with_secondary_spotify();
        spotuify_spotify::auth::save_dev_app_token_for("custom-cloud", &stored_token())
            .expect("seed secondary Spotify credential");
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));

        *state.token_cache.lock().await = Some(stored_token());
        state.auth_revoked.store(true, Ordering::Release);
        state.auth_required.store(true, Ordering::Release);

        let response = crate::handlers::admin::dispatch(state.clone(), Request::ReloadAuth, None)
            .await
            .expect("reload secondary Spotify auth");

        assert!(matches!(response, ResponseData::Ack { .. }));
        assert!(!state.auth_revoked());
        assert!(!state.auth_required());
        assert!(
            state.token_cache.lock().await.is_none(),
            "reload must clear the secondary Spotify token cache"
        );
        let target = state
            .configured_health_auth_target()
            .await
            .expect("configured health auth target");
        assert_eq!(target.provider_id.as_str(), "custom-cloud");

        state.request_shutdown();
        state.shutdown_player().await;
        state.shutdown_search().await;
        state
            .shutdown_background_tasks(Duration::from_millis(100))
            .await;
    }

    #[cfg(feature = "embedded-playback")]
    #[tokio::test]
    async fn provider_registry_remains_discoverable_while_auth_revoked() {
        let _guard = crate::ENV_LOCK.lock().await;
        let (_env, state) = test_spotify_auth_state().await;
        state.auth_revoked.store(true, Ordering::Release);

        let registry = state
            .providers()
            .await
            .expect("provider catalog remains usable");
        assert_eq!(registry.default_id().as_str(), "spotify");
        assert!(state.auth_revoked());

        shutdown_state(state).await;
    }

    #[cfg(feature = "embedded-playback")]
    #[tokio::test]
    async fn provider_registry_remains_discoverable_while_auth_required() {
        let _guard = crate::ENV_LOCK.lock().await;
        let (_env, state) = test_spotify_auth_state().await;
        state.auth_required.store(true, Ordering::Release);

        let registry = state
            .providers()
            .await
            .expect("provider catalog remains usable");
        assert_eq!(registry.default_id().as_str(), "spotify");
        assert!(state.auth_required());

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

    #[test]
    fn reconnect_backoff_is_exponential_and_capped() {
        use super::{reconnect_backoff, PLAYER_RECONNECT_MAX_BACKOFF};
        use std::time::Duration;

        // First drop (0/1 failures) keeps the historical 1s.
        assert_eq!(reconnect_backoff(0), Duration::from_secs(1));
        assert_eq!(reconnect_backoff(1), Duration::from_secs(1));
        // Then doubles: 2→2s, 3→4s, 4→8s, 5→16s.
        assert_eq!(reconnect_backoff(2), Duration::from_secs(2));
        assert_eq!(reconnect_backoff(3), Duration::from_secs(4));
        assert_eq!(reconnect_backoff(4), Duration::from_secs(8));
        assert_eq!(reconnect_backoff(5), Duration::from_secs(16));
        // Caps at the ceiling and never overflows/panics for huge inputs.
        assert_eq!(reconnect_backoff(6), PLAYER_RECONNECT_MAX_BACKOFF);
        assert_eq!(reconnect_backoff(100), PLAYER_RECONNECT_MAX_BACKOFF);
        assert_eq!(reconnect_backoff(u32::MAX), PLAYER_RECONNECT_MAX_BACKOFF);
        // Monotone non-decreasing.
        let mut prev = Duration::ZERO;
        for n in 0..40u32 {
            let b = reconnect_backoff(n);
            assert!(b >= prev, "backoff decreased at n={n}");
            prev = b;
        }
    }

    #[test]
    fn classify_audio_flow_covers_all_verdicts() {
        use super::{classify_audio_flow, AudioFlowVerdict};
        let thr = 6_000;

        // Not playing → inert regardless of counter.
        assert_eq!(
            classify_audio_flow(false, Some(100), Some(100), 99_999, thr),
            AudioFlowVerdict::NotPlaying
        );
        // No counter (non-embedded backend) → inert.
        assert_eq!(
            classify_audio_flow(true, None, None, 0, thr),
            AudioFlowVerdict::NotPlaying
        );
        // First observation while playing → assume flowing.
        assert_eq!(
            classify_audio_flow(true, Some(0), None, 0, thr),
            AudioFlowVerdict::Flowing
        );
        // Counter advanced → flowing.
        assert_eq!(
            classify_audio_flow(true, Some(200), Some(100), 4_000, thr),
            AudioFlowVerdict::Flowing
        );
        // Counter DECREASED (sink reset on a fresh start) → flowing, NOT stalled.
        assert_eq!(
            classify_audio_flow(true, Some(5), Some(900_000), 9_999, thr),
            AudioFlowVerdict::Flowing
        );
        // Flat while playing, within grace → buffering (tolerate).
        assert_eq!(
            classify_audio_flow(true, Some(100), Some(100), 4_000, thr),
            AudioFlowVerdict::Buffering
        );
        // Flat while playing, past grace → stalled.
        assert_eq!(
            classify_audio_flow(true, Some(100), Some(100), 6_000, thr),
            AudioFlowVerdict::Stalled
        );
    }

    #[test]
    fn resume_target_only_when_playing_on_our_device() {
        use super::resume_target_after_drop;
        use spotuify_core::{Device, MediaItem, Playback};

        let own = "ourdevice";
        let device = |id: &str| Device {
            id: Some(id.to_string()),
            name: "d".to_string(),
            kind: "Speaker".to_string(),
            is_active: true,
            is_restricted: false,
            volume_percent: None,
            supports_volume: true,
        };
        let item = MediaItem {
            uri: "spotify:track:abc".to_string(),
            ..Default::default()
        };
        let playing_on = |dev: Option<Device>| Playback {
            item: Some(item.clone()),
            device: dev,
            is_playing: true,
            progress_ms: 42_000,
            ..Default::default()
        };

        let own_name = "spotuify-hume";

        // Playing on our device → resume at the dropped position.
        assert_eq!(
            resume_target_after_drop(&playing_on(Some(device(own))), Some(own), Some(own_name)),
            Some(("spotify:track:abc".to_string(), 42_000))
        );
        // Silent drop with no recorded device → assume ours, resume.
        assert_eq!(
            resume_target_after_drop(&playing_on(None), Some(own), Some(own_name)),
            Some(("spotify:track:abc".to_string(), 42_000))
        );
        // Handed off to a different active device → do NOT steal playback back.
        assert_eq!(
            resume_target_after_drop(
                &playing_on(Some(device("phone"))),
                Some(own),
                Some(own_name)
            ),
            None
        );
        // Handed off to a device that reports NO id (car head units do this) —
        // it is still a foreign device, matched by name. Regression for
        // 2026-06-29: id-less car playback classified as "assume ours" made
        // stall recovery steal the car's session.
        let car = Device {
            id: None,
            name: "My Car".to_string(),
            kind: "CarThing".to_string(),
            is_active: true,
            is_restricted: true,
            volume_percent: None,
            supports_volume: false,
        };
        assert_eq!(
            resume_target_after_drop(&playing_on(Some(car)), Some(own), Some(own_name)),
            None
        );
        // An id-less device whose name IS ours (id lost in a lossy payload) →
        // still ours, resume.
        let own_by_name = Device {
            id: None,
            name: own_name.to_string(),
            kind: "Speaker".to_string(),
            is_active: true,
            is_restricted: false,
            volume_percent: None,
            supports_volume: true,
        };
        assert_eq!(
            resume_target_after_drop(&playing_on(Some(own_by_name)), Some(own), Some(own_name)),
            Some(("spotify:track:abc".to_string(), 42_000))
        );
        // Paused at drop time → nothing to resume (matches the chosen policy).
        let mut paused = playing_on(Some(device(own)));
        paused.is_playing = false;
        assert_eq!(
            resume_target_after_drop(&paused, Some(own), Some(own_name)),
            None
        );
        // Playing but no known track URI → nothing actionable.
        let mut no_uri = playing_on(Some(device(own)));
        no_uri.item = None;
        assert_eq!(
            resume_target_after_drop(&no_uri, Some(own), Some(own_name)),
            None
        );
    }

    #[tokio::test]
    async fn reactivation_and_manual_reset_clear_reconnect_give_up() {
        let _guard = crate::ENV_LOCK.lock().await;
        let (_env, state) = test_state().await;
        // Simulate the health loop having latched into "gave up" after repeated
        // failed probes.
        {
            let mut health = state.player_health.lock();
            health.gave_up = true;
            health.consecutive_failures = super::PLAYER_RECONNECT_GIVE_UP_AFTER + 1;
        }

        // Re-activating this device (false -> true transition) must forgive the
        // give-up so the backstop resumes.
        state.set_we_are_active(true);
        {
            let health = state.player_health.lock();
            assert!(!health.gave_up, "re-activation must clear gave_up");
            assert_eq!(
                health.consecutive_failures, 0,
                "re-activation must zero the failure count"
            );
        }

        // `reset_give_up` reports whether anything was latched: true once, then
        // false (idempotent).
        {
            let mut health = state.player_health.lock();
            health.gave_up = true;
        }
        assert!(state.reset_give_up(), "first reset clears the latch");
        assert!(!state.reset_give_up(), "second reset is a no-op");

        shutdown_state(state).await;
    }

    #[tokio::test]
    async fn audio_stall_recovery_fires_only_when_device_is_wanted() {
        let _guard = crate::ENV_LOCK.lock().await;
        let (_env, state) = test_state().await;
        state.providers().await.expect("install fake player");

        // Not active and nothing to resume → the watchdog must not reconnect.
        state.set_we_are_active(false);
        assert!(
            !state.trigger_audio_stall_recovery(1_000),
            "stall recovery must not fire when the device is not wanted"
        );

        // Active → schedule a reconnect and record the attempt. `schedule_player_
        // reconnect` flips `reconnect_in_flight` synchronously (swap before the
        // spawn), so the recovery is observable immediately.
        state.set_we_are_active(true);
        let before = state.player_health_snapshot().reconnect_attempts;
        assert!(
            state.trigger_audio_stall_recovery(2_000),
            "stall recovery must fire when the device is active"
        );
        let after = state.player_health_snapshot().reconnect_attempts;
        assert_eq!(after, before + 1, "a reconnect attempt must be recorded");

        // A reconnect is now in flight → a duplicate stall must be suppressed.
        assert!(
            !state.trigger_audio_stall_recovery(2_500),
            "a reconnect already in flight must suppress a duplicate recovery"
        );

        shutdown_state(state).await;
    }

    #[tokio::test]
    async fn secondary_transport_never_reconnects_default_player_with_foreign_playback() {
        let _guard = crate::ENV_LOCK.lock().await;
        let (_env, state) = test_state().await;
        state.providers().await.expect("install fake player");
        state.set_active_transport_provider(
            ProviderId::new("secondary-remote").expect("valid provider id"),
        );
        state.playback_clock.seed_from_cache(
            spotuify_core::Playback {
                item: Some(MediaItem {
                    uri: "secondary:track:foreign".to_string(),
                    kind: spotuify_core::MediaKind::Track,
                    ..Default::default()
                }),
                is_playing: true,
                device: None,
                ..Default::default()
            },
            spotuify_core::PlaybackStateSource::Cache,
            1_000,
        );
        state.set_we_are_active(true);
        let before = state.player_health_snapshot().reconnect_attempts;

        assert!(!state.embedded_owns_global_transport());
        assert!(!state.trigger_audio_stall_recovery(2_000));
        state.probe_player_health(2_000).await;
        assert_eq!(
            state.player_health_snapshot().reconnect_attempts,
            before,
            "secondary playback must not schedule the default provider player"
        );

        shutdown_state(state).await;
    }

    #[tokio::test]
    async fn idless_foreign_device_clears_we_are_active_and_reads_foreign() {
        // Regression for 2026-06-29: car head units report `device.id: null`
        // in /me/player. The old id-only match early-returned, leaving
        // `we_are_active` stale-true, and the audio-flow watchdog then stole
        // the car's playback onto this machine.
        use spotuify_core::{Device, Playback};

        let _guard = crate::ENV_LOCK.lock().await;
        let (_env, state) = test_state().await;
        *state.own_device_name.lock() = Some("spotuify-hume".to_string());

        let car = Device {
            id: None,
            name: "My Car".to_string(),
            kind: "CarThing".to_string(),
            is_active: true,
            is_restricted: true,
            volume_percent: None,
            supports_volume: false,
        };
        let playing_in_car = Playback {
            device: Some(car),
            is_playing: true,
            ..Default::default()
        };

        state.set_we_are_active(true);
        state.note_active_device(&playing_in_car);
        assert!(
            !state.is_we_are_active(),
            "an id-less foreign device must clear we_are_active"
        );
        assert!(
            state.active_device_is_foreign(&playing_in_car),
            "an id-less foreign device must read as foreign for the watchdog"
        );

        // Our own device matched by name (id-less payload) → ours, not foreign.
        let own_by_name = Device {
            id: None,
            name: "spotuify-hume".to_string(),
            kind: "Speaker".to_string(),
            is_active: true,
            is_restricted: false,
            volume_percent: None,
            supports_volume: true,
        };
        let playing_here = Playback {
            device: Some(own_by_name),
            is_playing: true,
            ..Default::default()
        };
        state.note_active_device(&playing_here);
        assert!(
            state.is_we_are_active(),
            "our own device matched by name must set we_are_active"
        );
        assert!(!state.active_device_is_foreign(&playing_here));

        // Unknown device (None) is not provably foreign and leaves the flag.
        let unknown = Playback {
            is_playing: true,
            ..Default::default()
        };
        state.note_active_device(&unknown);
        assert!(state.is_we_are_active(), "None device must leave the flag");
        assert!(!state.active_device_is_foreign(&unknown));

        shutdown_state(state).await;
    }

    #[tokio::test]
    async fn own_device_always_present_in_merged_device_list() {
        // Regression for 2026-07-06: after a session drop with another
        // device active, the daemon deliberately idles the player — but
        // `connected_own_device` returning None made spotuify-hume vanish
        // from every client's device list, so nothing could transfer
        // playback back without a manual `spotuify reconnect`. (The test
        // binary builds without `embedded-playback`, which otherwise
        // seeds this name at construction, so set it explicitly.)
        let _guard = crate::ENV_LOCK.lock().await;
        let (_env, state) = test_state().await;
        *state.own_device_name.lock() = Some("spotuify-hume".to_string());
        assert!(
            !state.player_is_connected().await,
            "precondition: embedded player is not connected in this test"
        );

        let entry = state
            .own_device_entry()
            .await
            .expect("own device entry must exist even while the player is idle");
        assert_eq!(entry.name, "spotuify-hume");
        assert!(
            !entry.is_active,
            "an idle own device is listed but inactive"
        );

        let provider = state.providers().await.unwrap().default_id().clone();
        let devices = crate::handler::cached_devices_with_own_device(&state, &provider)
            .await
            .expect("device list");
        assert!(
            devices.iter().any(|d| d.id == entry.id),
            "own device must appear in the merged device list"
        );
        // The transfer handler's reconnect-on-demand gate must recognise
        // the synthesized entry as ours.
        assert!(
            state.device_is_ours(&entry),
            "synthesized own-device entry must match device_is_ours"
        );

        shutdown_state(state).await;
    }

    #[tokio::test]
    async fn fake_backend_keeps_audio_watchdog_inert() {
        // The fake/mock backend exposes no audio counter, so `audio_samples()`
        // is None and `classify_audio_flow` returns NotPlaying every tick — the
        // watchdog stays inert and smoke.sh can never trigger a spurious
        // stall-reconnect. This guard documents that invariant: if a future
        // change gives the mock a live counter, re-check the false-stall risk
        // (a flat counter while the clock reads playing would fire recovery).
        let _guard = crate::ENV_LOCK.lock().await;
        let (_env, state) = test_state().await;
        assert_eq!(
            state.audio_samples(),
            None,
            "fake backend must expose no audio counter"
        );
        assert!(
            state.status().audio_health.is_none(),
            "no counter → no audio_health surfaced"
        );
        shutdown_state(state).await;
    }
}

#[cfg(test)]
mod phase_9_1_translate {
    #![allow(clippy::panic, clippy::unwrap_used)]

    //! Phase 9.1 — PlayerEvent → DaemonEvent translation. Pure
    //! function, no daemon spin-up needed. Adversarial: assert each
    //! lifecycle event maps to exactly one DaemonEvent with all
    //! fields preserved, and every playback-progress event maps to
    //! None so the wire bus stays clean during 9.1 (Phase 9.3 wires
    //! a richer position event).

    use super::translate_player_event;
    use spotuify_core::{ProviderId, ResourceUri};
    use spotuify_player::{DeviceId, PlayerEvent};
    use spotuify_protocol::DaemonEvent;

    fn provider() -> ProviderId {
        ProviderId::new("nebula").expect("valid provider id")
    }

    fn translate(event: PlayerEvent) -> Option<DaemonEvent> {
        translate_player_event(event, &provider())
    }

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
        let translated = translate(PlayerEvent::Ready {
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
        let translated = translate(PlayerEvent::Degraded {
            reason: "spirc-timeout".to_string(),
        })
        .expect("Degraded must translate");
        assert!(
            matches!(translated, DaemonEvent::PlayerDegraded { ref reason } if reason == "spirc-timeout"),
            "got {translated:?}"
        );
    }

    #[test]
    fn provider_policy_translates_with_installed_provider_and_redacted_reason() {
        let raw_token = "OWZhZWQzM2QtNjI1NC00MzEwLWFhZGMTNzEzZjBjMjM2U2VjcmV0MTIz";
        let translated = translate(PlayerEvent::ProviderPolicy {
            reason: format!("region restricted for {raw_token}"),
        })
        .expect("ProviderPolicy must translate");
        assert!(matches!(
            translated,
            DaemonEvent::ProviderPolicy { provider, reason }
                if provider.as_str() == "nebula"
                    && reason.contains("<redacted>")
                    && !reason.contains(raw_token)
        ));
    }

    #[test]
    fn session_disconnected_translates_with_reason() {
        let translated = translate(PlayerEvent::SessionDisconnected {
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
        let translated = translate(PlayerEvent::Failed {
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
                    uri: ResourceUri::parse("spotify:track:abc").unwrap(),
                    position_ms: 0,
                },
                "started spotify:track:abc",
            ),
            (PlayerEvent::PlaybackPaused, "paused"),
            (PlayerEvent::PlaybackResumed, "resumed"),
            (
                PlayerEvent::TrackChanged {
                    uri: ResourceUri::parse("spotify:track:def").unwrap(),
                    position_ms: 0,
                },
                "track changed spotify:track:def",
            ),
            (
                PlayerEvent::EndOfTrack {
                    uri: ResourceUri::parse("spotify:track:ghi").unwrap(),
                },
                "ended spotify:track:ghi",
            ),
        ];

        for (event, expected) in cases {
            let translated = translate(event).expect("playback event should emit");
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
                uri: ResourceUri::parse("spotify:track:abc").unwrap(),
            },
        ] {
            assert!(
                translate(event.clone()).is_none(),
                "{event:?} should not produce a broadcast event"
            );
        }
    }
}
