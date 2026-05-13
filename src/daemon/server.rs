use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result};
use futures::{FutureExt, SinkExt, StreamExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::codec::Framed;

use crate::daemon::handler::handle_request;
use crate::daemon::ipc_client::IpcClient;
use crate::daemon::state::DaemonState;
use crate::protocol::{
    DaemonEvent, DaemonStatus, IpcCodec, IpcMessage, IpcPayload, Request, Response, ResponseData,
    IPC_PROTOCOL_VERSION,
};
use crate::{config::Config, spotifyd};

const REQUEST_CONCURRENCY_LIMIT: usize = 64;
const CONNECTION_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const STATUS_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const START_DAEMON_TIMEOUT: Duration = Duration::from_secs(5);
const SOCKET_PROBE_ATTEMPTS: usize = 5;
const SOCKET_PROBE_DELAY: Duration = Duration::from_millis(100);

pub async fn run_daemon() -> Result<()> {
    let socket_path = DaemonState::socket_path();
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    match inspect_socket_state(&socket_path).await {
        SocketState::Reachable => anyhow::bail!(
            "daemon already running at {}. Try `spotuify daemon status`.",
            socket_path.display()
        ),
        SocketState::Stale => {
            let _ = std::fs::remove_file(&socket_path);
            clear_daemon_pid_file();
        }
        SocketState::Missing => {}
    }

    ensure_player_process_started();

    let state = Arc::new(DaemonState::new().await?);
    crate::sync::spawn_background_scheduler(state.clone());
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind {}", socket_path.display()))?;
    write_daemon_pid_file()?;
    tracing::info!(socket = %socket_path.display(), "spotuify daemon listening");

    let request_semaphore = Arc::new(Semaphore::new(REQUEST_CONCURRENCY_LIMIT));
    let mut shutdown_rx = state.shutdown_receiver();
    let mut connections = JoinSet::new();

    loop {
        tokio::select! {
            joined = connections.join_next(), if !connections.is_empty() => {
                if let Some(Err(err)) = joined {
                    tracing::warn!(error = %err, "daemon client task failed");
                }
            }
            changed = shutdown_rx.changed() => {
                if changed.is_ok() && *shutdown_rx.borrow_and_update() {
                    tracing::info!("daemon shutdown requested");
                    break;
                }
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let connection_state = state.clone();
                let request_semaphore = request_semaphore.clone();
                let event_rx = state.event_tx.subscribe();
                let connection_shutdown_rx = state.shutdown_receiver();
                connections.spawn(async move {
                    serve_client_connection(
                        stream,
                        connection_state,
                        request_semaphore,
                        event_rx,
                        connection_shutdown_rx,
                    ).await;
                });
            }
        }
    }

    let _ = state.event_tx.send(IpcMessage {
        id: 0,
        payload: IpcPayload::Event(DaemonEvent::ShutdownRequested),
    });
    state.shutdown_search().await;
    drop(listener);
    drain_connection_tasks(&mut connections, CONNECTION_DRAIN_TIMEOUT).await;
    let _ = std::fs::remove_file(&socket_path);
    clear_daemon_pid_file();
    Ok(())
}

fn ensure_player_process_started() {
    match Config::load() {
        Ok(config) => {
            if let Err(err) = spotifyd::ensure_started(&config) {
                tracing::warn!(error = %err, "failed to ensure spotifyd is started");
            }
        }
        Err(err) => tracing::warn!(error = %err, "skipping spotifyd startup; config unavailable"),
    }
}

async fn serve_client_connection(
    stream: UnixStream,
    state: Arc<DaemonState>,
    request_semaphore: Arc<Semaphore>,
    mut event_rx: tokio::sync::broadcast::Receiver<IpcMessage>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    let (mut sink, mut stream) = Framed::new(stream, IpcCodec::new()).split();
    let mut request_tasks = JoinSet::new();
    let mut accept_requests = true;
    let mut can_send = true;
    let mut shutdown_requested = false;

    loop {
        tokio::select! {
            biased;
            joined = request_tasks.join_next(), if !request_tasks.is_empty() => {
                match joined {
                    Some(Ok(response)) if can_send => {
                        if sink.send(response).await.is_err() {
                            can_send = false;
                            accept_requests = false;
                        }
                    }
                    Some(Err(err)) => tracing::warn!(error = %err, "IPC request task failed"),
                    _ => {}
                }
            }
            changed = shutdown_rx.changed(), if !shutdown_requested => {
                match changed {
                    Ok(()) if *shutdown_rx.borrow_and_update() => {
                        shutdown_requested = true;
                        accept_requests = false;
                    }
                    Ok(()) => {}
                    Err(_) => {
                        shutdown_requested = true;
                        accept_requests = false;
                    }
                }
            }
            event = event_rx.recv(), if can_send => {
                match event {
                    Ok(event) => {
                        if sink.send(event).await.is_err() {
                            can_send = false;
                            accept_requests = false;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        can_send = false;
                    }
                }
            }
            message = stream.next(), if accept_requests => {
                match message {
                    Some(Ok(message)) => {
                        let Ok(permit) = request_semaphore.clone().acquire_owned().await else {
                            accept_requests = false;
                            continue;
                        };
                        let state = state.clone();
                        request_tasks.spawn(async move {
                            let _permit = permit;
                            guard_ipc_response(message.id, state, message.payload).await
                        });
                    }
                    Some(Err(err)) => {
                        tracing::warn!(error = %err, "failed to read IPC frame");
                        accept_requests = false;
                    }
                    None => break,
                }
            }
            else => break,
        }

        if shutdown_requested && request_tasks.is_empty() {
            break;
        }
        if !accept_requests && !can_send {
            break;
        }
    }
}

async fn guard_ipc_response(
    message_id: u64,
    state: Arc<DaemonState>,
    payload: IpcPayload,
) -> IpcMessage {
    let response = match payload {
        IpcPayload::Request(request) => match AssertUnwindSafe(handle_request(state, request))
            .catch_unwind()
            .await
        {
            Ok(response) => response,
            Err(_) => Response::error("IPC handler panicked"),
        },
        _ => Response::error("IPC frame was not a request"),
    };

    IpcMessage {
        id: message_id,
        payload: IpcPayload::Response(response),
    }
}

async fn drain_connection_tasks(connections: &mut JoinSet<()>, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    while !connections.is_empty() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        if tokio::time::timeout(remaining, connections.join_next())
            .await
            .is_err()
        {
            break;
        }
    }
    connections.abort_all();
}

pub async fn start_daemon(foreground: bool) -> Result<Option<DaemonStatus>> {
    if foreground {
        run_daemon().await?;
        return Ok(None);
    }

    let socket_path = DaemonState::socket_path();
    match inspect_socket_state(&socket_path).await {
        SocketState::Reachable => return daemon_status().await.map(Some),
        SocketState::Stale => {
            let _ = std::fs::remove_file(&socket_path);
            clear_daemon_pid_file();
        }
        SocketState::Missing => {}
    }

    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    std::process::Command::new(exe)
        .args(["daemon", "start", "--foreground"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn spotuify daemon")?;

    let deadline = tokio::time::Instant::now() + START_DAEMON_TIMEOUT;
    loop {
        match daemon_status().await {
            Ok(status) if status.running && status.socket_reachable => return Ok(Some(status)),
            Ok(_) | Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Ok(status) => return Ok(Some(status)),
            Err(err) => return Err(err),
        }
    }
}

pub async fn ensure_daemon_running() -> Result<()> {
    let status = daemon_status().await?;
    if status.running && status.socket_reachable {
        let current_build_id = current_build_id();
        if status.daemon_build_id.as_deref() == Some(current_build_id.as_str()) {
            return Ok(());
        }
        tracing::info!(
            running_build_id = ?status.daemon_build_id,
            current_build_id,
            "restarting stale spotuify daemon"
        );
        restart_daemon().await?;
        warm_keychain_after_autostart();
        return Ok(());
    }
    start_daemon(false).await?;
    warm_keychain_after_autostart();
    Ok(())
}

fn warm_keychain_after_autostart() {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(crate::auth::token_status());
    });
    match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(Ok(_)) => {}
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "keychain warmup after daemon autostart failed")
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            tracing::warn!("keychain warmup after daemon autostart timed out")
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            tracing::warn!("keychain warmup worker exited")
        }
    }
}

