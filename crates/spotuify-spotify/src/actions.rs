use std::time::Duration;

use tokio::time;

use crate::config::Config;
use crate::error::{SpotifyError, SpotifyResult};
use crate::SpotifyClient;
use spotuify_core::{action_finished_event, now_ms, Device, MediaItem, Playback, Playlist, Queue};

use crate::selection::media_kind_from_uri;

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

pub async fn status(client: &mut SpotifyClient) -> SpotifyResult<Playback> {
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

pub async fn devices(client: &mut SpotifyClient) -> SpotifyResult<Vec<Device>> {
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

pub async fn queue(client: &mut SpotifyClient) -> SpotifyResult<Queue> {
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

pub async fn playlists(client: &mut SpotifyClient) -> SpotifyResult<Vec<Playlist>> {
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

pub async fn play_item(client: &mut SpotifyClient, item: &MediaItem) -> SpotifyResult<()> {
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

pub async fn play_uri(client: &mut SpotifyClient, uri: &str) -> SpotifyResult<()> {
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

pub async fn pause(client: &mut SpotifyClient) -> SpotifyResult<()> {
    client.play_pause(true).await?;
    record_action(client, "pause", None, serde_json::json!({})).await;
    Ok(())
}

pub async fn resume(client: &mut SpotifyClient) -> SpotifyResult<()> {
    ensure_playback_target(client).await?;
    client.play_pause(false).await?;
    record_action(client, "resume", None, serde_json::json!({})).await;
    Ok(())
}

pub async fn toggle_playback(client: &mut SpotifyClient) -> SpotifyResult<bool> {
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

pub async fn next(client: &mut SpotifyClient) -> SpotifyResult<()> {
    client.next().await?;
    record_action(client, "next", None, serde_json::json!({})).await;
    Ok(())
}

pub async fn previous(client: &mut SpotifyClient) -> SpotifyResult<()> {
    client.previous().await?;
    record_action(client, "previous", None, serde_json::json!({})).await;
    Ok(())
}

pub async fn execute(
    client: &mut SpotifyClient,
    command: CommandKind,
) -> SpotifyResult<CommandResult> {
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
                return Err(SpotifyError::InvalidInput {
                    message: "repeat must be off, context, or track".to_string(),
                });
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
                .ok_or_else(|| SpotifyError::InvalidInput {
                    message: "selected device has no transferable id".to_string(),
                })?;
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
                .ok_or_else(|| SpotifyError::InvalidInput {
                    message: "nothing is playing".to_string(),
                })?;
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

async fn ensure_playback_target(client: &mut SpotifyClient) -> SpotifyResult<()> {
    if let Ok(playback) = client.playback().await {
        if playback
            .device
            .as_ref()
            .is_some_and(|device| !device.is_restricted)
        {
            return Ok(());
        }
    }

    // Phase 0 cleanup: spotifyd auto-start removed (spotuify is
    // librespot-only). The embedded librespot backend self-registers
    // its Connect device at daemon startup, so by the time we poll
    // for devices here it is already in the list.

    let mut last_devices = Vec::new();
    for attempt in 0..4 {
        let devices = client.devices().await?;
        if let Some(device) = preferred_device(client.config(), &devices) {
            let id = device
                .id
                .clone()
                .ok_or_else(|| SpotifyError::InvalidInput {
                    message: format!("Spotify device {} has no transferable id", device.name),
                })?;
            client
                .transfer(&id, false)
                .await
                .map_err(|err| SpotifyError::Client {
                    message: format!("failed to activate Spotify device {}: {err}", device.name),
                })?;
            return Ok(());
        }
        last_devices = devices;
        if attempt < 3 {
            time::sleep(Duration::from_millis(750)).await;
        }
    }

    Err(SpotifyError::Client {
        message: playback_target_error(client.config(), &last_devices),
    })
}

pub fn preferred_device(config: &Config, devices: &[Device]) -> Option<Device> {
    let unrestricted = devices.iter().filter(|device| !device.is_restricted);
    // 1. Active device — already chosen by the user via another client.
    if let Some(device) = unrestricted.clone().find(|device| device.is_active) {
        return Some(device.clone());
    }
    // The user may have configured either the new librespot/embedded
    // device name (`player.device_name`) OR the legacy spotifyd name
    // (`spotifyd.device_name`). Try both — the embedded backend
    // registers under the former; spotifyd uses the latter.
    let names: Vec<&str> = [
        config.player.device_name.as_deref(),
        config.spotifyd_device_name.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|n| !n.is_empty())
    .collect();
    // 2. Exact name match against either configured preferred name.
    for name in &names {
        if let Some(device) = unrestricted
            .clone()
            .find(|device| device.name.eq_ignore_ascii_case(name))
        {
            return Some(device.clone());
        }
    }
    // 3. Any device whose name contains "spotifyd" or "librespot" —
    //    convention markers for our own virtual backends.
    if let Some(device) = unrestricted.clone().find(|device| {
        let dn = device.name.to_ascii_lowercase();
        dn.contains("spotifyd") || dn.contains("librespot")
    }) {
        return Some(device.clone());
    }
    // 4. Fall back to *some* unrestricted device so play actions don't
    //    fail outright. Prefer a name-substring overlap with one of the
    //    configured preferred names — for example, configured
    //    `spotuify-hume` matches a real `Hume` after the librespot
    //    refactor renamed the registration target. Otherwise pick the
    //    first by stable id ordering.
    let mut candidates: Vec<&Device> = unrestricted.collect();
    candidates.sort_by(|a, b| a.id.cmp(&b.id));
    for name in &names {
        let needle = name.to_ascii_lowercase();
        let stripped = needle
            .trim_start_matches("spotuify-")
            .trim_start_matches("spotifyd-")
            .trim_start_matches("librespot-");
        let needle_token = if stripped.is_empty() {
            needle.as_str()
        } else {
            stripped
        };
        if let Some(device) = candidates.iter().find(|device| {
            let dn = device.name.to_ascii_lowercase();
            dn.contains(needle_token) || needle_token.contains(&dn)
        }) {
            return Some((*device).clone());
        }
    }
    candidates.first().map(|device| (*device).clone())
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
            player: crate::config::PlayerConfig::default(),
            cache: crate::config::CacheConfig::default(),
            analytics: crate::config::AnalyticsConfig::default(),
            notifications: crate::config::NotificationsConfig::default(),
            discord: crate::config::DiscordConfig::default(),
            viz: crate::config::VizConfig::default(),
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
                .expect("preferred device should resolve to active unrestricted device")
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
                .expect("preferred device should skip restricted active devices")
                .name,
            "spotuify-hume"
        );
    }

    #[test]
    fn preferred_device_fuzzy_matches_when_preferred_name_doesnt_exist() {
        // User-reported case: their config asks for `spotuify-hume` but
        // the visible device is `Hume`. The `spotuify-` prefix is the
        // librespot virtual-device convention; strip it and the rest
        // matches.
        let devices = [
            device("Hume", false, false),
            device("Office Echo", false, false),
            device("Lounge", false, false),
        ];

        let chosen = preferred_device(&config(Some("spotuify-hume")), &devices)
            .expect("fuzzy fallback should match Hume");
        assert_eq!(chosen.name, "Hume");
    }

    #[test]
    fn preferred_device_falls_back_to_first_unrestricted_device_when_no_match() {
        // No preferred-name match, no spotifyd device, no fuzzy hit:
        // we still pick *something* so play doesn't fail outright.
        let devices = [
            device("Phone", false, false),
            device("Laptop", false, false),
        ];
        let chosen = preferred_device(&config(Some("unrelated-name")), &devices)
            .expect("first-by-id fallback should always produce a device");
        // Stable sort by id → "id-Laptop" < "id-Phone" alphabetically.
        assert_eq!(chosen.name, "Laptop");
    }

    #[test]
    fn preferred_device_skips_restricted_devices_in_fallback() {
        let devices = [
            device("Cast TV", false, true), // restricted, must be skipped
            device("Phone", false, false),
        ];
        let chosen = preferred_device(&config(None), &devices)
            .expect("fallback should ignore restricted devices");
        assert_eq!(chosen.name, "Phone");
    }

    #[test]
    fn preferred_device_returns_none_only_when_zero_unrestricted_devices() {
        let devices = [device("Cast TV", false, true)];
        assert!(preferred_device(&config(None), &devices).is_none());
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
