use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct Config {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    pub config_path: PathBuf,
    pub spotifyd_config_path: PathBuf,
    pub spotifyd_device_name: Option<String>,
    pub spotifyd_autostart: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
struct FileConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    redirect_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    spotifyd: Option<SpotifydConfig>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
struct SpotifydConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    config_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    autostart: Option<bool>,
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            client_id: None,
            client_secret: None,
            redirect_uri: None,
            spotifyd: Some(SpotifydConfig {
                config_path: None,
                device_name: None,
                autostart: Some(true),
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigKey {
    ClientId,
    ClientSecret,
    RedirectUri,
    SpotifydConfigPath,
    SpotifydDeviceName,
    SpotifydAutostart,
}

impl ConfigKey {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "client_id" | "client-id" => Ok(Self::ClientId),
            "client_secret" | "client-secret" => Ok(Self::ClientSecret),
            "redirect_uri" | "redirect-uri" => Ok(Self::RedirectUri),
            "spotifyd.config_path" | "spotifyd.config-path" => Ok(Self::SpotifydConfigPath),
            "spotifyd.device_name" | "spotifyd.device-name" => Ok(Self::SpotifydDeviceName),
            "spotifyd.autostart" => Ok(Self::SpotifydAutostart),
            _ => bail!(
                "unknown config key `{value}`; expected one of: {}",
                Self::valid_keys().join(", ")
            ),
        }
    }

    pub fn valid_keys() -> &'static [&'static str] {
        &[
            "client_id",
            "client_secret",
            "redirect_uri",
            "spotifyd.config_path",
            "spotifyd.device_name",
            "spotifyd.autostart",
        ]
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = config_path()?;
        ensure_config_exists(&config_path)?;

        let file = read_config_file(&config_path)?;

        let client_id = std::env::var("SPOTUIFY_CLIENT_ID")
            .ok()
            .or(file.client_id)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow!("client_id missing in {}", config_path.display()))?;
        let client_secret = std::env::var("SPOTUIFY_CLIENT_SECRET")
            .ok()
            .or(file.client_secret)
            .filter(|value| !value.trim().is_empty());
        let redirect_uri = std::env::var("SPOTUIFY_REDIRECT_URI")
            .ok()
            .or(file.redirect_uri)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(default_redirect_uri);

        let spotifyd = file.spotifyd;
        let spotifyd_config_path = spotifyd
            .as_ref()
            .and_then(|spotifyd| spotifyd.config_path.as_deref())
            .filter(|value| !value.trim().is_empty())
            .map(expand_home)
            .unwrap_or_else(default_spotifyd_config_path);
        let spotifyd_device_name = spotifyd
            .as_ref()
            .and_then(|spotifyd| spotifyd.device_name.clone())
            .filter(|value| !value.trim().is_empty());
        let spotifyd_autostart = spotifyd
            .and_then(|spotifyd| spotifyd.autostart)
            .unwrap_or(true);

        Ok(Self {
            client_id,
            client_secret,
            redirect_uri,
            config_path,
            spotifyd_config_path,
            spotifyd_device_name,
            spotifyd_autostart,
        })
    }

    pub fn redacted_client_id(&self) -> String {
        let len = self.client_id.chars().count();
        if len <= 8 {
            return "present".to_string();
        }

        let start: String = self.client_id.chars().take(4).collect();
        let end: String = self
            .client_id
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("{start}...{end}")
    }
}

pub fn config_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("SPOTUIFY_CONFIG") {
        return Ok(PathBuf::from(path));
    }

    dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .map(|dir| dir.join("spotuify/spotuify.toml"))
        .ok_or_else(|| anyhow!("could not resolve config directory"))
}

pub fn init_config() -> Result<PathBuf> {
    let path = config_path()?;
    if !path.exists() {
        write_template(&path)?;
    }
    Ok(path)
}

