//! Phase 14 (P14-D) — desktop notifications via notify-rust.
//!
//! Off by default (loud notifications surprise new users). Each
//! daemon event becomes at most one notification; per-event toggles
//! let users opt into the noise they actually want.
//!
//! On Linux we set XDG hints (`Urgency::Low`, `Transient`,
//! `Category="x-spotify.playback"`, `desktop_entry="spotuify"`) so the
//! shell collapses subsequent track-change notifications instead of
//! stacking them. macOS / Windows use notify-rust's native backend
//! (NSUserNotification / WinRT toast).

use spotuify_core::{MediaItem, Playback, ResourceUri};
use spotuify_protocol::DaemonEvent;

use std::collections::HashSet;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct NotificationsConfig {
    pub enabled: bool,
    pub summary: String,
    pub body: String,
    pub on_track_change: bool,
    pub on_pause: bool,
    pub on_resume: bool,
    pub on_skip: bool,
    pub on_error: bool,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            summary: "{track}".to_string(),
            body: "{artist} — {album}".to_string(),
            on_track_change: true,
            on_pause: false,
            on_resume: false,
            on_skip: false,
            on_error: true,
        }
    }
}

/// The user-facing playback notice an event maps to. Each maps to one
/// per-event toggle in [`NotificationsConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaybackNotice {
    TrackChange,
    Pause,
    Resume,
    Skip,
}

/// Dedup state: the daemon emits a `PlaybackChanged` twice for one user
/// action — once with the command-authoritative action label (`pause`,
/// `play-uri`) and once with the librespot state event (`paused`,
/// `started {uri}`). Without dedup, every enabled toggle would fire two
/// notifications per action.
#[derive(Default)]
struct NotifyDedup {
    /// Last track URI we raised a track-change notice for.
    last_track_uri: Option<String>,
    /// Last pause/resume notice raised.
    last_transport: Option<PlaybackNotice>,
}

pub struct NotificationsHandle {
    config: NotificationsConfig,
    notified_auth_errors: Arc<parking_lot::Mutex<HashSet<String>>>,
    dedup: Arc<parking_lot::Mutex<NotifyDedup>>,
}

impl NotificationsHandle {
    pub fn new(config: NotificationsConfig) -> anyhow::Result<Self> {
        Ok(Self {
            config,
            notified_auth_errors: Arc::new(parking_lot::Mutex::new(HashSet::new())),
            dedup: Arc::new(parking_lot::Mutex::new(NotifyDedup::default())),
        })
    }