pub async fn stop_daemon() -> Result<()> {
    let status = daemon_status().await?;
    if !status.socket_reachable {
        return Ok(());
    }

    let mut client = IpcClient::connect().await?;
    match client
        .request_with_timeout(Request::Shutdown, STATUS_REQUEST_TIMEOUT)
        .await?
    {
        Response::Ok {
            data: ResponseData::Shutdown,
        } => {}
        Response::Error { message, .. } => anyhow::bail!(message),
        other => anyhow::bail!("unexpected daemon shutdown response: {other:?}"),
    }

    let deadline = tokio::time::Instant::now() + START_DAEMON_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if !matches!(
            inspect_socket_state(&DaemonState::socket_path()).await,
            SocketState::Reachable
        ) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

pub async fn restart_daemon() -> Result<Option<DaemonStatus>> {
    stop_daemon().await?;
    start_daemon(false).await
}

pub async fn daemon_status() -> Result<DaemonStatus> {
    let socket_path = DaemonState::socket_path();
    let socket_state = inspect_socket_state(&socket_path).await;
    if socket_state != SocketState::Reachable {
        return Ok(status_without_running_daemon(&socket_path, socket_state));
    }

    match fetch_daemon_status_from_path(&socket_path, STATUS_REQUEST_TIMEOUT).await {
        Ok(status) => Ok(status),
        Err(err) => {
            tracing::warn!(error = %err, "daemon socket looked reachable but status failed");
            Ok(status_without_running_daemon(
                &socket_path,
                SocketState::Stale,
            ))
        }
    }
}

async fn fetch_daemon_status_from_path(path: &Path, timeout: Duration) -> Result<DaemonStatus> {
    let response = tokio::time::timeout(timeout, async {
        let mut client = IpcClient::connect_to(path).await?;
        client.request(Request::GetDaemonStatus).await
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "Timed out waiting for daemon status from {} after {}s",
            path.display(),
            timeout.as_secs()
        )
    })??;

    match response {
        Response::Ok {
            data: ResponseData::DaemonStatus { status },
        } => Ok(status),
        Response::Error { message, .. } => anyhow::bail!(message),
        other => anyhow::bail!("unexpected daemon status response: {other:?}"),
    }
}

