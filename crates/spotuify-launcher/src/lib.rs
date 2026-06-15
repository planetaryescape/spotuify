//! Client-side daemon launcher.
//!
//! Holds everything a *client* (CLI, TUI, the binary's `daemon`
//! subcommands) needs to find, start, restart, and health-check the
//! daemon — without linking the daemon itself. It depends only on
//! `spotuify-protocol`, so `spotuify-cli` can drive the daemon lifecycle
//! over IPC while keeping the client/daemon crate boundary honest.
//!
//! The one thing that stays in the daemon is `run_daemon` (the serve
//! loop). `start_daemon_background` here spawns `spotuify daemon start
//! --foreground` as a detached subprocess; that child runs the daemon.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result};
use spotuify_protocol::ipc_client::IpcClient;
use spotuify_protocol::{
    ipc_stream, paths, DaemonStatus, Request, Response, ResponseData, IPC_PROTOCOL_VERSION,
};

const STATUS_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const START_DAEMON_TIMEOUT: Duration = Duration::from_secs(60);
const START_DAEMON_STABILITY_DELAY: Duration = Duration::from_millis(250);
const SOCKET_PROBE_ATTEMPTS: usize = 5;
const SOCKET_PROBE_DELAY: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketState {
    Reachable,
    Stale,
    Missing,
}

/// Ensure a compatible daemon is running, auto-starting or restarting a
/// stale one unless the caller opted out via `--no-daemon-start`. Never
/// restarts a daemon that's mid-playback (player-first).
pub async fn ensure_daemon_running() -> Result<()> {
    let status = daemon_status().await?;
    if status.running && status.socket_reachable {
        let current_build_id = current_build_id();
        let current_version = current_daemon_version();
        if daemon_is_compatible_with_current_binary(&status, &current_build_id, current_version) {
            return Ok(());
        }
        if no_daemon_start() {
            anyhow::bail!(
                "running daemon is stale (version {:?}, build {:?} vs {current_version}, {current_build_id}) and \
                 --no-daemon-start is set; run `spotuify daemon restart` first",
                status.daemon_version,
                status.daemon_build_id,
            );
        }
        // Player-first: never yank audio out from under an active
        // session. A stale daemon that's mid-playback keeps running; the
        // user restarts it when convenient (or the TUI's update banner
        // prompts them). The next relaunch while idle picks up the new
        // binary automatically.
        if daemon_is_actively_playing().await {
            eprintln!(
                "Note: spotuify {current_version} is installed but the running daemon (v{}) is \
                 mid-playback — not restarting so audio keeps going. Run `spotuify daemon restart` \
                 to apply the update.",
                status.daemon_version.as_deref().unwrap_or("?"),
            );
            return Ok(());
        }
        tracing::info!(
            running_version = ?status.daemon_version,
            running_build_id = ?status.daemon_build_id,
            current_version,
            current_build_id,
            "restarting stale spotuify daemon"
        );
        restart_daemon().await?;
        return Ok(());
    }
    if no_daemon_start() {
        anyhow::bail!(
            "daemon not running and --no-daemon-start is set; \
             run `spotuify daemon start` first"
        );
    }
    start_daemon_background().await?;
    Ok(())
}