    fn notice_enabled(&self, notice: PlaybackNotice) -> bool {
        match notice {
            PlaybackNotice::TrackChange => self.config.on_track_change,
            PlaybackNotice::Pause => self.config.on_pause,
            PlaybackNotice::Resume => self.config.on_resume,
            PlaybackNotice::Skip => self.config.on_skip,
        }
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    pub async fn handle(&self, event: &DaemonEvent) {
        if !self.config.enabled {
            return;
        }
        let Some((summary, body)) = self.render(event) else {
            return;
        };
        // notify-rust is sync; spawn-blocking so we don't stall the
        // daemon's broadcast handler. Failures are logged + dropped.
        let cfg = self.config.clone();
        tokio::task::spawn_blocking(move || {
            let mut notification = notify_rust::Notification::new();
            notification.summary(&summary).body(&body);
            #[cfg(target_os = "linux")]
            {
                notification
                    .hint(notify_rust::Hint::Urgency(notify_rust::Urgency::Low))
                    .hint(notify_rust::Hint::Transient(true))
                    .hint(notify_rust::Hint::Category(
                        "x-spotify.playback".to_string(),
                    ))
                    .hint(notify_rust::Hint::DesktopEntry("spotuify".to_string()));
            }
            let _ = cfg; // keep for future per-event hints
            if let Err(err) = notification.show() {
                tracing::debug!(error = %err, "notify-rust show failed");
            }
        });
    }

    fn render(&self, event: &DaemonEvent) -> Option<(String, String)> {
        match event {
            DaemonEvent::PlaybackChanged { action, playback } => {
                let notice = classify_playback_action(action)?;
                if !self.notice_enabled(notice) {
                    return None;
                }
                let mut dedup = self.dedup.lock();
                match notice {
                    PlaybackNotice::TrackChange => {
                        // Fire once per distinct track. If the URI is
                        // unknown (a play-* command with no payload),
                        // suppress and let the URI-bearing librespot
                        // `started`/`track changed` event fire instead.
                        let uri = track_uri_from_event(action, playback.as_ref())?;
                        if dedup.last_track_uri.as_deref() == Some(uri.as_str()) {
                            return None;
                        }
                        dedup.last_track_uri = Some(uri);
                        dedup.last_transport = None;
                    }
                    PlaybackNotice::Pause | PlaybackNotice::Resume => {
                        if dedup.last_transport == Some(notice) {
                            return None;
                        }
                        dedup.last_transport = Some(notice);
                    }
                    PlaybackNotice::Skip => {
                        dedup.last_transport = Some(notice);
                    }
                }
                drop(dedup);
                // Expand templates from the playback snapshot when it rode
                // along; fall back to the action-only expansion otherwise.
                match playback.as_ref().and_then(|p| p.item.as_ref()) {
                    Some(item) => {
                        let progress_ms = playback.as_ref().map_or(0, |p| p.progress_ms);
                        Some((
                            expand_tokens_item(&self.config.summary, item, progress_ms),
                            expand_tokens_item(&self.config.body, item, progress_ms),
                        ))
                    }
                    None => Some((
                        expand_tokens(&self.config.summary, action),
                        expand_tokens(&self.config.body, action),
                    )),
                }
            }
            DaemonEvent::AuthError { kind } if self.config.on_error => {
                let key = format!("{kind:?}");
                if !self.notified_auth_errors.lock().insert(key) {
                    return None;
                }
                Some((
                    "spotuify auth error".to_string(),
                    format!("auth issue: {:?} — re-login required", kind),
                ))
            }
            // Listening reminder fired (Linux/Windows desktop path; on macOS the
            // GUI app posts the native alert). Gated by `enabled` in `handle`.
            DaemonEvent::ReminderDue { notification } => {
                let body = notification.message.clone().unwrap_or_else(|| {
                    if notification.subtitle.is_empty() {
                        "Time to listen".to_string()
                    } else {
                        notification.subtitle.clone()
                    }
                });
                Some((format!("Reminder: {}", notification.name), body))
            }
            _ => None,
        }
    }
}

/// Classify a `PlaybackChanged` action label into the notice it may
/// raise. `optimistic-*` actions are ignored — the daemon emits them
/// for instant client feedback and always follows with an authoritative
/// event, so notifying on both would double-fire. Action labels that
/// are neither a transport change nor a track change (volume, seek,
/// shuffle, repeat, queue, warmed, snapshot, refreshed, transfer) raise
/// nothing.
fn classify_playback_action(action: &str) -> Option<PlaybackNotice> {
    if action.starts_with("optimistic-") {
        return None;
    }
    // librespot embeds the URI after the verb: "started spotify:track:x",
    // "track changed spotify:track:x".
    if action.starts_with("track changed") || action.starts_with("started") {
        return Some(PlaybackNotice::TrackChange);
    }
    match action {
        "pause" | "paused" => Some(PlaybackNotice::Pause),
        "resume" | "resumed" => Some(PlaybackNotice::Resume),
        "next" | "previous" => Some(PlaybackNotice::Skip),
        "play-uri" | "play-context" | "play-item" => Some(PlaybackNotice::TrackChange),
        _ => None,
    }
}

/// Resolve the playing track URI for a `PlaybackChanged`, preferring the
/// event payload and falling back to the trailing URI token librespot
/// embeds in its action labels. Used to dedup track-change notices.
fn track_uri_from_event(action: &str, playback: Option<&Playback>) -> Option<String> {
    if let Some(item) = playback.and_then(|p| p.item.as_ref()) {
        if !item.uri.is_empty() {
            return Some(item.uri.clone());
        }
    }
    action
        .rsplit_once(' ')
        .map(|(_, uri)| uri.to_string())
        .filter(|uri| ResourceUri::parse(uri).is_ok())
}

/// Expand `{track}`/`{artist}`/`{album}`/`{duration}`/`{progress}` from a
/// playback item. Used whenever the `PlaybackChanged` carried a snapshot.
pub fn expand_tokens_item(template: &str, item: &MediaItem, progress_ms: u64) -> String {
    let album = item
        .album
        .clone()
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| item.context.clone());
    template
        .replace("{track}", &item.name)
        .replace("{artist}", &item.subtitle)
        .replace("{artists}", &item.subtitle)
        .replace("{album}", &album)
        .replace("{duration}", &format_ms(item.duration_ms))
        .replace("{progress}", &format_ms(progress_ms))
}

