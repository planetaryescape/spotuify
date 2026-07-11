//! Phase 14 (P14-C) — souvlaki media controls.
//!
//! Bridges OS media-key + Now-Playing widgets (MPRIS on Linux,
//! MediaRemote on macOS, SMTC on Windows) to spotuify's
//! `Request::PlaybackCommand`. Rate-limited to 1 update/second per
//! souvlaki's documented best practice (D-Bus flooding warning).
//!
//! On macOS / Windows souvlaki needs a real window handle; we spawn a
//! hidden message-only winit window in a dedicated thread (mirrors
//! spotify-player's `media_control.rs:160-263`). The daemon-only
//! deployment without a UI process emits
//! `DaemonEvent::MediaControlsUnavailable` and degrades gracefully.

#[cfg(not(target_os = "windows"))]
use std::sync::Mutex;
use std::time::Duration;

#[cfg(not(target_os = "windows"))]
use anyhow::Context;
use spotuify_protocol::{DaemonEvent, PlaybackCommand};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct MediaControlsConfig {
    pub enabled: bool,
    /// When false on mac/win, skip the hidden-window setup and emit
    /// `MediaControlsUnavailable` once. CLI flag is
    /// `--no-media-controls`.
    pub allow_hidden_window: bool,
}

impl Default for MediaControlsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allow_hidden_window: true,
        }
    }
}

pub struct MediaControlsHandle {
    config: MediaControlsConfig,
    bus_name: String,
    commands_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<PlaybackCommand>>,
    /// On Linux/macOS souvlaki runs in-process (MPRIS / MediaRemote). On
    /// Windows the SMTC must live on its hidden-window thread, so the
    /// controls are owned there and reached via `win` instead.
    #[cfg(not(target_os = "windows"))]
    controls: Mutex<souvlaki::MediaControls>,
    #[cfg(target_os = "windows")]
    win: crate::media_controls_win::WindowsMediaControls,
}

impl MediaControlsHandle {
    pub fn new(config: MediaControlsConfig) -> anyhow::Result<Self> {
        let bus_name = format!("spotuify.instance{}", std::process::id());
        let (commands_tx, commands_rx) = mpsc::unbounded_channel();

        #[cfg(not(target_os = "windows"))]
        {
            let mut controls = souvlaki::MediaControls::new(souvlaki::PlatformConfig {
                display_name: "spotuify",
                dbus_name: &bus_name,
                hwnd: None,
            })
            .context("failed to create souvlaki media controls")?;
            controls
                .attach(move |event| {
                    if let Some(command) =
                        souvlaki_event_to_action(event).and_then(map_media_control_event)
                    {
                        let _ = commands_tx.send(command);
                    }
                })
                .context("failed to attach souvlaki media controls")?;
            Ok(Self {
                config,
                bus_name,
                commands_rx: tokio::sync::Mutex::new(commands_rx),
                controls: Mutex::new(controls),
            })
        }

        #[cfg(target_os = "windows")]
        {
            // The daemon has no UI; the SMTC needs a real window. Honour
            // the `--no-media-controls` opt-out by skipping the driver.
            if !config.allow_hidden_window {
                anyhow::bail!(
                    "Windows media controls disabled without a window handle (--no-media-controls)"
                );
            }
            let win = crate::media_controls_win::WindowsMediaControls::new(
                bus_name.clone(),
                commands_tx,
            )?;
            Ok(Self {
                config,
                bus_name,
                commands_rx: tokio::sync::Mutex::new(commands_rx),
                win,
            })
        }
    }

    pub fn bus_name(&self) -> &str {
        &self.bus_name
    }

    pub async fn recv_command(&self) -> Option<PlaybackCommand> {
        self.commands_rx.lock().await.recv().await
    }

