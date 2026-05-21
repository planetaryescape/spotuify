//! Phase 11 — cross-platform path resolution.
//!
//! Centralises every "where does spotuify put X?" question so the
//! daemon, CLI, TUI, MCP server, and ipc_client all agree without a
//! circular dependency. Lives in `spotuify-protocol` because it has no
//! workspace deps below it and is consumed everywhere upstream.
//!
//! Resolution rules per platform (see `docs/blueprint/14-reuse-strategy.md`
//! and ncspot's v1.0.0 socket-path lesson — never put sockets in the
//! cache dir):
//!
//! - **macOS**: `~/Library/Application Support/<instance>/`.
//! - **Linux**: `$XDG_RUNTIME_DIR/<instance>/` → `/run/user/<uid>/<instance>/`
//!   → `/tmp/<instance>-<uid>/` (in that order, first writable wins).
//! - **Windows**: `%LOCALAPPDATA%\<instance>\` for files; named pipe
//!   `\\.\pipe\<instance>-<username>` for the IPC socket.
//!
//! Every path can be overridden via the matching `SPOTUIFY_*` env var
//! so integration tests can hop into a temp directory.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Per-build instance name. Debug builds and binaries run from Cargo's
/// `target/{debug,release}` tree get `spotuify-dev` so a developer can
/// run both a stable installed binary and a local build next to it
/// without socket/cache/log/auth collisions. Override with
/// `SPOTUIFY_INSTANCE`.
pub fn app_instance_name() -> String {
    if let Some(name) = std::env::var_os("SPOTUIFY_INSTANCE") {
        if let Some(s) = name.to_str() {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    if cfg!(debug_assertions) || current_exe_is_cargo_target_build() {
        "spotuify-dev".to_string()
    } else {
        "spotuify".to_string()
    }
}

fn current_exe_is_cargo_target_build() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    path_has_cargo_target_profile_ancestor(&exe)
}

fn path_has_cargo_target_profile_ancestor(path: &Path) -> bool {
    let mut dir = path.parent();
    while let Some(current) = dir {
        let Some(name) = current.file_name() else {
            dir = current.parent();
            continue;
        };
        if (name == OsStr::new("debug") || name == OsStr::new("release"))
            && has_target_parent(current)
        {
            return true;
        }
        dir = current.parent();
    }
    false
}

fn has_target_parent(profile_dir: &Path) -> bool {
    let Some(parent) = profile_dir.parent() else {
        return false;
    };
    if parent.file_name().is_some_and(is_cargo_target_dir_name) {
        return true;
    }
    parent
        .parent()
        .and_then(|grandparent| grandparent.file_name())
        .is_some_and(is_cargo_target_dir_name)
}

fn is_cargo_target_dir_name(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name == "target" || name.starts_with("target-")
}

/// Where the daemon places its runtime state (socket, pid file). Per
/// platform-conditional resolution. Never returns a cache directory.
///
/// `SPOTUIFY_RUNTIME_DIR` always wins. On Linux falls back through
/// `$XDG_RUNTIME_DIR` → `/run/user/<uid>` → `/tmp/spotuify-<uid>` so
/// minimal systems without systemd still work.
pub fn runtime_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("SPOTUIFY_RUNTIME_DIR") {
        return PathBuf::from(path);
    }
    let instance = app_instance_name();

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = dirs::home_dir() {
            return home.join("Library/Application Support").join(&instance);
        }
        return PathBuf::from("/tmp").join(&instance);
    }

    #[cfg(target_os = "linux")]
    {
        // dirs::runtime_dir() returns XDG_RUNTIME_DIR when set, which
        // is the canonical Linux runtime directory. Falls through to
        // /tmp on minimal systems with no DBus/systemd.
        if let Some(rt) = dirs::runtime_dir() {
            return rt.join(&instance);
        }
        let user = std::env::var("USER").unwrap_or_else(|_| "user".to_string());
        return PathBuf::from(format!("/tmp/{}-{}", instance, user));
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local).join(&instance);
        }
        if let Some(home) = dirs::home_dir() {
            return home.join("AppData").join("Local").join(&instance);
        }
        return PathBuf::from(".").join(&instance);
    }

    #[allow(unreachable_code)]
    {
        // Any other Unix-like (BSDs etc.): mirror Linux fallback.
        let user = std::env::var("USER").unwrap_or_else(|_| "user".to_string());
        PathBuf::from(format!("/tmp/{}-{}", instance, user))
    }
}

/// Local user data directory. Distinct from `runtime_dir`: data here
/// persists across reboots (sqlite db, search index, encrypted creds
/// file). On Linux uses `$XDG_DATA_HOME` → `~/.local/share`.
pub fn data_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("SPOTUIFY_DATA_DIR") {
        return PathBuf::from(path);
    }
    let instance = app_instance_name();
    dirs::data_local_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join(&instance)
}