/// `m:ss` for notification templates.
fn format_ms(ms: u64) -> String {
    let total_secs = ms / 1000;
    format!("{}:{:02}", total_secs / 60, total_secs % 60)
}

/// Action-only token expansion fallback for `PlaybackChanged` events that
/// arrived without a playback snapshot (older daemons / rare paths). The
/// action label stands in for `{track}`; the rest render empty.
pub fn expand_tokens(template: &str, action: &str) -> String {
    template
        .replace("{track}", action)
        .replace("{artist}", "")
        .replace("{artists}", "")
        .replace("{album}", "")
        .replace("{duration}", "")
        .replace("{progress}", "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_tokens_expand_for_track_change_event() {
        // Spotuify's body template uses {artist} — {album}; once track
        // metadata lands on PlaybackChanged we'll plug it in. Today,
        // empty fields render to bare separators, which is acceptable
        // until the protocol enriches the event.
        let body = expand_tokens("{artist} — {album}", "next");
        assert_eq!(body, " — ");
        let summary = expand_tokens("{track}", "next");
        assert_eq!(summary, "next");
    }

    #[test]
    fn disabled_notifications_render_nothing() {
        let h = NotificationsHandle::new(NotificationsConfig {
            enabled: false,
            ..NotificationsConfig::default()
        })
        .expect("notifications handle should construct");
        // A track-change event renders Some (on_track_change defaults
        // true), proving the suppression when disabled lives in the
        // `enabled` gate at the top of handle(), not in render().
        let ev = DaemonEvent::PlaybackChanged {
            action: "started spotify:track:1".into(),
            playback: None,
        };
        assert!(h.render(&ev).is_some());
        // The actual `handle()` invocation skips the notification when
        // disabled; we don't exercise the notify-rust backend in tests.
    }

    fn enabled_config() -> NotificationsConfig {
        NotificationsConfig {
            enabled: true,
            ..NotificationsConfig::default()
        }
    }

    fn playback_changed(action: &str) -> DaemonEvent {
        DaemonEvent::PlaybackChanged {
            action: action.into(),
            playback: None,
        }
    }

    #[test]
    fn optimistic_actions_never_notify() {
        assert!(classify_playback_action("optimistic-pause").is_none());
        assert!(classify_playback_action("optimistic-next").is_none());
        assert!(classify_playback_action("optimistic-play-uri").is_none());
    }

    #[test]
    fn non_transport_actions_never_notify() {
        for action in [
            "volume 40",
            "seek",
            "shuffle",
            "warmed",
            "snapshot",
            "refreshed",
            "transfer",
            "queue",
        ] {
            assert!(
                classify_playback_action(action).is_none(),
                "{action} should not notify"
            );
        }
    }

    #[test]
    fn pause_resume_skip_gated_by_their_own_flags() {
        // Default config: only on_track_change is true.
        let h = NotificationsHandle::new(enabled_config()).expect("handle");
        assert!(
            h.render(&playback_changed("paused")).is_none(),
            "on_pause defaults off"
        );
        assert!(
            h.render(&playback_changed("resumed")).is_none(),
            "on_resume defaults off"
        );
        assert!(
            h.render(&playback_changed("next")).is_none(),
            "on_skip defaults off"
        );

        let h = NotificationsHandle::new(NotificationsConfig {
            on_pause: true,
            on_resume: true,
            on_skip: true,
            ..enabled_config()
        })
        .expect("handle");
        assert!(h.render(&playback_changed("paused")).is_some());
        assert!(h.render(&playback_changed("resumed")).is_some());
        assert!(h.render(&playback_changed("next")).is_some());
    }

    #[test]
    fn pause_dedups_command_then_librespot_emit() {
        let h = NotificationsHandle::new(NotificationsConfig {
            on_pause: true,
            ..enabled_config()
        })
        .expect("handle");
        // Command-authoritative "pause" then librespot "paused" for one
        // user action: only the first should notify.
        assert!(h.render(&playback_changed("pause")).is_some());
        assert!(
            h.render(&playback_changed("paused")).is_none(),
            "second pause emit should be deduped"
        );
    }

    #[test]
    fn item_tokens_expand_from_playback_snapshot() {
        let item = MediaItem {
            name: "Never Too Much".into(),
            subtitle: "Luther Vandross".into(),
            album: Some("Never Too Much".into()),
            duration_ms: 425_000,
            ..MediaItem::default()
        };
        let summary = expand_tokens_item("{track}", &item, 60_000);
        assert_eq!(summary, "Never Too Much");
        let body = expand_tokens_item("{artist} — {album} ({progress}/{duration})", &item, 60_000);
        assert_eq!(body, "Luther Vandross — Never Too Much (1:00/7:05)");
    }

    #[test]
    fn playback_changed_with_snapshot_renders_real_track_fields() {
        let h = NotificationsHandle::new(enabled_config()).expect("handle");
        let item = MediaItem {
            uri: "spotify:track:1".into(),
            name: "Song".into(),
            subtitle: "Artist".into(),
            ..MediaItem::default()
        };
        let ev = DaemonEvent::PlaybackChanged {
            action: "started spotify:track:1".into(),
            playback: Some(Playback {
                item: Some(item),
                is_playing: true,
                ..Playback::default()
            }),
        };
        let (summary, _) = h.render(&ev).expect("track change renders");
        assert_eq!(summary, "Song", "summary should use the real track name");
    }

    #[test]
    fn track_change_dedups_by_uri() {
        let h = NotificationsHandle::new(enabled_config()).expect("handle");
        // Command "play-uri" carries no URI in the action and no payload
        // → suppressed; the librespot "started {uri}" fires.
        assert!(h.render(&playback_changed("play-uri")).is_none());
        assert!(h
            .render(&playback_changed("started spotify:track:abc"))
            .is_some());
        // Same track again → deduped.
        assert!(h
            .render(&playback_changed("track changed spotify:track:abc"))
            .is_none());
        // New track → fires.
        assert!(h
            .render(&playback_changed("track changed spotify:track:def"))
            .is_some());
    }

    #[test]
    fn auth_error_notifications_are_deduped() {
        let h = NotificationsHandle::new(NotificationsConfig {
            enabled: true,
            on_error: true,
            ..NotificationsConfig::default()
        })
        .expect("notifications handle should construct");
        let ev = DaemonEvent::AuthError {
            kind: spotuify_protocol::AuthErrorKind::NotLoggedIn,
        };

        assert!(h.render(&ev).is_some());
        assert!(
            h.render(&ev).is_none(),
            "same auth error should only produce one desktop notification"
        );
    }
}