    /// Fan a `PlaybackChanged` out to the OS media controls: push the
    /// track metadata (title / artist / album / cover / duration) from
    /// the event's playback snapshot, then the play/pause state. The
    /// cadence cap is enforced inside the souvlaki driver loop.
    pub async fn handle(&self, event: &DaemonEvent) {
        if !self.config.enabled {
            return;
        }
        let DaemonEvent::PlaybackChanged { action, playback } = event else {
            return;
        };

        #[cfg(not(target_os = "windows"))]
        {
            let Ok(mut controls) = self.controls.lock() else {
                return;
            };
            if let Some(item) = playback.as_ref().and_then(|p| p.item.as_ref()) {
                let album = item
                    .album
                    .as_deref()
                    .filter(|a| !a.is_empty())
                    .unwrap_or(item.context.as_str());
                let metadata = souvlaki::MediaMetadata {
                    title: Some(item.name.as_str()),
                    artist: Some(item.subtitle.as_str()),
                    album: (!album.is_empty()).then_some(album),
                    // souvlaki loads http(s)/file URLs per platform; the
                    // Spotify CDN URL works for MPRIS/SMTC/NowPlaying.
                    cover_url: item.image_url.as_deref(),
                    duration: (item.duration_ms > 0)
                        .then(|| std::time::Duration::from_millis(item.duration_ms)),
                };
                if let Err(err) = controls.set_metadata(metadata) {
                    tracing::warn!(error = %err, "media-controls metadata update failed");
                }
            }
            if let Some(state) = media_playback_state(action, playback.as_ref()) {
                if let Err(err) = controls.set_playback(state) {
                    tracing::warn!(error = %err, "media-controls playback update failed");
                }
            }
        }

        // Windows owns the souvlaki controls on the SMTC thread; marshal
        // owned metadata/playback over the event-loop proxy instead.
        #[cfg(target_os = "windows")]
        {
            use crate::media_controls_win::{ControlUpdate, PlaybackKind};
            if let Some(item) = playback.as_ref().and_then(|p| p.item.as_ref()) {
                let album = item
                    .album
                    .as_deref()
                    .filter(|a| !a.is_empty())
                    .unwrap_or(item.context.as_str());
                self.win.send(ControlUpdate::Metadata {
                    title: Some(item.name.clone()),
                    artist: Some(item.subtitle.clone()),
                    album: (!album.is_empty()).then(|| album.to_string()),
                    cover_url: item.image_url.clone(),
                    duration_ms: (item.duration_ms > 0).then_some(item.duration_ms),
                });
            }
            if let Some(state) = media_playback_state(action, playback.as_ref()) {
                let kind = match state {
                    souvlaki::MediaPlayback::Playing { progress } => {
                        PlaybackKind::Playing(progress.map(|p| p.0.as_millis() as u64))
                    }
                    souvlaki::MediaPlayback::Paused { progress } => {
                        PlaybackKind::Paused(progress.map(|p| p.0.as_millis() as u64))
                    }
                    souvlaki::MediaPlayback::Stopped => return,
                };
                self.win.send(ControlUpdate::Playback(kind));
            }
        }
    }
}

/// Map a `PlaybackChanged` to a souvlaki play/pause state. Prefers the
/// snapshot's `is_playing` (with progress); falls back to the action
/// label when no snapshot rode along.
fn media_playback_state(
    action: &str,
    playback: Option<&spotuify_core::Playback>,
) -> Option<souvlaki::MediaPlayback> {
    if let Some(pb) = playback {
        let progress = Some(souvlaki::MediaPosition(std::time::Duration::from_millis(
            pb.progress_ms,
        )));
        return Some(if pb.is_playing {
            souvlaki::MediaPlayback::Playing { progress }
        } else {
            souvlaki::MediaPlayback::Paused { progress }
        });
    }
    if action == "paused" || action == "pause" {
        return Some(souvlaki::MediaPlayback::Paused { progress: None });
    }
    if action == "resumed"
        || action == "resume"
        || action == "play-uri"
        || action.starts_with("started ")
        || action.starts_with("track changed ")
    {
        return Some(souvlaki::MediaPlayback::Playing { progress: None });
    }
    None
}

/// Phase 14 (P14-C) — pure mapping from souvlaki `MediaControlEvent`
/// to spotuify's `PlaybackCommand`. The async driver loop (not part
/// of the unit-testable surface) calls this on every key event.
pub fn map_media_control_event(action: SouvlakiAction) -> Option<PlaybackCommand> {
    use SouvlakiAction as A;
    match action {
        A::Play => Some(PlaybackCommand::Resume),
        A::Pause => Some(PlaybackCommand::Pause),
        A::Toggle => Some(PlaybackCommand::Toggle),
        A::Next => Some(PlaybackCommand::Next),
        A::Previous => Some(PlaybackCommand::Previous),
        A::SeekToMs(ms) => Some(PlaybackCommand::Seek { position_ms: ms }),
        A::SetVolume(pct) => Some(PlaybackCommand::Volume {
            volume_percent: pct.clamp(0, 100),
        }),
        A::OpenUri(uri) => Some(PlaybackCommand::PlayUri {
            uri,
            context_uri: None,
        }),
        A::Stop | A::Quit | A::Raise => None,
    }
}

