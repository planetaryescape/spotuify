use std::time::Duration;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use tokio::time;

use crate::analytics::{action_finished_event, now_ms, search_performed_event};
use crate::config::Config;
use crate::selection::media_kind_from_uri;
use crate::spotify::{Device, MediaItem, MediaKind, Playback, Playlist, Queue, SpotifyClient};
use crate::spotifyd;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchScope {
    All,
    Track,
    Episode,
    Album,
    Artist,
    Playlist,
}

impl SearchScope {
    fn kinds(self) -> Vec<MediaKind> {
        match self {
            Self::All => vec![
                MediaKind::Track,
                MediaKind::Episode,
                MediaKind::Album,
                MediaKind::Artist,
                MediaKind::Playlist,
            ],
            Self::Track => vec![MediaKind::Track],
            Self::Episode => vec![MediaKind::Episode],
            Self::Album => vec![MediaKind::Album],
            Self::Artist => vec![MediaKind::Artist],
            Self::Playlist => vec![MediaKind::Playlist],
        }
    }
}

#[derive(Clone, Debug)]
pub enum CommandKind {
    Pause,
    Resume,
    TogglePlayback,
    PlayItem {
        item: MediaItem,
    },
    PlayUri {
        uri: String,
    },
    Next,
    Previous,
    Seek {
        position_ms: u64,
    },
    Volume {
        volume_percent: u8,
    },
    Shuffle {
        state: bool,
    },
    Repeat {
        state: String,
    },
    QueueItem {
        item: MediaItem,
    },
    QueueUri {
        uri: String,
    },
    Transfer {
        device: Device,
        play: bool,
    },
    AddToPlaylist {
        item: MediaItem,
        playlist_id: String,
        playlist_name: String,
    },
    SaveItem {
        item: MediaItem,
    },
    SaveCurrent,
}

#[derive(Clone, Debug, Default)]
pub struct CommandResult {
    pub message: Option<String>,
    pub playback: Option<Playback>,
    pub queue: Option<Queue>,
    pub devices: Option<Vec<Device>>,
    pub request_refresh: bool,
}

pub async fn status(client: &mut SpotifyClient) -> Result<Playback> {
    let playback = client.playback().await?;
    record_action(
        client,
        "status",
        playback.item.as_ref().map(|item| item.uri.as_str()),
        serde_json::json!({"is_playing": playback.is_playing}),
    )
    .await;
    Ok(playback)
}

pub async fn devices(client: &mut SpotifyClient) -> Result<Vec<Device>> {
    let devices = client.devices().await?;
    record_action(
        client,
        "devices",
        None,
        serde_json::json!({"device_count": devices.len()}),
    )
    .await;
    Ok(devices)
}

pub async fn queue(client: &mut SpotifyClient) -> Result<Queue> {
    let queue = client.queue().await?;
    record_action(
        client,
        "queue",
        queue
            .currently_playing
            .as_ref()
            .map(|item| item.uri.as_str()),
        serde_json::json!({"upcoming_count": queue.items.len()}),
    )
    .await;
    Ok(queue)
}

pub async fn playlists(client: &mut SpotifyClient) -> Result<Vec<Playlist>> {
    let playlists = client.playlists().await?;
    record_action(
        client,
        "playlists",
        None,
        serde_json::json!({"playlist_count": playlists.len()}),
    )
    .await;
    Ok(playlists)
}

pub async fn search(
    client: &mut SpotifyClient,
    query: &str,
    scope: SearchScope,
) -> Result<Vec<MediaItem>> {
    let kinds = scope.kinds();
    let started = Instant::now();
    let items = client
        .search(query, &kinds)
        .await?
        .into_iter()
        .filter(|item| kinds.contains(&item.kind))
        .collect::<Vec<_>>();
    client
        .record_analytics_event(search_performed_event(
            client.analytics_source(),
            query,
            items.len(),
            started.elapsed().as_millis(),
            now_ms(),
        ))
        .await;
    record_action(
        client,
        "search",
        None,
        serde_json::json!({"query": query, "result_count": items.len()}),
    )
    .await;
    Ok(items)
}

pub async fn play_item(client: &mut SpotifyClient, item: &MediaItem) -> Result<()> {
    ensure_playback_target(client).await?;
    client.play_uri(&item.uri, &item.kind).await?;
    record_action(
        client,
        "play",
        Some(&item.uri),
        serde_json::json!({"kind": item.kind.label(), "name": item.name}),
    )
    .await;
    Ok(())
}

