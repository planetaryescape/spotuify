//! Phase 2 — `PlaybackClock`.
//!
//! In-memory single source of truth for "what is playing, where, since when".
//! The daemon owns it; the rest of the system (handlers, TUI, MCP) reads
//! from it through `snapshot()` which returns a `spotuify_core::Playback`
//! with progress derived from `base_progress_ms + monotonic_elapsed`.
//!
//! Why this exists: before Phase 2, every `PlaybackGet` returned the
//! latest persisted snapshot from SQLite. While playing, that meant the
//! progress bar advanced only when a Web API poll happened (every few
//! seconds), then jumped. After Phase 2, `PlaybackGet` is a sub-millisecond
//! `RwLock` read that extrapolates locally; polls only re-seat the base.
//!
//! Reseat priority (highest wins; ties broken by `sampled_at_ms`):
//!   PlayerEvent > CommandResult > WebApiPoll > Cache > RecentFallback
//!
//! A URI change always wins, regardless of priority — switching tracks
//! invalidates extrapolation.
//!
//! Concurrency:
//! - `parking_lot::RwLock` (not `tokio::sync::Mutex`) — readers don't
//!   `await`, snapshot is microseconds, multiple `snapshot()` callers
//!   share the read lock.
//! - Writers (`apply_*`) hold the write lock for a handful of word
//!   writes; never `.await` while holding.
//! - `std::time::Instant` for the monotonic baseline; immune to NTP
//!   steps and sleep/resume.

use std::sync::Arc;
use std::time::Instant;

use parking_lot::RwLock;

use spotuify_core::{Device, MediaItem, Playback, PlaybackStateSource};
use spotuify_player::PlayerEvent;

/// Drift threshold (ms) before a `PositionTick` re-seats the clock.
/// Below this is tick jitter — librespot's worker tick is ~400ms,
/// network jitter is sub-second, so 1500ms preserves smooth local
/// extrapolation while catching real corrections (Seeked,
/// PositionCorrection, sink underrun).
pub const POSITION_DRIFT_THRESHOLD_MS: i64 = 1_500;

#[derive(Debug)]
pub struct PlaybackClock {
    inner: RwLock<ClockState>,
}

#[derive(Debug, Clone)]
struct ClockState {
    item: Option<MediaItem>,
    device: Option<Device>,
    is_playing: bool,
    base_progress_ms: u64,
    base_instant: Instant,
    provider_timestamp_ms: Option<i64>,
    sampled_at_ms: i64,
    shuffle: bool,
    repeat: String,
    source: PlaybackStateSource,
}

impl ClockState {
    fn empty() -> Self {
        Self {
            item: None,
            device: None,
            is_playing: false,
            base_progress_ms: 0,
            base_instant: Instant::now(),
            provider_timestamp_ms: None,
            sampled_at_ms: 0,
            shuffle: false,
            repeat: "off".to_string(),
            source: PlaybackStateSource::RecentFallback,
        }
    }

    fn current_progress_ms(&self, now: Instant) -> u64 {
        if !self.is_playing {
            return self.clamp_to_duration(self.base_progress_ms);
        }
        let elapsed_ms = now.saturating_duration_since(self.base_instant).as_millis() as u64;
        self.clamp_to_duration(self.base_progress_ms.saturating_add(elapsed_ms))
    }

    fn clamp_to_duration(&self, ms: u64) -> u64 {
        match self.item.as_ref() {
            Some(item) if item.duration_ms > 0 => ms.min(item.duration_ms),
            _ => ms,
        }
    }
}