pub(crate) fn souvlaki_event_to_action(
    event: souvlaki::MediaControlEvent,
) -> Option<SouvlakiAction> {
    match event {
        souvlaki::MediaControlEvent::Play => Some(SouvlakiAction::Play),
        souvlaki::MediaControlEvent::Pause => Some(SouvlakiAction::Pause),
        souvlaki::MediaControlEvent::Toggle => Some(SouvlakiAction::Toggle),
        souvlaki::MediaControlEvent::Next => Some(SouvlakiAction::Next),
        souvlaki::MediaControlEvent::Previous => Some(SouvlakiAction::Previous),
        souvlaki::MediaControlEvent::Stop => Some(SouvlakiAction::Stop),
        souvlaki::MediaControlEvent::SetPosition(position) => {
            Some(SouvlakiAction::SeekToMs(duration_ms(position.0)))
        }
        souvlaki::MediaControlEvent::SetVolume(volume) => {
            let percent = (volume * 100.0).round().clamp(0.0, 100.0) as u8;
            Some(SouvlakiAction::SetVolume(percent))
        }
        souvlaki::MediaControlEvent::OpenUri(uri) => Some(SouvlakiAction::OpenUri(uri)),
        souvlaki::MediaControlEvent::Raise => Some(SouvlakiAction::Raise),
        souvlaki::MediaControlEvent::Quit => Some(SouvlakiAction::Quit),
        souvlaki::MediaControlEvent::Seek(_) | souvlaki::MediaControlEvent::SeekBy(_, _) => None,
    }
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

/// A subset of souvlaki's MediaControlEvent that we project into
/// spotuify's PlaybackCommand. Keeping a local enum keeps the mapping
/// unit-testable without depending on the souvlaki types in the test
/// binary (which would pull in the OS subsystem).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SouvlakiAction {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    Stop,
    Quit,
    Raise,
    SeekToMs(u64),
    SetVolume(u8),
    OpenUri(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_control_play_maps_to_resume_not_play_uri() {
        // souvlaki "play" means resume current track, not start a new
        // URI. Mapping it to PlayUri would require knowing the URI we
        // were last playing, which we don't carry here.
        assert_eq!(
            map_media_control_event(SouvlakiAction::Play),
            Some(PlaybackCommand::Resume)
        );
    }

    #[test]
    fn media_control_toggle_routes_to_playback_toggle() {
        assert_eq!(
            map_media_control_event(SouvlakiAction::Toggle),
            Some(PlaybackCommand::Toggle)
        );
    }

    #[test]
    fn media_control_volume_clamps_above_100() {
        // souvlaki sends u8 volumes; macOS sometimes overshoots by a
        // percent. We clamp to 100 so Spotify doesn't reject the
        // request and the user keeps audio.
        assert_eq!(
            map_media_control_event(SouvlakiAction::SetVolume(110)),
            Some(PlaybackCommand::Volume {
                volume_percent: 100
            })
        );
    }

    #[test]
    fn media_control_stop_and_quit_drop_to_none() {
        // spotuify's Request enum has no Stop / Quit equivalent — the
        // daemon owns its own lifecycle. Returning None means the
        // bridge silently ignores the key.
        assert_eq!(map_media_control_event(SouvlakiAction::Stop), None);
        assert_eq!(map_media_control_event(SouvlakiAction::Quit), None);
    }

    #[test]
    fn souvlaki_set_position_maps_to_absolute_seek() {
        let action = souvlaki_event_to_action(souvlaki::MediaControlEvent::SetPosition(
            souvlaki::MediaPosition(Duration::from_millis(42_500)),
        ));

        assert_eq!(action, Some(SouvlakiAction::SeekToMs(42_500)));
    }

    #[test]
    fn playback_events_update_souvlaki_state() {
        // No snapshot rode along (playback = None) → fall back to the
        // action label.
        assert_eq!(
            media_playback_state("paused", None),
            Some(souvlaki::MediaPlayback::Paused { progress: None })
        );
        assert_eq!(
            media_playback_state("started spotify:track:1", None),
            Some(souvlaki::MediaPlayback::Playing { progress: None })
        );
    }
}