pub fn get_config_value(key: ConfigKey) -> Result<Option<String>> {
    let path = config_path()?;
    let file = if path.exists() {
        read_config_file(&path)?
    } else {
        FileConfig::default()
    };

    Ok(match key {
        ConfigKey::ClientId => blank_to_none(file.client_id),
        ConfigKey::ClientSecret => blank_to_none(file.client_secret),
        ConfigKey::RedirectUri => {
            blank_to_none(file.redirect_uri).or_else(|| Some(default_redirect_uri()))
        }
        ConfigKey::SpotifydConfigPath => file
            .spotifyd
            .as_ref()
            .and_then(|spotifyd| blank_to_none(spotifyd.config_path.clone()))
            .or_else(|| Some(default_spotifyd_config_path().display().to_string())),
        ConfigKey::SpotifydDeviceName => file
            .spotifyd
            .as_ref()
            .and_then(|spotifyd| blank_to_none(spotifyd.device_name.clone())),
        ConfigKey::SpotifydAutostart => Some(
            file.spotifyd
                .and_then(|spotifyd| spotifyd.autostart)
                .unwrap_or(true)
                .to_string(),
        ),
    })
}

pub fn set_config_value(key: ConfigKey, value: &str) -> Result<PathBuf> {
    let path = init_config()?;
    let mut file = read_config_file(&path)?;

    match key {
        ConfigKey::ClientId => file.client_id = blank_to_none(Some(value.to_string())),
        ConfigKey::ClientSecret => file.client_secret = blank_to_none(Some(value.to_string())),
        ConfigKey::RedirectUri => file.redirect_uri = blank_to_none(Some(value.to_string())),
        ConfigKey::SpotifydConfigPath => {
            spotifyd_config_mut(&mut file).config_path = blank_to_none(Some(value.to_string()));
        }
        ConfigKey::SpotifydDeviceName => {
            spotifyd_config_mut(&mut file).device_name = blank_to_none(Some(value.to_string()));
        }
        ConfigKey::SpotifydAutostart => {
            spotifyd_config_mut(&mut file).autostart = Some(parse_bool(value)?);
        }
    }

    write_config_file(&path, &file)?;
    Ok(path)
}

fn ensure_config_exists(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }

    write_template(path)?;
    bail!(
        "created {}; add your Spotify client_id and client_secret, then rerun spotuify",
        path.display()
    )
}

fn read_config_file(path: &Path) -> Result<FileConfig> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    toml::from_str(&contents).with_context(|| format!("could not parse {}", path.display()))
}

fn write_config_file(path: &Path, file: &FileConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let contents = toml::to_string_pretty(file).context("failed to encode config")?;
    fs::write(path, contents).with_context(|| format!("failed to write {}", path.display()))
}

fn write_template(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(path, CONFIG_TEMPLATE).with_context(|| format!("failed to create {}", path.display()))
}

fn spotifyd_config_mut(file: &mut FileConfig) -> &mut SpotifydConfig {
    file.spotifyd.get_or_insert_with(SpotifydConfig::default)
}

fn default_redirect_uri() -> String {
    "http://127.0.0.1:8888/callback".to_string()
}

fn blank_to_none(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_string();
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    })
}

fn parse_bool(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => bail!("expected true or false, got `{value}`"),
    }
}

fn default_spotifyd_config_path() -> PathBuf {
    dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .map(|dir| dir.join("spotifyd/spotifyd.conf"))
        .unwrap_or_else(|| PathBuf::from("spotifyd.conf"))
}

fn expand_home(value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(value)
}

const CONFIG_TEMPLATE: &str = r#"# spotuify config
# Copy your Spotify app credentials from https://developer.spotify.com/dashboard.
client_id = ""
client_secret = ""
redirect_uri = "http://127.0.0.1:8888/callback"

[spotifyd]
autostart = true
# Set this if your spotifyd config lives outside ~/.config/spotifyd/spotifyd.conf.
# config_path = "~/.config/spotifyd/spotifyd.conf"
# device_name = "spotuify"
"#;

#[cfg(test)]
mod tests {
    use super::{expand_home, parse_bool};

    #[test]
    fn keeps_absolute_paths() {
        assert_eq!(
            expand_home("/tmp/spotifyd.conf"),
            std::path::PathBuf::from("/tmp/spotifyd.conf")
        );
    }

    #[test]
    fn parses_bool_config_values() {
        assert!(parse_bool("on").unwrap());
        assert!(!parse_bool("false").unwrap());
        assert!(parse_bool("later").is_err());
    }
}
