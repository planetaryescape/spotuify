use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::config::Config;

#[derive(Clone, Debug)]
pub enum SpotifydStatus {
    AlreadyRunning,
    Started,
    Disabled,
    NotInstalled,
}

pub fn ensure_started(config: &Config) -> Result<SpotifydStatus> {
    if !config.spotifyd_autostart {
        return Ok(SpotifydStatus::Disabled);
    }

    if is_running() {
        return Ok(SpotifydStatus::AlreadyRunning);
    }

    let Some(binary) = spotifyd_binary() else {
        return Ok(SpotifydStatus::NotInstalled);
    };

    let mut command = Command::new(binary);
    if config.spotifyd_config_path.exists() {
        command
            .arg("--config-path")
            .arg(&config.spotifyd_config_path);
    }
    command
        .arg("--pid")
        .arg(pid_path()?)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    command.spawn().context("failed to start spotifyd")?;
    Ok(SpotifydStatus::Started)
}

pub fn is_running() -> bool {
    Command::new("pgrep")
        .args(["-x", "spotifyd"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn spotifyd_binary() -> Option<PathBuf> {
    let homebrew = Path::new("/opt/homebrew/bin/spotifyd");
    if homebrew.exists() {
        return Some(homebrew.to_path_buf());
    }
    if Command::new("spotifyd")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
    {
        return Some(PathBuf::from("spotifyd"));
    }
    None
}

fn pid_path() -> Result<PathBuf> {
    let base = dirs::cache_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".cache")))
        .context("could not resolve cache directory")?
        .join("spotuify");
    fs::create_dir_all(&base).with_context(|| format!("failed to create {}", base.display()))?;
    Ok(base.join("spotifyd.pid"))
}
