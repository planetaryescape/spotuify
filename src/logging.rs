use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

pub fn init() -> Result<WorkerGuard> {
    let path = log_path()?;
    let dir = path
        .parent()
        .context("log path has no parent directory")?
        .to_path_buf();
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;

    let appender = tracing_appender::rolling::never(&dir, "spotuify.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_env("SPOTUIFY_LOG")
        .unwrap_or_else(|_| EnvFilter::new("spotuify=debug,info"));

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .try_init();

    Ok(guard)
}

pub fn log_path() -> Result<PathBuf> {
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