impl PlaybackClock {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(ClockState::empty()),
        })
    }

    /// Seed from the durable store at daemon start. Marks the source
    /// as `Cache` (or `RecentFallback` when the input is a synthetic
    /// "last played" snapshot — caller decides by passing the right
    /// source). After seeding, the next `apply_player_event` or
    /// `apply_command_result` will overwrite this with truthier state.
    pub fn seed_from_cache(
        &self,
        playback: Playback,
        source: PlaybackStateSource,
        sampled_at_ms: i64,
    ) {
        let mut st = self.inner.write();
        st.item = playback.item;
        st.device = playback.device;
        st.is_playing = playback.is_playing;
        st.base_progress_ms = playback.progress_ms;
        st.base_instant = Instant::now();
        st.shuffle = playback.shuffle;
        st.repeat = playback.repeat;
        st.provider_timestamp_ms = playback.provider_timestamp_ms;
        st.sampled_at_ms = sampled_at_ms;
        st.source = source;
    }

    /// Read the current best-known playback state. Pure read — no
    /// blocking, no I/O. Progress is computed against the monotonic
    /// clock; the rest is a clone of the stored snapshot.
    pub fn snapshot(&self) -> Playback {
        let st = self.inner.read();
        let now = Instant::now();
        Playback {
            item: st.item.clone(),
            device: st.device.clone(),
            is_playing: st.is_playing,
            progress_ms: st.current_progress_ms(now),
            shuffle: st.shuffle,
            repeat: st.repeat.clone(),
            sampled_at_ms: Some(st.sampled_at_ms),
            provider_timestamp_ms: st.provider_timestamp_ms,
            source: Some(st.source),
        }
    }

    /// Apply a librespot/local player event. PlayerEvent is the
    /// highest-priority source — overwrites command-result and
    /// Web-API state unconditionally on transitions, and re-anchors
    /// the clock when `PositionTick` drift exceeds the threshold.
    pub fn apply_player_event(&self, ev: &PlayerEvent, now_ms: i64) {
        let mut st = self.inner.write();
        match ev {
            PlayerEvent::PlaybackStarted { uri, position_ms }
            | PlayerEvent::TrackChanged { uri, position_ms } => {
                if st.item.as_ref().map(|i| i.uri.as_str()) != Some(uri.as_str()) {
                    // URI change: keep the existing MediaItem if it
                    // already matches; otherwise replace with a stub
                    // so the next snapshot at least carries the URI.
                    if st.item.as_ref().map(|i| i.uri.as_str()) != Some(uri.as_str()) {
                        st.item = Some(MediaItem {
                            uri: uri.clone(),
                            ..Default::default()
                        });
                    }
                }
                st.base_progress_ms = *position_ms as u64;
                st.base_instant = Instant::now();
                st.is_playing = true;
                st.source = PlaybackStateSource::PlayerEvent;
                st.sampled_at_ms = now_ms;
            }
            PlayerEvent::PlaybackPaused => {
                // Freeze at the current extrapolated progress.
                let frozen = st.current_progress_ms(Instant::now());
                st.base_progress_ms = frozen;
                st.base_instant = Instant::now();
                st.is_playing = false;
                st.source = PlaybackStateSource::PlayerEvent;
                st.sampled_at_ms = now_ms;
            }
            PlayerEvent::PlaybackResumed => {
                st.base_instant = Instant::now();
                st.is_playing = true;
                st.source = PlaybackStateSource::PlayerEvent;
                st.sampled_at_ms = now_ms;
            }
            PlayerEvent::PositionTick { position_ms } => {
                let extrapolated = st.current_progress_ms(Instant::now()) as i64;
                let real = *position_ms as i64;
                if (extrapolated - real).abs() > POSITION_DRIFT_THRESHOLD_MS {
                    st.base_progress_ms = real as u64;
                    st.base_instant = Instant::now();
                    st.source = PlaybackStateSource::PlayerEvent;
                    st.sampled_at_ms = now_ms;
                    tracing::warn!(
                        target: "spotuify_daemon::clock",
                        extrapolated,
                        real,
                        "clock_drift_reanchored"
                    );
                }
            }
            PlayerEvent::EndOfTrack { .. } => {
                if let Some(item) = st.item.as_ref() {
                    if item.duration_ms > 0 {
                        st.base_progress_ms = item.duration_ms;
                    }
                }
                st.base_instant = Instant::now();
                st.is_playing = false;
                st.source = PlaybackStateSource::PlayerEvent;
                st.sampled_at_ms = now_ms;
            }
            _ => {
                // Ready/Degraded/PremiumRequired/SessionDisconnected/Failed/
                // PreloadNext/VolumeChanged don't move the playback clock;
                // volume is handled by `apply_device_volume`.
            }
        }
    }

    /// Apply a `PlayerEvent::VolumeChanged`. The embedded device's real
    /// volume only reaches us through librespot (the Web API reports it as
    /// `null`), so this keeps the snapshot's device volume truthful without
    /// waiting on a poll. Updates the current device's volume in place; if
    /// no device is known yet (pre-poll, e.g. right after activation),
    /// seeds it so the now-playing volume row has something to render.
    pub fn apply_device_volume(
        &self,
        percent: u8,
        seed: impl FnOnce() -> Option<Device>,
        now_ms: i64,
    ) {
        let mut st = self.inner.write();
        match st.device.as_mut() {
            Some(device) => device.volume_percent = Some(percent),
            None => st.device = seed(),
        }
        st.sampled_at_ms = now_ms;
    }

    /// Apply a `CommandResult.playback` returned by `actions::execute`.
    /// Authoritative — overwrites cache/Web-API state. Captured-seq
    /// guard is the caller's responsibility (handler enforces it via
    /// `may_apply_state_update` before calling us).
    pub fn apply_command_result(&self, playback: &Playback, sampled_at_ms: i64) {
        let mut st = self.inner.write();
        let same_uri = matches_uri(&st.item, &playback.item);
        st.item = playback.item.clone();
        st.device = playback.device.clone();
        st.is_playing = playback.is_playing;
        st.base_progress_ms = playback.progress_ms;
        st.base_instant = Instant::now();
        st.shuffle = playback.shuffle;
        st.repeat = playback.repeat.clone();
        // CommandResult shouldn't be older than a previous PlayerEvent
        // for the same URI; if a PlayerEvent has already advanced past
        // the command's stale progress, the event wins (it was sampled
        // later AND the priority is higher). But for a fresh URI we
        // always rebase.
        if !same_uri || st.source != PlaybackStateSource::PlayerEvent {
            st.source = PlaybackStateSource::CommandResult;
            st.sampled_at_ms = sampled_at_ms;
        }
    }

    /// Apply a Web API poll. Lowest-trust real source. Rejected when
    /// the daemon's `mutation_seq` has advanced past the seq captured
    /// at the start of the poll (in-flight mutation). Also rejected
    /// when a higher-priority sample was applied AFTER `sampled_at_ms`
    /// for the same track (player-event truth always wins).
    ///
    /// Returns `true` when the sample replaced the stored state.
    pub fn apply_web_api_poll(
        &self,
        playback: &Playback,
        captured_seq: u64,
        state_seq: u64,
        sampled_at_ms: i64,
        provider_timestamp_ms: Option<i64>,
    ) -> bool {
        if captured_seq != state_seq {
            return false;
        }
        let mut st = self.inner.write();
        let same_uri = matches_uri(&st.item, &playback.item);
        if same_uri {
            // Same track: only re-seat if our current source is weaker
            // OR significantly older.
            let beats_priority =
                source_priority(PlaybackStateSource::WebApiPoll) > source_priority(st.source);
            let older_than_threshold =
                (sampled_at_ms - st.sampled_at_ms) > POSITION_DRIFT_THRESHOLD_MS;
            if !(beats_priority || older_than_threshold) {
                // Update the freshness fields only — keep extrapolation.
                st.provider_timestamp_ms = provider_timestamp_ms;
                return false;
            }
        }
        st.item = playback.item.clone();
        st.device = playback.device.clone();
        st.is_playing = playback.is_playing;
        st.base_progress_ms = playback.progress_ms;
        st.base_instant = Instant::now();
        st.shuffle = playback.shuffle;
        st.repeat = playback.repeat.clone();
        st.provider_timestamp_ms = provider_timestamp_ms;
        st.source = PlaybackStateSource::WebApiPoll;
        st.sampled_at_ms = sampled_at_ms;
        true
    }

    /// Apply a user-issued absolute seek. Pretty much an event itself
    /// — bumps progress and treats the change as authoritative.
    pub fn apply_seek(&self, position_ms: u64, sampled_at_ms: i64) {
        let mut st = self.inner.write();
        st.base_progress_ms = st.clamp_to_duration(position_ms);
        st.base_instant = Instant::now();
        st.source = PlaybackStateSource::CommandResult;
        st.sampled_at_ms = sampled_at_ms;
    }
}

