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

#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, PermissionsExt};

/// Per-build instance name. Debug builds and binaries run from Cargo's
/// `target/{debug,release}` tree get `spotuify-dev` so a developer can
/// run both a stable installed binary and a local build next to it
/// without socket/cache/log/auth collisions. Override with
/// `SPOTUIFY_INSTANCE`.
pub fn app_instance_name() -> String {
    let is_target_build = cfg!(debug_assertions) || current_exe_is_cargo_target_build();
    if let Some(name) = std::env::var_os("SPOTUIFY_INSTANCE") {
        if let Some(s) = name.to_str() {
            if !s.is_empty() {
                return resolve_app_instance_name(
                    Some(s),
                    is_target_build,
                    allow_prod_instance_from_target_build(),
                );
            }
        }
    }
    resolve_app_instance_name(None, is_target_build, false)
}

fn resolve_app_instance_name(
    override_name: Option<&str>,
    is_target_build: bool,
    allow_prod_instance_from_target_build: bool,
) -> String {
    if let Some(name) = override_name {
        if is_target_build && name == "spotuify" && !allow_prod_instance_from_target_build {
            return "spotuify-dev".to_string();
        }
        return name.to_string();
    }
    if is_target_build {
        "spotuify-dev".to_string()
    } else {
        "spotuify".to_string()
    }
}

fn allow_prod_instance_from_target_build() -> bool {
    std::env::var("SPOTUIFY_ALLOW_PROD_INSTANCE_FROM_TARGET")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
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
        PathBuf::from(format!("/tmp/{instance}-{user}"))
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
        PathBuf::from(pipe)
    }

    #[cfg(not(windows))]
    {
        runtime_dir().join("daemon.sock")
    }
}

/// Sentinel marking an *intentional* `daemon stop`. `daemon stop` writes it
/// (epoch-seconds payload) and `daemon start` removes it, so a supervising
/// client — notably the macOS menubar app — can tell a deliberate stop from a
/// crash and avoid immediately relaunching the daemon the user just stopped.
/// Lives beside the socket in the instance runtime dir; the macOS app derives
/// the same path from the socket's parent directory.
pub fn intentional_stop_sentinel() -> PathBuf {
    runtime_dir().join("intentional-stop")
}