/// Honour the `--no-daemon-start` global CLI flag, threaded via env var
/// so any IPC helper can opt into the gate without a signature change.
pub fn no_daemon_start() -> bool {
    std::env::var("SPOTUIFY_NO_DAEMON_START")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Spawn the daemon as a detached background process and wait until it is
/// reachable + stable. Returns the live status, or the existing daemon's
/// status if one is already reachable.
pub async fn start_daemon_background() -> Result<Option<DaemonStatus>> {
    let socket_path = paths::socket_path();
    match inspect_socket_state(&socket_path).await {
        SocketState::Reachable => return daemon_status().await.map(Some),
        SocketState::Stale => {
            remove_stale_socket(&socket_path);
            clear_daemon_pid_file();
        }
        SocketState::Missing => {}
    }

    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    let mut command = Command::new(exe);
    command
        .args(["daemon", "start", "--foreground"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach_daemon_command(&mut command);
    let mut child = command.spawn().context("failed to spawn spotuify daemon")?;
    let child_pid = child.id();

    let deadline = tokio::time::Instant::now() + START_DAEMON_TIMEOUT;
    loop {
        if let Some(status) = child
            .try_wait()
            .context("failed to inspect daemon child status")?
        {
            anyhow::bail!(
                "spotuify daemon exited during startup with {status}; \
                 inspect the daemon log (`spotuify logs path`)"
            );
        }
        match daemon_status().await {
            Ok(status)
                if status.running
                    && status.socket_reachable
                    && status.daemon_pid == Some(child_pid) =>
            {
                tokio::time::sleep(START_DAEMON_STABILITY_DELAY).await;
                if let Some(exit_status) = child
                    .try_wait()
                    .context("failed to inspect daemon child status")?
                {
                    anyhow::bail!(
                        "spotuify daemon exited during startup with {exit_status}; \
                         inspect the daemon log (`spotuify logs path`)"
                    );
                }
                let stable = daemon_status().await?;
                if stable.running && stable.socket_reachable && stable.daemon_pid == Some(child_pid)
                {
                    return Ok(Some(stable));
                }
                if tokio::time::Instant::now() >= deadline {
                    anyhow::bail!(
                        "spotuify daemon did not become stable within {}s (last status: {:?})",
                        START_DAEMON_TIMEOUT.as_secs(),
                        Some(stable)
                    );
                }
            }
            Ok(_) | Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Ok(status) => {
                anyhow::bail!(
                    "spotuify daemon did not become stable within {}s (last status: {:?})",
                    START_DAEMON_TIMEOUT.as_secs(),
                    Some(status)
                );
            }
            Err(err) => return Err(err),
        }
    }
}

#[cfg(unix)]
fn detach_daemon_command(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn detach_daemon_command(_command: &mut Command) {}

/// Best-effort: is the running daemon actively playing on an active
/// device? Used to defer a stale-daemon auto-restart so the update can't
/// cut audio mid-track. Any IPC hiccup answers "no" — safe to restart.
async fn daemon_is_actively_playing() -> bool {
    let Ok(mut client) = IpcClient::connect().await else {
        return false;
    };
    matches!(
        client
            .request_with_timeout(Request::PlaybackGet, STATUS_REQUEST_TIMEOUT)
            .await,
        Ok(Response::Ok {
            data: ResponseData::Playback { playback },
        }) if playback.is_playing
            && playback.device.as_ref().is_some_and(|device| device.is_active)
    )
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
            inspect_socket_state(&paths::socket_path()).await,
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
    start_daemon_background().await
}

pub async fn daemon_status() -> Result<DaemonStatus> {
    let socket_path = paths::socket_path();
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
        audio_health: None,
    }
}

pub async fn inspect_socket_state(path: &Path) -> SocketState {
    #[cfg(windows)]
    {
        return if socket_accepts_connections(path).await {
            SocketState::Reachable
        } else {
            SocketState::Missing
        };
    }

    #[cfg(not(windows))]
    {
        if !path.exists() {
            return SocketState::Missing;
        }
        if socket_accepts_connections(path).await {
            SocketState::Reachable
        } else {
            SocketState::Stale
        }
    }
}

async fn socket_accepts_connections(path: &Path) -> bool {
    for attempt in 0..SOCKET_PROBE_ATTEMPTS {
        match ipc_stream::connect(path).await {
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

#[cfg(unix)]
pub fn remove_stale_socket(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(not(unix))]
pub fn remove_stale_socket(_path: &Path) {}

pub fn clear_daemon_pid_file() {
    let _ = std::fs::remove_file(paths::pid_path());
}

pub fn current_daemon_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn current_build_id() -> String {
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
        .map_or(0, |duration| duration.as_secs());
    format!("{version}:{}:{}:{modified}", path.display(), meta.len())
}

fn daemon_is_compatible_with_current_binary(
    status: &DaemonStatus,
    current_build_id: &str,
    current_version: &str,
) -> bool {
    if status.daemon_build_id.as_deref() == Some(current_build_id) {
        return true;
    }
    // Same protocol + daemon at least as new as this client = leave it
    // alone. Requiring exact version equality caused an upgrade
    // livelock: a TUI opened before `brew upgrade` (older compiled-in
    // version) kept "restarting" the daemon, but every restart spawned
    // the new on-disk binary, so the mismatch never converged and the
    // daemon was bounced every few seconds. Only an OLDER daemon is
    // stale.
    status.protocol_version == IPC_PROTOCOL_VERSION
        && status
            .daemon_version
            .as_deref()
            .is_some_and(|daemon| version_at_least(daemon, current_version))
}

/// True when dotted version `candidate` >= `baseline`. Tolerates a
/// leading `v`; unparseable input compares as 0 so a malformed daemon
/// version reads as older (restart, the safe direction).
fn version_at_least(candidate: &str, baseline: &str) -> bool {
    fn parts(version: &str) -> Vec<u64> {
        version
            .trim()
            .trim_start_matches('v')
            .split('.')
            .map(|part| part.parse().unwrap_or(0))
            .collect()
    }
    let (a, b) = (parts(candidate), parts(baseline));
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_without_running_daemon_marks_stale_socket() {
        let status =
            status_without_running_daemon(Path::new("/tmp/spotuify.sock"), SocketState::Stale);
        assert!(!status.running);
        assert!(!status.socket_reachable);
        assert!(status.stale_socket);
    }

    #[test]
    fn compat_matches_on_build_id_or_version_and_protocol() {
        let mut status = status_without_running_daemon(Path::new("/x"), SocketState::Reachable);
        status.daemon_build_id = Some("0.1.0:bin:1:2".to_string());
        assert!(daemon_is_compatible_with_current_binary(
            &status,
            "0.1.0:bin:1:2",
            "0.1.0"
        ));

        status.daemon_build_id = Some("different".to_string());
        status.daemon_version = Some("0.1.0".to_string());
        assert!(daemon_is_compatible_with_current_binary(
            &status, "current", "0.1.0"
        ));

        // A NEWER daemon on the same protocol is compatible — an old
        // client restarting it caused the post-upgrade livelock (every
        // restart spawned the new on-disk binary again).
        status.daemon_version = Some("9.9.9".to_string());
        assert!(daemon_is_compatible_with_current_binary(
            &status, "current", "0.1.0"
        ));

        // An OLDER daemon is stale and should be restarted.
        status.daemon_version = Some("0.0.9".to_string());
        assert!(!daemon_is_compatible_with_current_binary(
            &status, "current", "0.1.0"
        ));
    }

    #[test]
    fn version_at_least_orders_dotted_versions() {
        assert!(version_at_least("0.1.62", "0.1.60"));
        assert!(version_at_least("0.1.60", "0.1.60"));
        assert!(!version_at_least("0.1.60", "0.1.62"));
        assert!(version_at_least("v0.2.0", "0.1.99"));
        assert!(version_at_least("1.0", "0.9.9"));
        // Unparseable daemon versions read as older (restart-safe).
        assert!(!version_at_least("garbage", "0.1.0"));
    }
}