fn status_without_running_daemon(path: &Path, socket_state: SocketState) -> DaemonStatus {
    DaemonStatus {
        running: false,
        socket_path: path.display().to_string(),
        socket_exists: path.exists(),
        socket_reachable: false,
        stale_socket: socket_state == SocketState::Stale,
        daemon_pid: None,
        uptime_secs: None,
        protocol_version: IPC_PROTOCOL_VERSION,
        daemon_version: None,
        daemon_build_id: None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SocketState {
    Reachable,
    Stale,
    Missing,
}

pub(crate) async fn inspect_socket_state(path: &Path) -> SocketState {
    if !path.exists() {
        return SocketState::Missing;
    }
    if socket_accepts_connections(path).await {
        SocketState::Reachable
    } else {
        SocketState::Stale
    }
}

async fn socket_accepts_connections(path: &Path) -> bool {
    for attempt in 0..SOCKET_PROBE_ATTEMPTS {
        match UnixStream::connect(path).await {
            Ok(_) => return true,
            Err(error)
                if should_retry_socket_probe(&error) && attempt + 1 < SOCKET_PROBE_ATTEMPTS =>
            {
                tokio::time::sleep(SOCKET_PROBE_DELAY).await;
            }
            Err(_) => return false,
        }
    }
    false
}

fn should_retry_socket_probe(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::WouldBlock
    )
}

fn write_daemon_pid_file() -> Result<()> {
    let pid_path = DaemonState::pid_path();
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(pid_path, std::process::id().to_string())?;
    Ok(())
}

fn clear_daemon_pid_file() {
    let _ = std::fs::remove_file(DaemonState::pid_path());
}

pub(crate) fn current_daemon_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub(crate) fn current_build_id() -> String {
    let version = current_daemon_version();
    let Ok(exe) = std::env::current_exe() else {
        return format!("{version}:unknown");
    };
    let path = std::fs::canonicalize(&exe).unwrap_or(exe);
    let Ok(meta) = std::fs::metadata(&path) else {
        return format!("{version}:{}", path.display());
    };
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!("{version}:{}:{}:{modified}", path.display(), meta.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_without_running_daemon_marks_stale_socket() {
        let status =
            status_without_running_daemon(Path::new("/tmp/spotuify.sock"), SocketState::Stale);

        assert!(!status.running);
        assert!(status.stale_socket);
        assert!(!status.socket_reachable);
    }
}