/// User config directory. `~/.config/<instance>/` on Linux,
/// `~/Library/Application Support/<instance>/` on macOS,
/// `%APPDATA%\<instance>\` on Windows.
pub fn config_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("SPOTUIFY_CONFIG_DIR") {
        return PathBuf::from(path);
    }
    let instance = app_instance_name();
    dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join(&instance)
}

/// User cache directory (search index, art thumbnails, …). Never
/// holds the IPC socket — see `socket_path`.
pub fn cache_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("SPOTUIFY_CACHE_DIR") {
        return PathBuf::from(path);
    }
    let instance = app_instance_name();
    dirs::cache_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".cache")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join(&instance)
}

/// Where structured logs are written.
pub fn log_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("SPOTUIFY_LOG_DIR") {
        return PathBuf::from(path);
    }
    #[cfg(target_os = "macos")]
    {
        let instance = app_instance_name();
        if let Some(home) = dirs::home_dir() {
            return home.join("Library/Logs").join(instance);
        }
    }

    cache_dir()
}

/// IPC socket path / named-pipe address.
///
/// - Unix: a path inside `runtime_dir`.
/// - Windows: a `\\.\pipe\spotuify-<username>` string. The caller is
///   responsible for using `NamedPipeServer::create` / `NamedPipeClient`
///   rather than `UnixStream`; this returns a `PathBuf` purely so the
///   API stays uniform.
pub fn socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("SPOTUIFY_SOCKET") {
        return PathBuf::from(path);
    }

    #[cfg(windows)]
    {
        let user = std::env::var_os("USERNAME")
            .and_then(|os| os.into_string().ok())
            .unwrap_or_else(|| "user".to_string());
        let pipe = format!("\\\\.\\pipe\\{}-{}", app_instance_name(), user);
        return PathBuf::from(pipe);
    }

    runtime_dir().join("daemon.sock")
}

/// Sibling pidfile used to detect stale sockets at daemon startup.
pub fn pid_path() -> PathBuf {
    if let Some(path) = std::env::var_os("SPOTUIFY_PID_FILE") {
        return PathBuf::from(path);
    }
    #[cfg(windows)]
    {
        return runtime_dir().join("daemon.pid");
    }
    let mut p = socket_path();
    let file = p
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "daemon".to_string());
    p.set_file_name(format!("{file}.pid"));
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Process-global env is shared across parallel tests; serialize
    // every env-mutating test through this mutex so they don't race.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn app_instance_name_respects_override() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("SPOTUIFY_INSTANCE", "ci-job-42");
        assert_eq!(app_instance_name(), "ci-job-42");
        std::env::remove_var("SPOTUIFY_INSTANCE");
    }

    #[test]
    fn app_instance_name_returns_dev_or_release_default() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("SPOTUIFY_INSTANCE");
        let name = app_instance_name();
        assert!(name == "spotuify" || name == "spotuify-dev");
    }

    #[test]
    fn socket_path_respects_env_override() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("SPOTUIFY_SOCKET", "/tmp/test-spotuify.sock");
        assert_eq!(socket_path(), PathBuf::from("/tmp/test-spotuify.sock"));
        std::env::remove_var("SPOTUIFY_SOCKET");
    }

    #[test]
    fn runtime_dir_respects_env_override() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("SPOTUIFY_RUNTIME_DIR", "/tmp/test-runtime");
        assert_eq!(runtime_dir(), PathBuf::from("/tmp/test-runtime"));
        std::env::remove_var("SPOTUIFY_RUNTIME_DIR");
    }

    #[test]
    #[cfg(unix)]
    fn pid_path_is_socket_with_pid_extension_on_unix() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("SPOTUIFY_SOCKET", "/tmp/test-spotuify.sock");
        std::env::remove_var("SPOTUIFY_PID_FILE");
        let pid = pid_path();
        assert_eq!(
            pid.file_name().unwrap().to_string_lossy(),
            "test-spotuify.sock.pid"
        );
        std::env::remove_var("SPOTUIFY_SOCKET");
    }

    #[test]
    fn data_dir_under_override() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("SPOTUIFY_DATA_DIR", "/tmp/spotuify-data-test");
        assert_eq!(data_dir(), PathBuf::from("/tmp/spotuify-data-test"));
        std::env::remove_var("SPOTUIFY_DATA_DIR");
    }

    #[test]
    fn target_profile_paths_are_classified_as_dev_builds() {
        assert!(path_has_cargo_target_profile_ancestor(Path::new(
            "/repo/target/release/spotuify"
        )));
        assert!(path_has_cargo_target_profile_ancestor(Path::new(
            "/repo/target/aarch64-apple-darwin/release/spotuify"
        )));
        assert!(path_has_cargo_target_profile_ancestor(Path::new(
            "/repo/target-cli/release/spotuify"
        )));
        assert!(!path_has_cargo_target_profile_ancestor(Path::new(
            "/opt/homebrew/bin/spotuify"
        )));
    }
}
