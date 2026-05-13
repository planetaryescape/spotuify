use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use crate::analytics::{AnalyticsSource, AnalyticsStore};
use crate::config::Config;
use crate::protocol::{DaemonStatus, IpcMessage, IPC_PROTOCOL_VERSION};
use crate::search::{SearchIndex, SearchServiceHandle};
use crate::spotify::SpotifyClient;
use crate::store::Store;

pub(crate) struct DaemonState {
    started_at: Instant,
    shutdown_tx: watch::Sender<bool>,
    pub(crate) event_tx: broadcast::Sender<IpcMessage>,
    store: Store,
    search: SearchServiceHandle,
    search_worker: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}

impl DaemonState {
    pub(crate) async fn new() -> Result<Self> {
        let (shutdown_tx, _) = watch::channel(false);
        let (event_tx, _) = broadcast::channel(128);
        let store = Store::open_default().await?;
        let (search, search_worker) =
            SearchServiceHandle::start(SearchIndex::open(store.index_path())?);
        Ok(Self {
            started_at: Instant::now(),
            shutdown_tx,
            event_tx,
            store,
            search,
            search_worker: tokio::sync::Mutex::new(Some(search_worker)),
        })
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
            daemon_build_id: Some(crate::daemon::server::current_build_id()),
        }
    }

    pub(crate) async fn spotify_client(&self) -> Result<SpotifyClient> {
        let config = Config::load().context("failed to load Spotify config")?;
        let client = SpotifyClient::new(config)?;
        match AnalyticsStore::open_default().await {
            Ok(store) => Ok(client.with_analytics(store, AnalyticsSource::Daemon)),
            Err(err) => {
                tracing::warn!(error = %err, "analytics store unavailable");
                Ok(client)
            }
        }
    }
}
