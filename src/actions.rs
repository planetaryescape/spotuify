use std::time::Duration;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use tokio::time;

use crate::analytics::{action_finished_event, now_ms, search_performed_event};
use crate::config::Config;
use crate::selection::{first_media_item, media_kind_from_uri};
use crate::spotify::{Device, MediaItem, MediaKind, Playback, Playlist, Queue, SpotifyClient};
use crate::spotifyd;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchScope {
    All,
    Track,
    Episode,
    Album,
    Playlist,
}

impl SearchScope {
    fn kinds(self) -> Vec<MediaKind> {
        match self {
            Self::All => vec![
                MediaKind::Track,
                MediaKind::Episode,
                MediaKind::Album,
                MediaKind::Playlist,
            ],
            Self::Track => vec![MediaKind::Track],
            Self::Episode => vec![MediaKind::Episode],
            Self::Album => vec![MediaKind::Album],
            Self::Playlist => vec![MediaKind::Playlist],
        }
    }
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

pub async fn play_query(
    client: &mut SpotifyClient,
    query: &str,
    scope: SearchScope,
) -> Result<MediaItem> {
    let item = first_media_item(search(client, query, scope).await?, query)?;
    play_item(client, &item).await?;
    Ok(item)
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