pub async fn play_uri(client: &mut SpotifyClient, uri: &str) -> Result<()> {
    let kind = media_kind_from_uri(uri)?;
    ensure_playback_target(client).await?;
    client.play_uri(uri, &kind).await?;
    record_action(
        client,
        "play_uri",
        Some(uri),
        serde_json::json!({"kind": kind.label()}),
    )
    .await;
    Ok(())
}

pub async fn pause(client: &mut SpotifyClient) -> Result<()> {
    client.play_pause(true).await?;
    record_action(client, "pause", None, serde_json::json!({})).await;
    Ok(())
}

pub async fn resume(client: &mut SpotifyClient) -> Result<()> {
    ensure_playback_target(client).await?;
    client.play_pause(false).await?;
    record_action(client, "resume", None, serde_json::json!({})).await;
    Ok(())
}

pub async fn toggle_playback(client: &mut SpotifyClient) -> Result<bool> {
    let playback = client.playback().await?;
    if playback.is_playing {
        pause(client).await?;
        record_action(
            client,
            "toggle",
            playback.item.as_ref().map(|item| item.uri.as_str()),
            serde_json::json!({"new_state": "paused"}),
        )
        .await;
        Ok(false)
    } else {
        resume(client).await?;
        record_action(
            client,
            "toggle",
            playback.item.as_ref().map(|item| item.uri.as_str()),
            serde_json::json!({"new_state": "playing"}),
        )
        .await;
        Ok(true)
    }
}

pub async fn next(client: &mut SpotifyClient) -> Result<()> {
    client.next().await?;
    record_action(client, "next", None, serde_json::json!({})).await;
    Ok(())
}

pub async fn previous(client: &mut SpotifyClient) -> Result<()> {
    client.previous().await?;
    record_action(client, "previous", None, serde_json::json!({})).await;
    Ok(())
}