fn matches_uri(a: &Option<MediaItem>, b: &Option<MediaItem>) -> bool {
    match (a.as_ref(), b.as_ref()) {
        (Some(a), Some(b)) => a.uri == b.uri,
        (None, None) => true,
        _ => false,
    }
}

fn source_priority(source: PlaybackStateSource) -> u8 {
    match source {
        PlaybackStateSource::PlayerEvent => 4,
        PlaybackStateSource::CommandResult => 3,
        PlaybackStateSource::WebApiPoll => 2,
        PlaybackStateSource::Cache => 1,
        PlaybackStateSource::RecentFallback => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spotuify_core::MediaItem;
    use spotuify_player::PlayerEvent;

    fn track(uri: &str, duration_ms: u64) -> MediaItem {
        MediaItem {
            uri: uri.to_string(),
            duration_ms,
            ..Default::default()
        }
    }

    #[test]
    fn clock_advances_while_playing() {
        let clock = PlaybackClock::new();
        clock.apply_player_event(
            &PlayerEvent::PlaybackStarted {
                uri: "track:a".to_string(),
                position_ms: 10_000,
            },
            1,
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
        let snap = clock.snapshot();
        assert!(snap.is_playing);
        assert!(
            snap.progress_ms >= 10_040 && snap.progress_ms <= 10_200,
            "expected ~10_050, got {}",
            snap.progress_ms
        );
    }

    #[test]
    fn clock_does_not_advance_while_paused() {
        let clock = PlaybackClock::new();
        clock.apply_player_event(
            &PlayerEvent::PlaybackStarted {
                uri: "track:a".to_string(),
                position_ms: 5_000,
            },
            1,
        );
        clock.apply_player_event(&PlayerEvent::PlaybackPaused, 2);
        let after_pause = clock.snapshot().progress_ms;
        std::thread::sleep(std::time::Duration::from_millis(60));
        let later = clock.snapshot().progress_ms;
        assert_eq!(after_pause, later, "paused progress must not advance");
    }

    #[test]
    fn clock_pause_freezes_at_current_progress() {
        let clock = PlaybackClock::new();
        clock.apply_player_event(
            &PlayerEvent::PlaybackStarted {
                uri: "track:a".to_string(),
                position_ms: 10_000,
            },
            1,
        );
        std::thread::sleep(std::time::Duration::from_millis(40));
        clock.apply_player_event(&PlayerEvent::PlaybackPaused, 2);
        let snap = clock.snapshot();
        assert!(!snap.is_playing);
        assert!(snap.progress_ms >= 10_030, "got {}", snap.progress_ms);
    }

    #[test]
    fn clock_resume_uses_fresh_base_instant() {
        let clock = PlaybackClock::new();
        clock.apply_player_event(
            &PlayerEvent::PlaybackStarted {
                uri: "track:a".to_string(),
                position_ms: 0,
            },
            1,
        );
        clock.apply_player_event(&PlayerEvent::PlaybackPaused, 2);
        let frozen = clock.snapshot().progress_ms;
        std::thread::sleep(std::time::Duration::from_millis(40));
        clock.apply_player_event(&PlayerEvent::PlaybackResumed, 3);
        std::thread::sleep(std::time::Duration::from_millis(40));
        let after = clock.snapshot().progress_ms;
        assert!(
            after >= frozen + 30 && after < frozen + 200,
            "expected ~{}+40, got {}",
            frozen,
            after
        );
    }

    #[test]
    fn clock_track_change_resets_progress() {
        let clock = PlaybackClock::new();
        clock.apply_player_event(
            &PlayerEvent::PlaybackStarted {
                uri: "track:a".to_string(),
                position_ms: 30_000,
            },
            1,
        );
        clock.apply_player_event(
            &PlayerEvent::TrackChanged {
                uri: "track:b".to_string(),
                position_ms: 0,
            },
            2,
        );
        let snap = clock.snapshot();
        assert_eq!(snap.item.as_ref().map(|i| i.uri.as_str()), Some("track:b"));
        assert!(snap.progress_ms < 200);
    }

    #[test]
    fn clock_position_tick_below_threshold_does_not_rebase() {
        let clock = PlaybackClock::new();
        clock.apply_player_event(
            &PlayerEvent::PlaybackStarted {
                uri: "track:a".to_string(),
                position_ms: 10_000,
            },
            1,
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
        let before = clock.snapshot().progress_ms;
        // Tiny "drift" of 100ms is normal jitter.
        clock.apply_player_event(
            &PlayerEvent::PositionTick {
                position_ms: before as u32 + 50,
            },
            2,
        );
        let after = clock.snapshot().progress_ms;
        // Should still be extrapolating from the original base; the
        // tick was within threshold so no rebase.
        assert!((after as i64 - before as i64).abs() < 200);
    }

    #[test]
    fn clock_position_tick_above_threshold_rebases() {
        let clock = PlaybackClock::new();
        clock.apply_player_event(
            &PlayerEvent::PlaybackStarted {
                uri: "track:a".to_string(),
                position_ms: 10_000,
            },
            1,
        );
        clock.apply_player_event(
            &PlayerEvent::PositionTick {
                position_ms: 30_000, // huge drift = real seek correction
            },
            2,
        );
        let snap = clock.snapshot();
        assert!(snap.progress_ms >= 29_900 && snap.progress_ms <= 30_300);
    }

    #[test]
    fn clock_web_api_poll_rejected_when_mutation_seq_advanced() {
        let clock = PlaybackClock::new();
        let pb = Playback {
            item: Some(track("track:a", 200_000)),
            is_playing: true,
            progress_ms: 50_000,
            ..Default::default()
        };
        // captured_seq=5, state_seq=6 → in-flight mutation, reject.
        let applied = clock.apply_web_api_poll(&pb, 5, 6, 1, None);
        assert!(!applied);
        assert!(clock.snapshot().item.is_none(), "clock must not be touched");
    }

    #[test]
    fn clock_web_api_poll_clobbered_by_player_event_priority() {
        let clock = PlaybackClock::new();
        // PlayerEvent at sampled_at=10
        clock.apply_player_event(
            &PlayerEvent::PlaybackStarted {
                uri: "track:a".to_string(),
                position_ms: 1_000,
            },
            10,
        );
        // Web API poll, same URI, slightly older sample → rejected.
        let pb = Playback {
            item: Some(track("track:a", 200_000)),
            is_playing: true,
            progress_ms: 5_000,
            ..Default::default()
        };
        let applied = clock.apply_web_api_poll(&pb, 1, 1, 9, None);
        assert!(!applied);
        let snap = clock.snapshot();
        assert!(snap.progress_ms < 3_000, "player event should still win");
    }

    #[test]
    fn clock_uri_change_always_rebases_even_from_web_api() {
        let clock = PlaybackClock::new();
        clock.apply_player_event(
            &PlayerEvent::PlaybackStarted {
                uri: "track:a".to_string(),
                position_ms: 50_000,
            },
            1,
        );
        let pb = Playback {
            item: Some(track("track:b", 200_000)),
            is_playing: true,
            progress_ms: 0,
            ..Default::default()
        };
        let applied = clock.apply_web_api_poll(&pb, 7, 7, 2, None);
        assert!(applied);
        assert_eq!(
            clock.snapshot().item.as_ref().map(|i| i.uri.as_str()),
            Some("track:b")
        );
    }

    #[test]
    fn clock_clamps_to_track_duration() {
        let clock = PlaybackClock::new();
        let pb = Playback {
            item: Some(track("track:a", 1_000)),
            is_playing: true,
            progress_ms: 900,
            ..Default::default()
        };
        clock.apply_command_result(&pb, 1);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let snap = clock.snapshot();
        assert_eq!(snap.progress_ms, 1_000, "must clamp to duration_ms");
    }

    #[test]
    fn clock_seek_reanchors_progress() {
        let clock = PlaybackClock::new();
        clock.apply_player_event(
            &PlayerEvent::PlaybackStarted {
                uri: "track:a".to_string(),
                position_ms: 1_000,
            },
            1,
        );
        clock.apply_seek(60_000, 2);
        let snap = clock.snapshot();
        assert!(snap.progress_ms >= 60_000 && snap.progress_ms < 60_500);
    }

    #[test]
    fn clock_snapshot_carries_source_and_sample() {
        let clock = PlaybackClock::new();
        let pb = Playback {
            item: Some(track("track:a", 100_000)),
            is_playing: true,
            progress_ms: 10_000,
            ..Default::default()
        };
        clock.apply_command_result(&pb, 42);
        let snap = clock.snapshot();
        assert_eq!(snap.source, Some(PlaybackStateSource::CommandResult));
        assert_eq!(snap.sampled_at_ms, Some(42));
    }

    fn device(id: &str, volume: Option<u8>) -> Device {
        Device {
            id: Some(id.to_string()),
            name: "spotuify-test".to_string(),
            kind: "Speaker".to_string(),
            is_active: true,
            is_restricted: false,
            volume_percent: volume,
            supports_volume: true,
        }
    }

    #[test]
    fn apply_device_volume_seeds_device_when_absent() {
        let clock = PlaybackClock::new();
        // No device yet (embedded playback before the first Web API poll).
        assert!(clock.snapshot().device.is_none());
        clock.apply_device_volume(60, || Some(device("dev-embedded", Some(60))), 7);
        let snap = clock.snapshot();
        assert_eq!(snap.device.as_ref().and_then(|d| d.volume_percent), Some(60));
        assert_eq!(snap.device.as_ref().and_then(|d| d.id.as_deref()), Some("dev-embedded"));
    }

    #[test]
    fn apply_device_volume_updates_existing_device_in_place() {
        let clock = PlaybackClock::new();
        let pb = Playback {
            item: Some(track("track:a", 100_000)),
            device: Some(device("dev-embedded", Some(40))),
            is_playing: true,
            progress_ms: 1_000,
            ..Default::default()
        };
        clock.apply_command_result(&pb, 1);
        // Seed closure must NOT run when a device already exists — updating
        // in place preserves the richer poll/command device fields.
        clock.apply_device_volume(75, || panic!("seed must not run"), 2);
        let snap = clock.snapshot();
        assert_eq!(snap.device.as_ref().and_then(|d| d.volume_percent), Some(75));
        assert_eq!(snap.device.as_ref().and_then(|d| d.id.as_deref()), Some("dev-embedded"));
    }
}
