use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

/// Phase 13 (P13-A) — log output format. Plaintext is the default;
/// JSON is opt-in via `SPOTUIFY_LOG_FORMAT=json` or `--log-format json`.
/// JSON output is what agents and `spotuify logs tail --format json`
/// consume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Text,
    Json,
}

impl LogFormat {
    pub fn from_env_or_default() -> Self {
        match std::env::var("SPOTUIFY_LOG_FORMAT").ok().as_deref() {
            Some("json") | Some("jsonl") => Self::Json,
            _ => Self::Text,
        }
    }
}

pub fn init() -> Result<WorkerGuard> {
    init_with_format(LogFormat::from_env_or_default())
}

pub fn init_with_format(format: LogFormat) -> Result<WorkerGuard> {
    let path = log_path()?;
    let dir = path
        .parent()
        .context("log path has no parent directory")?
        .to_path_buf();
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;

    // Daily rotation with a 7-file retention window. The previous
    // setup used `rolling::never` which left a single growing file —
    // a 2026-05-17 inspection found a 14 GB log dominated by stale
    // tantivy file_watcher WARNs from old test temp dirs. Daily files
    // are named `spotuify.log.YYYY-MM-DD`; older days get deleted
    // automatically.
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("spotuify.log")
        .max_log_files(7)
        .build(&dir)
        .with_context(|| format!("failed to init rolling log at {}", dir.display()))?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = resolve_log_filter();

    match format {
        LogFormat::Json => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(writer)
                .with_ansi(false)
                .json()
                .with_current_span(true)
                .with_span_list(false)
                .try_init();
        }
        LogFormat::Text => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(writer)
                .with_ansi(false)
                .try_init();
        }
    }

    Ok(guard)
}

/// Resolve the log filter. `SPOTUIFY_LOG` wins; fall back to `RUST_LOG`
/// (covers installs whose service files set the standard tracing env);
/// finally a sensible default.
///
/// The default suppresses two tantivy modules at WARN level because
/// their watchers chronically log spurious `Failed to open meta file
/// ... NotFound` lines when test temp dirs disappear out from under
/// them. The errors are not actionable and used to fill the daemon
/// log at gigabyte scale.
fn resolve_log_filter() -> EnvFilter {
    EnvFilter::try_from_env("SPOTUIFY_LOG")
        .or_else(|_| EnvFilter::try_from_env("RUST_LOG"))
        .unwrap_or_else(|_| {
            EnvFilter::new(
                "spotuify=debug,info,\
                 tantivy::directory::file_watcher=error,\
                 tantivy::directory::managed_directory=error",
            )
        })
}

pub fn log_path() -> Result<PathBuf> {
    // `SPOTUIFY_LOG_DIR` was previously declared in `spotuify_protocol::paths::log_dir`
    // but the daemon's `log_path()` ignored it. Honor it here so integration tests
    // and packagers can redirect logs into a sandbox.
    if let Some(dir) = std::env::var_os("SPOTUIFY_LOG_DIR") {
        return Ok(PathBuf::from(dir).join("spotuify.log"));
    }

    if cfg!(target_os = "macos") {
        return dirs::home_dir()
            .map(|home| home.join("Library/Logs/spotuify/spotuify.log"))
            .context("could not resolve home directory");
    }

    dirs::cache_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".cache")))
        .map(|dir| dir.join("spotuify/spotuify.log"))
        .context("could not resolve cache directory")
}

pub fn read_tail(lines: usize) -> Result<String> {
    let path = log_path()?;
    if !path.exists() {
        return Ok(String::new());
    }

    let contents =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let lines = contents
        .lines()
        .rev()
        .take(lines)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Process env is shared across parallel tests; serialise env-mutating ones.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn log_path_honors_spotuify_log_dir_env() {
        let _g = ENV_LOCK.lock().expect("env lock");
        let prev = std::env::var_os("SPOTUIFY_LOG_DIR");
        std::env::set_var("SPOTUIFY_LOG_DIR", "/tmp/spotuify-test-logs");
        let path = log_path().expect("log_path");
        assert_eq!(path, PathBuf::from("/tmp/spotuify-test-logs/spotuify.log"));
        if let Some(v) = prev {
            std::env::set_var("SPOTUIFY_LOG_DIR", v);
        } else {
            std::env::remove_var("SPOTUIFY_LOG_DIR");
        }
    }

    #[test]
    fn resolve_log_filter_prefers_spotuify_log_over_rust_log() {
        let _g = ENV_LOCK.lock().expect("env lock");
        let prev_s = std::env::var_os("SPOTUIFY_LOG");
        let prev_r = std::env::var_os("RUST_LOG");
        std::env::set_var("SPOTUIFY_LOG", "spotuify=trace");
        std::env::set_var("RUST_LOG", "debug");
        let filter = resolve_log_filter();
        // EnvFilter::Display reflects the directives in the order they
        // were parsed; the spotuify-specific one must be present.
        assert!(filter.to_string().contains("spotuify"));
        restore("SPOTUIFY_LOG", prev_s);
        restore("RUST_LOG", prev_r);
    }

    #[test]
    fn resolve_log_filter_falls_back_to_rust_log_when_spotuify_log_absent() {
        let _g = ENV_LOCK.lock().expect("env lock");
        let prev_s = std::env::var_os("SPOTUIFY_LOG");
        let prev_r = std::env::var_os("RUST_LOG");
        std::env::remove_var("SPOTUIFY_LOG");
        std::env::set_var("RUST_LOG", "warn");
        let filter = resolve_log_filter();
        assert_eq!(filter.to_string(), "warn");
        restore("SPOTUIFY_LOG", prev_s);
        restore("RUST_LOG", prev_r);
    }

    #[test]
    fn resolve_log_filter_falls_back_to_default_when_both_absent() {
        let _g = ENV_LOCK.lock().expect("env lock");
        let prev_s = std::env::var_os("SPOTUIFY_LOG");
        let prev_r = std::env::var_os("RUST_LOG");
        std::env::remove_var("SPOTUIFY_LOG");
        std::env::remove_var("RUST_LOG");
        let filter = resolve_log_filter();
        let s = filter.to_string();
        assert!(s.contains("spotuify=debug") || s.contains("info"));
        restore("SPOTUIFY_LOG", prev_s);
        restore("RUST_LOG", prev_r);
    }

    fn restore(key: &str, prev: Option<std::ffi::OsString>) {
        if let Some(v) = prev {
            std::env::set_var(key, v);
        } else {
            std::env::remove_var(key);
        }
    }
}