pub async fn execute(client: &mut SpotifyClient, command: CommandKind) -> Result<CommandResult> {
    let mut result = CommandResult::default();
    match command {
        CommandKind::Pause => {
            pause(client).await?;
            result.message = Some("Paused".to_string());
            result.request_refresh = true;
            refresh_playback(client, &mut result).await;
        }
        CommandKind::Resume => {
            resume(client).await?;
            result.message = Some("Playing".to_string());
            result.request_refresh = true;
            refresh_playback(client, &mut result).await;
        }
        CommandKind::TogglePlayback => {
            let is_playing = toggle_playback(client).await?;
            result.message = Some(if is_playing { "Playing" } else { "Paused" }.to_string());
            result.request_refresh = true;
            refresh_playback(client, &mut result).await;
        }
        CommandKind::PlayItem { item } => {
            play_item(client, &item).await?;
            result.message = Some(format!("Playing {}", item.name));
            result.request_refresh = true;
            refresh_playback(client, &mut result).await;
        }
        CommandKind::PlayUri { uri } => {
            play_uri(client, &uri).await?;
            result.message = Some(format!("Playing {uri}"));
            result.request_refresh = true;
            refresh_playback(client, &mut result).await;
        }
        CommandKind::Next => {
            next(client).await?;
            result.message = Some("Skipped".to_string());
            result.request_refresh = true;
            refresh_playback(client, &mut result).await;
        }
        CommandKind::Previous => {
            previous(client).await?;
            result.message = Some("Previous track".to_string());
            result.request_refresh = true;
            refresh_playback(client, &mut result).await;
        }
        CommandKind::Seek { position_ms } => {
            client.seek(position_ms).await?;
            record_action(
                client,
                "seek",
                None,
                serde_json::json!({"position_ms": position_ms}),
            )
            .await;
            result.message = Some(format!("Seeked to {}ms", position_ms));
            result.request_refresh = true;
            refresh_playback(client, &mut result).await;
        }
        CommandKind::Volume { volume_percent } => {
            let volume_percent = volume_percent.min(100);
            client.volume(volume_percent).await?;
            record_action(
                client,
                "volume",
                None,
                serde_json::json!({"volume_percent": volume_percent}),
            )
            .await;
            result.message = Some(format!("Volume {volume_percent}%"));
            result.request_refresh = true;
            refresh_playback(client, &mut result).await;
        }
        CommandKind::Shuffle { state } => {
            client.shuffle(state).await?;
            record_action(client, "shuffle", None, serde_json::json!({"state": state})).await;
            result.message = Some(format!("Shuffle {}", if state { "on" } else { "off" }));
            result.request_refresh = true;
            refresh_playback(client, &mut result).await;
        }
        CommandKind::Repeat { state } => {
            if !matches!(state.as_str(), "off" | "context" | "track") {
                anyhow::bail!("repeat must be off, context, or track");
            }
            client.repeat(&state).await?;
            record_action(client, "repeat", None, serde_json::json!({"state": state})).await;
            result.message = Some(format!("Repeat {state}"));
            result.request_refresh = true;
            refresh_playback(client, &mut result).await;
        }
        CommandKind::QueueItem { item } => {
            client.add_to_queue(&item.uri).await?;
            record_action(
                client,
                "queue",
                Some(&item.uri),
                serde_json::json!({"name": item.name}),
            )
            .await;
            result.message = Some(format!("Queued {}", item.name));
            result.request_refresh = true;
            refresh_queue(client, &mut result).await;
        }
        CommandKind::QueueUri { uri } => {
            client.add_to_queue(&uri).await?;
            record_action(client, "queue", Some(&uri), serde_json::json!({})).await;
            result.message = Some(format!("Queued {uri}"));
            result.request_refresh = true;
            refresh_queue(client, &mut result).await;
        }
        CommandKind::Transfer { device, play } => {
            let id = device
                .id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("selected device has no transferable id"))?;
            client.transfer(id, play).await?;
            record_action(
                client,
                "transfer",
                None,
                serde_json::json!({"device": device.name, "play": play}),
            )
            .await;
            result.message = Some(format!("Transferred to {}", device.name));
            result.request_refresh = true;
            refresh_devices(client, &mut result).await;
            refresh_playback(client, &mut result).await;
        }
        CommandKind::AddToPlaylist {
            item,
            playlist_id,
            playlist_name,
        } => {
            client.add_to_playlist(&playlist_id, &item.uri).await?;
            record_action(client, "playlist_add", Some(&item.uri), serde_json::json!({"playlist_id": playlist_id, "playlist_name": playlist_name, "name": item.name})).await;
            result.message = Some(format!("Added {} to {}", item.name, playlist_name));
        }
        CommandKind::SaveItem { item } => {
            client.save_item(&item).await?;
            record_action(
                client,
                "save",
                Some(&item.uri),
                serde_json::json!({"kind": item.kind.label(), "name": item.name}),
            )
            .await;
            result.message = Some(format!("Saved {}", item.name));
        }
        CommandKind::SaveCurrent => {
            let item = client
                .playback()
                .await?
                .item
                .ok_or_else(|| anyhow::anyhow!("nothing is playing"))?;
            client.save_item(&item).await?;
            record_action(
                client,
                "save",
                Some(&item.uri),
                serde_json::json!({"kind": item.kind.label(), "name": item.name}),
            )
            .await;
            result.message = Some(format!("Saved {}", item.name));
        }
    }
    Ok(result)
}

async fn refresh_playback(client: &mut SpotifyClient, result: &mut CommandResult) {
    match client.playback().await {
        Ok(playback) => result.playback = Some(playback),
        Err(err) => tracing::warn!(error = %err, "failed to refresh playback after command"),
    }
}

async fn refresh_queue(client: &mut SpotifyClient, result: &mut CommandResult) {
    match client.queue().await {
        Ok(queue) => result.queue = Some(queue),
        Err(err) => tracing::warn!(error = %err, "failed to refresh queue after command"),
    }
}

async fn refresh_devices(client: &mut SpotifyClient, result: &mut CommandResult) {
    match client.devices().await {
        Ok(devices) => result.devices = Some(devices),
        Err(err) => tracing::warn!(error = %err, "failed to refresh devices after command"),
    }
}

async fn record_action(
    client: &SpotifyClient,
    action: &str,
    subject_uri: Option<&str>,
    payload: serde_json::Value,
) {
    client
        .record_analytics_event(action_finished_event(
            client.analytics_source(),
            action,
            subject_uri,
            "ok",
            payload,
            now_ms(),
        ))
        .await;
}

