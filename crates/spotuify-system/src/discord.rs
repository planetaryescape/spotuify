//! Phase 14 (P14-F) — Discord Rich Presence (opt-in).
//!
//! Disabled by default; users add `[discord] enabled = true` + a
//! Discord application_id to flip it on. Failure to connect (no
//! Discord running, app id rejected, IPC socket missing) is logged
//! and, after a few consecutive failures, disables RPC for the
//! session — never crashes the daemon.
//!
//! The `discord-rich-presence` IPC client is blocking, so every update
//! runs on `spawn_blocking`; a shared lock serialises connect/update so
//! one client is reused across events.

use std::sync::Arc;

use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};
use parking_lot::Mutex;
use spotuify_core::Playback;
use spotuify_protocol::DaemonEvent;

/// Disable Discord RPC for the session after this many consecutive
/// failures so a missing/locked Discord socket doesn't churn forever.
const DISCORD_GIVE_UP_AFTER: u32 = 3;

#[derive(Debug, Clone, Default)]
pub struct DiscordConfig {
    pub enabled: bool,
    pub application_id: String,
}

/// The presence we want Discord to show for the current event.
enum Desired {
    /// Clear presence (paused / stopped).
    Clear,
    /// Show the playing track.
    Show {
        details: String,
        state: String,
        large_text: String,
        image_url: Option<String>,
        start_unix_secs: Option<i64>,
    },
}

struct ConnState {
    client: Option<DiscordIpcClient>,
    consecutive_failures: u32,
    disabled: bool,
}

impl ConnState {
    fn note_failure(&mut self, err: &dyn std::fmt::Display) {
        // A failed update almost always means the socket is gone; drop
        // the client so the next event reconnects.
        self.client = None;
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures >= DISCORD_GIVE_UP_AFTER {
            self.disabled = true;
            tracing::warn!(
                error = %err,
                "Discord RPC disabled for this session after repeated failures"
            );
        } else {
            tracing::debug!(error = %err, "Discord RPC update failed; will retry");
        }
    }
}

pub struct DiscordHandle {
    config: DiscordConfig,
    state: Arc<Mutex<ConnState>>,
}

impl DiscordHandle {
    pub fn new(config: DiscordConfig) -> anyhow::Result<Self> {
        if config.application_id.trim().is_empty() {
            anyhow::bail!("[discord] enabled = true but application_id is empty");
        }
        Ok(Self {
            config,
            state: Arc::new(Mutex::new(ConnState {
                client: None,
                consecutive_failures: 0,
                disabled: false,
            })),
        })
    }

    pub fn application_id(&self) -> &str {
        &self.config.application_id
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// Reflect a `PlaybackChanged` into Discord Rich Presence. Connects
    /// lazily, reuses the client across events, and runs the blocking
    /// IPC on a blocking thread so the daemon's event handler never
    /// stalls.
    pub async fn handle(&self, event: &DaemonEvent) {
        if !self.config.enabled {
            return;
        }
        let DaemonEvent::PlaybackChanged { action, playback } = event else {
            return;
        };
        let Some(desired) = desired_presence(action, playback.as_ref()) else {
            return;
        };

        let state = self.state.clone();
        let app_id = self.config.application_id.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = state.lock();
            if guard.disabled {
                return;
            }
            if guard.client.is_none() {
                match connect(&app_id) {
                    Ok(client) => guard.client = Some(client),
                    Err(err) => {
                        guard.note_failure(&err);
                        return;
                    }
                }
            }
            let client = guard.client.as_mut().expect("client present after connect");
            let result = match &desired {
                Desired::Clear => client.clear_activity(),
                Desired::Show {
                    details,
                    state,
                    large_text,
                    image_url,
                    start_unix_secs,
                } => {
                    let mut activity = activity::Activity::new().details(details).state(state);
                    if let Some(start) = start_unix_secs {
                        activity = activity.timestamps(activity::Timestamps::new().start(*start));
                    }
                    let mut assets = activity::Assets::new().large_text(large_text);
                    if let Some(url) = image_url {
                        assets = assets.large_image(url);
                    }
                    activity = activity.assets(assets);
                    client.set_activity(activity)
                }
            };
            match result {
                Ok(()) => guard.consecutive_failures = 0,
                Err(err) => guard.note_failure(&err),
            }
        });
    }
}