/// Sibling pidfile used to detect stale sockets at daemon startup.
pub fn pid_path() -> PathBuf {
    if let Some(path) = std::env::var_os("SPOTUIFY_PID_FILE") {
        return PathBuf::from(path);
    }
    #[cfg(windows)]
    {
        runtime_dir().join("daemon.pid")
    }
    #[cfg(not(windows))]
    {
        let mut p = socket_path();
        let file = p.file_name().map_or_else(
            || "daemon".to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
        p.set_file_name(format!("{file}.pid"));
        p
    }
}

#[cfg(unix)]
pub fn ensure_private_dir(path: &Path) -> anyhow::Result<()> {
    if path.as_os_str().is_empty() {
        anyhow::bail!("private directory path is empty");
    }
    if path.exists() {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.file_type().is_dir() {
            anyhow::bail!("{} is not a directory", path.display());
        }
        let mode = metadata.permissions().mode();
        if mode & 0o002 != 0 && mode & 0o1000 != 0 {
            anyhow::bail!(
                "refusing to chmod shared sticky directory {}; choose an app-specific subdirectory",
                path.display()
            );
        }
    }
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
pub fn ensure_private_dir(path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(path)?;
    Ok(())
}

#[cfg(unix)]
pub fn secure_private_file_if_exists(path: &Path) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        anyhow::bail!("{} is not a regular file", path.display());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
pub fn secure_private_file_if_exists(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(unix)]
pub fn secure_private_socket(path: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket() {
        anyhow::bail!("{} is not a Unix socket", path.display());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
pub fn secure_private_socket(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

pub fn secure_current_instance_dirs() -> anyhow::Result<()> {
    ensure_private_dir(&runtime_dir())?;
    ensure_private_dir(&data_dir())?;
    ensure_private_dir(&config_dir())?;
    ensure_private_dir(&cache_dir())?;
    ensure_private_dir(&log_dir())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic, clippy::unwrap_used)]

    use super::*;

    // Process-global env is shared across parallel tests; serialize
    // every env-mutating test through this mutex so they don't race.
    #[test]
    fn app_instance_name_respects_override() {
        let _g = crate::TEST_ENV_LOCK.lock().unwrap();
        std::env::set_var("SPOTUIFY_INSTANCE", "ci-job-42");
        std::env::remove_var("SPOTUIFY_ALLOW_PROD_INSTANCE_FROM_TARGET");
        assert_eq!(app_instance_name(), "ci-job-42");
        std::env::remove_var("SPOTUIFY_INSTANCE");
    }

    #[test]
    fn app_instance_name_returns_dev_or_release_default() {
        let _g = crate::TEST_ENV_LOCK.lock().unwrap();
        std::env::remove_var("SPOTUIFY_INSTANCE");
        let name = app_instance_name();
        assert!(name == "spotuify" || name == "spotuify-dev");
    }

    #[test]
    fn socket_path_respects_env_override() {
        let _g = crate::TEST_ENV_LOCK.lock().unwrap();
        std::env::set_var("SPOTUIFY_SOCKET", "/tmp/test-spotuify.sock");
        assert_eq!(socket_path(), PathBuf::from("/tmp/test-spotuify.sock"));
        std::env::remove_var("SPOTUIFY_SOCKET");
    }

    #[test]
    fn runtime_dir_respects_env_override() {
        let _g = crate::TEST_ENV_LOCK.lock().unwrap();
        std::env::set_var("SPOTUIFY_RUNTIME_DIR", "/tmp/test-runtime");
        assert_eq!(runtime_dir(), PathBuf::from("/tmp/test-runtime"));
        std::env::remove_var("SPOTUIFY_RUNTIME_DIR");
    }

    #[test]
    #[cfg(unix)]
    fn pid_path_is_socket_with_pid_extension_on_unix() {
        let _g = crate::TEST_ENV_LOCK.lock().unwrap();
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
        let _g = crate::TEST_ENV_LOCK.lock().unwrap();
        std::env::set_var("SPOTUIFY_DATA_DIR", "/tmp/spotuify-data-test");
        assert_eq!(data_dir(), PathBuf::from("/tmp/spotuify-data-test"));
        std::env::remove_var("SPOTUIFY_DATA_DIR");
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_dir_repairs_existing_world_readable_directory() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::TempDir::new().expect("tempdir");
        let path = temp.path().join("state");
        std::fs::create_dir(&path).expect("dir");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("loosen dir");

        ensure_private_dir(&path).expect("dir should be secured");

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn secure_private_file_if_exists_repairs_existing_world_readable_file() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::TempDir::new().expect("tempdir");
        let path = temp.path().join("spotuify.toml");
        std::fs::write(&path, "client_secret = \"secret\"\n").expect("write file");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("loosen file");

        secure_private_file_if_exists(&path).expect("file should be secured");

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn secure_private_socket_repairs_bound_unix_socket() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;

        let temp = tempfile::TempDir::new().expect("tempdir");
        let path = temp.path().join("daemon.sock");
        let _listener = UnixListener::bind(&path).expect("bind socket");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("loosen socket");

        secure_private_socket(&path).expect("socket should be secured");

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn secure_current_instance_dirs_repairs_overridden_state_dirs() {
        use std::os::unix::fs::PermissionsExt;

        let _g = crate::TEST_ENV_LOCK.lock().unwrap();
        let temp = tempfile::TempDir::new().expect("tempdir");
        let runtime = temp.path().join("runtime");
        let data = temp.path().join("data");
        let config = temp.path().join("config");
        let cache = temp.path().join("cache");
        let log = temp.path().join("logs");

        for dir in [&runtime, &data, &config, &cache, &log] {
            std::fs::create_dir(dir).expect("dir");
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))
                .expect("loosen dir");
        }

        std::env::set_var("SPOTUIFY_RUNTIME_DIR", &runtime);
        std::env::set_var("SPOTUIFY_DATA_DIR", &data);
        std::env::set_var("SPOTUIFY_CONFIG_DIR", &config);
        std::env::set_var("SPOTUIFY_CACHE_DIR", &cache);
        std::env::set_var("SPOTUIFY_LOG_DIR", &log);

        secure_current_instance_dirs().expect("state dirs should be secured");

        for dir in [&runtime, &data, &config, &cache, &log] {
            let mode = std::fs::metadata(dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{} should be private", dir.display());
        }

        std::env::remove_var("SPOTUIFY_RUNTIME_DIR");
        std::env::remove_var("SPOTUIFY_DATA_DIR");
        std::env::remove_var("SPOTUIFY_CONFIG_DIR");
        std::env::remove_var("SPOTUIFY_CACHE_DIR");
        std::env::remove_var("SPOTUIFY_LOG_DIR");
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

    #[test]
    fn target_build_cannot_use_prod_instance_by_accident() {
        assert_eq!(
            resolve_app_instance_name(Some("spotuify"), true, false),
            "spotuify-dev"
        );
        assert_eq!(
            resolve_app_instance_name(Some("spotuify-smoke"), true, false),
            "spotuify-smoke"
        );
        assert_eq!(
            resolve_app_instance_name(Some("spotuify"), true, true),
            "spotuify"
        );
    }
}