async fn ensure_playback_target(client: &mut SpotifyClient) -> Result<()> {
    if let Ok(playback) = client.playback().await {
        if playback
            .device
            .as_ref()
            .is_some_and(|device| !device.is_restricted)
        {
            return Ok(());
        }
    }

    if let Err(err) = spotifyd::ensure_started(client.config()) {
        tracing::warn!(error = %err, "failed to ensure spotifyd is started");
    }

    let mut last_devices = Vec::new();
    for attempt in 0..4 {
        let devices = client
            .devices()
            .await
            .context("no active Spotify device found; failed to fetch devices")?;
        if let Some(device) = preferred_device(client.config(), &devices) {
            let id = device.id.clone().with_context(|| {
                format!("Spotify device {} has no transferable id", device.name)
            })?;
            client
                .transfer(&id, false)
                .await
                .with_context(|| format!("failed to activate Spotify device {}", device.name))?;
            return Ok(());
        }
        last_devices = devices;
        if attempt < 3 {
            time::sleep(Duration::from_millis(750)).await;
        }
    }

    bail!("{}", playback_target_error(client.config(), &last_devices))
}

pub fn preferred_device(config: &Config, devices: &[Device]) -> Option<Device> {
    let unrestricted = devices.iter().filter(|device| !device.is_restricted);
    if let Some(device) = unrestricted.clone().find(|device| device.is_active) {
        return Some(device.clone());
    }
    if let Some(name) = &config.spotifyd_device_name {
        if let Some(device) = unrestricted
            .clone()
            .find(|device| device.name.eq_ignore_ascii_case(name))
        {
            return Some(device.clone());
        }
    }
    if let Some(device) = unrestricted
        .clone()
        .find(|device| device.name.to_ascii_lowercase().contains("spotifyd"))
    {
        return Some(device.clone());
    }
    let candidates = unrestricted.collect::<Vec<_>>();
    if candidates.len() == 1 {
        return Some(candidates[0].clone());
    }
    None
}

pub fn playback_target_error(config: &Config, devices: &[Device]) -> String {
    let names = devices
        .iter()
        .filter(|device| !device.is_restricted)
        .map(|device| device.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let preferred = config
        .spotifyd_device_name
        .as_deref()
        .unwrap_or("not configured");
    if names.is_empty() {
        return format!(
            "no active Spotify device found; start Spotify or spotifyd; preferred device: {preferred}; run `spotuify devices`"
        );
    }
    format!(
        "no preferred Spotify device found; preferred device: {preferred}; visible devices: {names}; run `spotuify devices`"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(preferred: Option<&str>) -> Config {
        Config {
            client_id: "client".into(),
            client_secret: None,
            redirect_uri: "http://127.0.0.1:8888/callback".into(),
            config_path: "spotuify.toml".into(),
            spotifyd_config_path: "spotifyd.conf".into(),
            spotifyd_device_name: preferred.map(str::to_string),
            spotifyd_autostart: true,
        }
    }

    fn device(name: &str, active: bool, restricted: bool) -> Device {
        Device {
            id: Some(format!("id-{name}")),
            name: name.into(),
            kind: "Computer".into(),
            is_active: active,
            is_restricted: restricted,
            volume_percent: None,
            supports_volume: true,
        }
    }

    #[test]
    fn preferred_device_prefers_active_unrestricted_device() {
        let devices = [
            device("spotuify-hume", false, false),
            device("phone", true, false),
        ];

        assert_eq!(
            preferred_device(&config(Some("spotuify-hume")), &devices)
                .unwrap()
                .name,
            "phone"
        );
    }

    #[test]
    fn preferred_device_ignores_restricted_active_device() {
        let devices = [
            device("tv", true, true),
            device("spotuify-hume", false, false),
        ];

        assert_eq!(
            preferred_device(&config(Some("spotuify-hume")), &devices)
                .unwrap()
                .name,
            "spotuify-hume"
        );
    }

    #[test]
    fn playback_target_error_lists_unrestricted_visible_devices_only() {
        let message = playback_target_error(
            &config(Some("spotuify-hume")),
            &[device("phone", false, false), device("tv", false, true)],
        );

        assert!(message.contains("preferred device: spotuify-hume"));
        assert!(message.contains("phone"));
        assert!(!message.contains("tv"));
        assert!(message.contains("spotuify devices"));
    }
}