fn connect(application_id: &str) -> Result<DiscordIpcClient, String> {
    let mut client = DiscordIpcClient::new(application_id).map_err(|err| err.to_string())?;
    client.connect().map_err(|err| err.to_string())?;
    Ok(client)
}

/// Map a `PlaybackChanged` to the presence Discord should show. Reacts
/// only to track-change / play / pause actions so volume/seek events
/// don't churn presence. `None` means "leave presence unchanged".
fn desired_presence(action: &str, playback: Option<&Playback>) -> Option<Desired> {
    if action == "paused" || action == "pause" {
        return Some(Desired::Clear);
    }
    let is_play = action.starts_with("started ")
        || action.starts_with("track changed ")
        || action == "resumed"
        || action == "play-uri"
        || action == "play-context";
    if !is_play {
        return None;
    }
    let playback = playback?;
    let item = playback.item.as_ref()?;
    Some(Desired::Show {
        details: non_empty(&item.name).unwrap_or_else(|| "Listening on Spotify".to_string()),
        state: non_empty(&item.subtitle).unwrap_or_default(),
        large_text: item
            .album
            .clone()
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| item.context.clone()),
        image_url: item.image_url.clone(),
        // Elapsed bar: when this track started = now - progress.
        start_unix_secs: playback_start_unix_secs(playback),
    })
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

fn playback_start_unix_secs(playback: &Playback) -> Option<i64> {
    if !playback.is_playing {
        return None;
    }
    let now_secs = spotuify_core::now_ms() / 1000;
    let progress_secs = (playback.progress_ms / 1000) as i64;
    Some(now_secs - progress_secs)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic)]

    use super::*;
    use spotuify_core::MediaItem;

    #[test]
    fn new_rejects_empty_application_id() {
        // Connecting to Discord without an application_id is a no-op
        // at the IPC layer (the server returns InvalidPayload), so we
        // refuse construction up-front with a clear message rather
        // than spamming the log on every event.
        let cfg = DiscordConfig {
            enabled: true,
            application_id: "  ".into(),
        };
        assert!(DiscordHandle::new(cfg).is_err());
    }

    #[test]
    fn new_accepts_non_empty_application_id() {
        let cfg = DiscordConfig {
            enabled: true,
            application_id: "1234567890".into(),
        };
        assert!(DiscordHandle::new(cfg).is_ok());
    }

    fn track() -> MediaItem {
        MediaItem {
            uri: "spotify:track:1".into(),
            name: "Never Too Much".into(),
            subtitle: "Luther Vandross".into(),
            album: Some("Never Too Much".into()),
            ..MediaItem::default()
        }
    }

    #[test]
    fn pause_clears_presence() {
        assert!(matches!(
            desired_presence("paused", None),
            Some(Desired::Clear)
        ));
    }

    #[test]
    fn track_change_shows_track_metadata() {
        let playback = Playback {
            item: Some(track()),
            is_playing: true,
            ..Playback::default()
        };
        match desired_presence("track changed spotify:track:1", Some(&playback)) {
            Some(Desired::Show {
                details,
                state,
                large_text,
                ..
            }) => {
                assert_eq!(details, "Never Too Much");
                assert_eq!(state, "Luther Vandross");
                assert_eq!(large_text, "Never Too Much");
            }
            other => panic!("expected Show, got {:?}", other.is_some()),
        }
    }

    #[test]
    fn volume_and_seek_leave_presence_unchanged() {
        assert!(desired_presence("volume 40", None).is_none());
        assert!(desired_presence("seek", None).is_none());
    }
}
