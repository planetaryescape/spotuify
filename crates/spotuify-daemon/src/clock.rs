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
//!   PlayerEvent > CommandResult > RemotePoll > Cache > RecentFallback
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

use spotuify_core::{Device, MediaItem, Playback, PlaybackStateSource, RepeatMode};
use spotuify_player::PlayerEvent;

/// Drift threshold (ms) before a `PositionTick` re-seats the clock.
/// Below this is tick jitter — librespot's worker tick is ~400ms,
/// network jitter is sub-second, so 1500ms preserves smooth local
/// extrapolation while catching real corrections (Seeked,
/// PositionCorrection, sink underrun).
pub const POSITION_DRIFT_THRESHOLD_MS: i64 = 1_500;

/// Spotify can return an empty `GET /me/player` body for a few seconds while
/// next/previous is transitioning. Require the no-active-session signal to
/// persist before clearing the daemon-owned clock so transient readbacks do not
/// make playback look stopped, while genuine idle state still wins eventually.
pub const NO_ACTIVE_SESSION_CONFIRM_MS: i64 = 10_000;

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
    repeat: RepeatMode,
    source: PlaybackStateSource,
    first_empty_web_api_poll_ms: Option<i64>,
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
            repeat: RepeatMode::Off,
            source: PlaybackStateSource::RecentFallback,
            first_empty_web_api_poll_ms: None,
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
        st.first_empty_web_api_poll_ms = None;
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
            repeat: st.repeat,
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
                let uri = uri.as_uri();
                if st.item.as_ref().map(|i| i.uri.as_str()) != Some(uri.as_str()) {
                    // URI change: keep the existing MediaItem if it
                    // already matches; otherwise replace with a stub
                    // so the next snapshot at least carries the URI.
                    if st.item.as_ref().map(|i| i.uri.as_str()) != Some(uri.as_str()) {
                        st.item = Some(MediaItem {
                            uri,
                            ..Default::default()
                        });
                    }
                }
                st.base_progress_ms = *position_ms as u64;
                st.base_instant = Instant::now();
                st.is_playing = true;
                st.source = PlaybackStateSource::PlayerEvent;
                st.sampled_at_ms = now_ms;
                st.first_empty_web_api_poll_ms = None;
            }
            PlayerEvent::PlaybackPaused => {
                // Freeze at the current extrapolated progress.
                let frozen = st.current_progress_ms(Instant::now());
                st.base_progress_ms = frozen;
                st.base_instant = Instant::now();
                st.is_playing = false;
                st.source = PlaybackStateSource::PlayerEvent;
                st.sampled_at_ms = now_ms;
                st.first_empty_web_api_poll_ms = None;
            }
            PlayerEvent::PlaybackResumed => {
                st.base_instant = Instant::now();
                st.is_playing = true;
                st.source = PlaybackStateSource::PlayerEvent;
                st.sampled_at_ms = now_ms;
                st.first_empty_web_api_poll_ms = None;
            }
            PlayerEvent::PositionTick { position_ms } => {
                st.first_empty_web_api_poll_ms = None;
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
                st.first_empty_web_api_poll_ms = None;
            }
            _ => {
                // Ready/Degraded/ProviderPolicy/SessionDisconnected/Failed/
                // PreloadNext/VolumeChanged don't move the playback clock;
                // volume is handled by `apply_device_volume`.
            }
        }
    }

    /// Audio-flow watchdog reconciliation: the sink stopped emitting PCM while
    /// the clock still reports playing (silent zombie player). Freeze progress
    /// at the current extrapolation and flip `is_playing=false` so `snapshot()`
    /// stops lying. Keeps `item`/`device` (the track is still "loaded", just
    /// not audible). Marks the source as `PlayerEvent` so a lagging lower-trust
    /// Web-API poll can't immediately re-assert playing; a genuine resume
    /// arrives as a real `PlaybackStarted`/`PlaybackResumed` and legitimately
    /// overrides this. Returns `true` if it changed state (was playing).
    pub fn mark_audio_stalled(&self, now_ms: i64) -> bool {
        let mut st = self.inner.write();
        if !st.is_playing {
            return false;
        }
        let frozen = st.current_progress_ms(Instant::now());
        st.base_progress_ms = frozen;
        st.base_instant = Instant::now();
        st.is_playing = false;
        st.source = PlaybackStateSource::PlayerEvent;
        st.sampled_at_ms = now_ms;
        st.first_empty_web_api_poll_ms = None;
        true
    }

    /// Fill display metadata for the currently-playing URI without changing
    /// the transport source, progress, or play/pause state.
    pub fn enrich_current_item(&self, item: &MediaItem) -> bool {
        let mut st = self.inner.write();
        let Some(current) = st.item.as_mut() else {
            return false;
        };
        if current.uri != item.uri {
            return false;
        }
        let changed = merge_missing_item_metadata(current, item);
        if changed {
            st.first_empty_web_api_poll_ms = None;
        }
        changed
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
        st.first_empty_web_api_poll_ms = None;
    }

    /// Apply a `CommandResult.playback` returned by `actions::execute`.
    /// Authoritative — overwrites cache/Web-API state. Captured-seq
    /// guard is the caller's responsibility (handler enforces it via
    /// `may_apply_state_update` before calling us).
    pub fn apply_command_result(&self, playback: &Playback, sampled_at_ms: i64) {
        let mut st = self.inner.write();
        let same_uri = matches_uri(&st.item, &playback.item);
        // CommandResult shouldn't outrank a PlayerEvent for the same
        // URI: the event is the audio pipeline's truth and the command
        // readback is a Web-API round-trip that can lag it by over a
        // second. When the event already owns the clock for this track,
        // keep its play-state and progress anchor — overwriting them
        // yanked the progress bar backwards on every transport command
        // until a PositionTick re-anchored. Item/device/shuffle/repeat
        // are still adopted (commands legitimately change those).
        let player_event_owns_track = same_uri && st.source == PlaybackStateSource::PlayerEvent;
        st.item = playback.item.clone();
        st.device = playback.device.clone();
        st.shuffle = playback.shuffle;
        st.repeat = playback.repeat;
        st.first_empty_web_api_poll_ms = None;
        if !player_event_owns_track {
            st.is_playing = playback.is_playing;
            st.base_progress_ms = playback.progress_ms;
            st.base_instant = Instant::now();
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
    /// Returns `true` when the sample changed the stored state.
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
        if !playback_has_live_signal(playback) {
            return apply_empty_web_api_poll(&mut st, sampled_at_ms, provider_timestamp_ms);
        }
        st.first_empty_web_api_poll_ms = None;
        let same_uri = matches_uri(&st.item, &playback.item);
        if same_uri {
            // Same track: only re-seat if our current source is weaker
            // OR significantly older.
            let beats_priority =
                source_priority(PlaybackStateSource::RemotePoll) > source_priority(st.source);
            let older_than_threshold =
                (sampled_at_ms - st.sampled_at_ms) > POSITION_DRIFT_THRESHOLD_MS;
            if !(beats_priority || older_than_threshold) {
                // Update metadata/freshness only — keep extrapolation.
                let metadata_changed = if let Some(item) = playback.item.as_ref() {
                    merge_missing_current_item_metadata(&mut st.item, item)
                } else {
                    false
                };
                st.provider_timestamp_ms = provider_timestamp_ms;
                return metadata_changed;
            }
        }
        st.item = playback.item.clone();
        st.device = playback.device.clone();
        st.is_playing = playback.is_playing;
        st.base_progress_ms = playback.progress_ms;
        st.base_instant = Instant::now();
        st.shuffle = playback.shuffle;
        st.repeat = playback.repeat;
        st.provider_timestamp_ms = provider_timestamp_ms;
        st.source = PlaybackStateSource::RemotePoll;
        st.sampled_at_ms = sampled_at_ms;
        st.first_empty_web_api_poll_ms = None;
        true
    }

    /// Build the durable candidate for a Web API poll without changing the
    /// user-visible clock. Empty responses advance only the transient-empty
    /// confirmation marker; the active snapshot is returned for persistence
    /// only once the clear is confirmed. Callers persist this candidate first,
    /// then call [`Self::apply_web_api_poll`] with the original sample.
    pub fn prepare_web_api_poll(
        &self,
        playback: &Playback,
        sampled_at_ms: i64,
        provider_timestamp_ms: Option<i64>,
    ) -> Option<Playback> {
        if playback_has_live_signal(playback) {
            return Some(playback.clone());
        }
        let mut st = self.inner.write();
        if !clock_state_has_live_signal(&st) {
            st.first_empty_web_api_poll_ms = None;
            return None;
        }
        let first_empty_ms = match st.first_empty_web_api_poll_ms {
            Some(first_empty_ms) => first_empty_ms,
            None => {
                st.first_empty_web_api_poll_ms = Some(sampled_at_ms);
                return None;
            }
        };
        if sampled_at_ms.saturating_sub(first_empty_ms) < NO_ACTIVE_SESSION_CONFIRM_MS {
            return None;
        }
        let frozen = st.current_progress_ms(Instant::now());
        Some(Playback {
            item: st.item.clone(),
            device: None,
            is_playing: false,
            progress_ms: if st.item.is_some() { frozen } else { 0 },
            shuffle: st.shuffle,
            repeat: st.repeat,
            sampled_at_ms: Some(sampled_at_ms),
            provider_timestamp_ms,
            source: Some(if st.item.is_some() {
                PlaybackStateSource::RecentFallback
            } else {
                PlaybackStateSource::RemotePoll
            }),
        })
    }

    /// Apply a user-issued absolute seek. Pretty much an event itself
    /// — bumps progress and treats the change as authoritative.
    pub fn apply_seek(&self, position_ms: u64, sampled_at_ms: i64) {
        let mut st = self.inner.write();
        st.base_progress_ms = st.clamp_to_duration(position_ms);
        st.base_instant = Instant::now();
        st.source = PlaybackStateSource::CommandResult;
        st.sampled_at_ms = sampled_at_ms;
        st.first_empty_web_api_poll_ms = None;
    }
}

fn apply_empty_web_api_poll(
    st: &mut ClockState,
    sampled_at_ms: i64,
    provider_timestamp_ms: Option<i64>,
) -> bool {
    if !clock_state_has_live_signal(st) {
        st.first_empty_web_api_poll_ms = None;
        st.provider_timestamp_ms = provider_timestamp_ms;
        return false;
    }

    let first_empty_ms = match st.first_empty_web_api_poll_ms {
        Some(first_empty_ms) => first_empty_ms,
        None => {
            st.first_empty_web_api_poll_ms = Some(sampled_at_ms);
            tracing::debug!(
                target: "spotuify_daemon::clock",
                sampled_at_ms,
                "ignoring first empty Web API playback poll"
            );
            return false;
        }
    };

    if sampled_at_ms.saturating_sub(first_empty_ms) < NO_ACTIVE_SESSION_CONFIRM_MS {
        tracing::debug!(
            target: "spotuify_daemon::clock",
            sampled_at_ms,
            first_empty_ms,
            "ignoring unconfirmed empty Web API playback poll"
        );
        return false;
    }

    let frozen = st.current_progress_ms(Instant::now());
    let changed = st.is_playing
        || st.device.is_some()
        || (st.item.is_some() && st.source != PlaybackStateSource::RecentFallback);
    if st.item.is_some() {
        st.device = None;
        st.is_playing = false;
        st.base_progress_ms = frozen;
    } else {
        st.device = None;
        st.is_playing = false;
        st.base_progress_ms = 0;
    }
    st.base_instant = Instant::now();
    if st.item.is_none() {
        st.shuffle = false;
        st.repeat = RepeatMode::Off;
    }
    st.provider_timestamp_ms = provider_timestamp_ms;
    st.source = if st.item.is_some() {
        PlaybackStateSource::RecentFallback
    } else {
        PlaybackStateSource::RemotePoll
    };
    st.sampled_at_ms = sampled_at_ms;
    st.first_empty_web_api_poll_ms = None;
    changed
}

fn clock_state_has_live_signal(st: &ClockState) -> bool {
    st.is_playing
        || st.device.is_some()
        || (st.item.is_some() && st.source != PlaybackStateSource::RecentFallback)
}

fn playback_has_live_signal(playback: &Playback) -> bool {
    playback.item.is_some() || playback.device.is_some() || playback.is_playing
}

fn matches_uri(a: &Option<MediaItem>, b: &Option<MediaItem>) -> bool {
    match (a.as_ref(), b.as_ref()) {
        (Some(a), Some(b)) => a.uri == b.uri,
        (None, None) => true,
        _ => false,
    }
}

fn merge_missing_item_metadata(target: &mut MediaItem, source: &MediaItem) -> bool {
    if target.uri != source.uri {
        return false;
    }
    let mut changed = false;

    if target.id.is_none() && source.id.is_some() {
        target.id = source.id.clone();
        changed = true;
    }
    if target.name.is_empty() && !source.name.is_empty() {
        target.name = source.name.clone();
        changed = true;
    }
    if target.subtitle.is_empty() && !source.subtitle.is_empty() {
        target.subtitle = source.subtitle.clone();
        changed = true;
    }
    if target.context.is_empty() && !source.context.is_empty() {
        target.context = source.context.clone();
        changed = true;
    }
    if target.duration_ms == 0 && source.duration_ms > 0 {
        target.duration_ms = source.duration_ms;
        changed = true;
    }
    if target.image_url.is_none() && source.image_url.is_some() {
        target.image_url = source.image_url.clone();
        changed = true;
    }
    if target.source.is_none() && source.source.is_some() {
        target.source = source.source.clone();
        changed = true;
    }
    if target.freshness.is_none() && source.freshness.is_some() {
        target.freshness = source.freshness.clone();
        changed = true;
    }
    if target.explicit.is_none() && source.explicit.is_some() {
        target.explicit = source.explicit;
        changed = true;
    }
    if target.is_playable.is_none() && source.is_playable.is_some() {
        target.is_playable = source.is_playable;
        changed = true;
    }
    if target.album.is_none() && source.album.is_some() {
        target.album = source.album.clone();
        changed = true;
    }
    if target.added_at_ms.is_none() && source.added_at_ms.is_some() {
        target.added_at_ms = source.added_at_ms;
        changed = true;
    }
    if target.resume_position_ms.is_none() && source.resume_position_ms.is_some() {
        target.resume_position_ms = source.resume_position_ms;
        changed = true;
    }
    if target.fully_played.is_none() && source.fully_played.is_some() {
        target.fully_played = source.fully_played;
        changed = true;
    }
    if target.release_date.is_none() && source.release_date.is_some() {
        target.release_date = source.release_date;
        changed = true;
    }
    if target.album_group.is_none() && source.album_group.is_some() {
        target.album_group = source.album_group.clone();
        changed = true;
    }
    if target.in_library.is_none() && source.in_library.is_some() {
        target.in_library = source.in_library;
        changed = true;
    }
    if target.album_uri.is_none() && source.album_uri.is_some() {
        target.album_uri = source.album_uri.clone();
        changed = true;
    }
    if target.artists.is_empty() && !source.artists.is_empty() {
        target.artists = source.artists.clone();
        changed = true;
    }
    if target.genre.is_none() && source.genre.is_some() {
        target.genre = source.genre.clone();
        changed = true;
    }

    changed
}

fn merge_missing_current_item_metadata(target: &mut Option<MediaItem>, source: &MediaItem) -> bool {
    let Some(target) = target.as_mut() else {
        return false;
    };
    merge_missing_item_metadata(target, source)
}

fn source_priority(source: PlaybackStateSource) -> u8 {
    match source {
        PlaybackStateSource::PlayerEvent => 4,
        PlaybackStateSource::CommandResult => 3,
        PlaybackStateSource::RemotePoll => 2,
        PlaybackStateSource::Cache => 1,
        PlaybackStateSource::RecentFallback => 0,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use spotuify_core::{MediaItem, ResourceUri};
    use spotuify_player::PlayerEvent;

    fn track(uri: &str, duration_ms: u64) -> MediaItem {
        MediaItem {
            uri: uri.to_string(),
            duration_ms,
            ..Default::default()
        }
    }

    fn named_track(uri: &str) -> MediaItem {
        MediaItem {
            uri: uri.to_string(),
            name: "Known Track".to_string(),
            subtitle: "Known Artist".to_string(),
            context: "Known Album".to_string(),
            duration_ms: 123_000,
            image_url: Some("https://i.scdn.co/image/test".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn clock_advances_while_playing() {
        let clock = PlaybackClock::new();
        clock.apply_player_event(
            &PlayerEvent::PlaybackStarted {
                uri: ResourceUri::parse("spotify:track:a").unwrap(),
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
                uri: ResourceUri::parse("spotify:track:a").unwrap(),
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
                uri: ResourceUri::parse("spotify:track:a").unwrap(),
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
                uri: ResourceUri::parse("spotify:track:a").unwrap(),
                position_ms: 0,
            },
            1,
        );
        clock.apply_player_event(&PlayerEvent::PlaybackPaused, 2);
        let frozen = clock.snapshot().progress_ms;
        // Make the paused interval much larger than scheduling tolerance so a
        // stale pre-pause anchor cannot accidentally satisfy the assertion.
        std::thread::sleep(std::time::Duration::from_millis(200));
        clock.apply_player_event(&PlayerEvent::PlaybackResumed, 3);
        let resumed_at = std::time::Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(40));
        let after = clock.snapshot().progress_ms;
        let resumed_elapsed_ms = resumed_at.elapsed().as_millis() as u64;
        let advanced = after.saturating_sub(frozen);
        assert!(
            advanced >= 30 && advanced <= resumed_elapsed_ms.saturating_add(50),
            "expected resumed progress near {resumed_elapsed_ms}ms, advanced {advanced}ms"
        );
    }

    #[test]
    fn clock_track_change_resets_progress() {
        let clock = PlaybackClock::new();
        clock.apply_player_event(
            &PlayerEvent::PlaybackStarted {
                uri: ResourceUri::parse("spotify:track:a").unwrap(),
                position_ms: 30_000,
            },
            1,
        );
        clock.apply_player_event(
            &PlayerEvent::TrackChanged {
                uri: ResourceUri::parse("spotify:track:b").unwrap(),
                position_ms: 0,
            },
            2,
        );
        let snap = clock.snapshot();
        assert_eq!(
            snap.item.as_ref().map(|i| i.uri.as_str()),
            Some("spotify:track:b")
        );
        assert!(snap.progress_ms < 200);
    }

    #[test]
    fn clock_enriches_player_event_stub_for_same_uri() {
        let clock = PlaybackClock::new();
        clock.apply_player_event(
            &PlayerEvent::PlaybackStarted {
                uri: ResourceUri::parse("spotify:track:a").unwrap(),
                position_ms: 10_000,
            },
            1,
        );

        assert!(clock.enrich_current_item(&named_track("spotify:track:a")));
        let snap = clock.snapshot();
        let item = snap.item.expect("current item");
        assert_eq!(item.uri, "spotify:track:a");
        assert_eq!(item.name, "Known Track");
        assert_eq!(item.subtitle, "Known Artist");
        assert_eq!(item.context, "Known Album");
        assert_eq!(item.duration_ms, 123_000);
        assert_eq!(
            item.image_url.as_deref(),
            Some("https://i.scdn.co/image/test")
        );
        assert_eq!(snap.source, Some(PlaybackStateSource::PlayerEvent));
        assert!(snap.is_playing);
    }

    #[test]
    fn clock_enrichment_ignores_different_uri() {
        let clock = PlaybackClock::new();
        clock.apply_player_event(
            &PlayerEvent::PlaybackStarted {
                uri: ResourceUri::parse("spotify:track:a").unwrap(),
                position_ms: 10_000,
            },
            1,
        );

        assert!(!clock.enrich_current_item(&named_track("spotify:track:b")));
        let snap = clock.snapshot();
        let item = snap.item.expect("current item");
        assert_eq!(item.uri, "spotify:track:a");
        assert!(item.name.is_empty());
    }

    #[test]
    fn clock_position_tick_below_threshold_does_not_rebase() {
        let clock = PlaybackClock::new();
        clock.apply_player_event(
            &PlayerEvent::PlaybackStarted {
                uri: ResourceUri::parse("spotify:track:a").unwrap(),
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
                uri: ResourceUri::parse("spotify:track:a").unwrap(),
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
            item: Some(track("spotify:track:a", 200_000)),
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
    fn clock_web_api_poll_ignores_transient_empty_snapshot() {
        let clock = PlaybackClock::new();
        let playing = Playback {
            item: Some(track("spotify:track:a", 200_000)),
            is_playing: true,
            progress_ms: 50_000,
            ..Default::default()
        };
        clock.apply_command_result(&playing, 1);

        let empty = Playback::default();
        let applied = clock.apply_web_api_poll(&empty, 1, 1, 5_000, None);

        assert!(
            !applied,
            "first empty Spotify poll must be treated as transient"
        );
        let snap = clock.snapshot();
        assert_eq!(
            snap.item.as_ref().map(|item| item.uri.as_str()),
            Some("spotify:track:a")
        );
        assert!(
            snap.is_playing,
            "transient empty poll must not stop playback"
        );
    }

    #[test]
    fn clock_web_api_poll_remembers_item_after_confirmed_no_active_session() {
        let clock = PlaybackClock::new();
        let playing = Playback {
            item: Some(track("spotify:track:a", 200_000)),
            is_playing: true,
            progress_ms: 50_000,
            ..Default::default()
        };
        clock.apply_command_result(&playing, 1);

        let empty = Playback::default();
        assert!(!clock.apply_web_api_poll(&empty, 1, 1, 5_000, None));
        let applied =
            clock.apply_web_api_poll(&empty, 1, 1, 5_000 + NO_ACTIVE_SESSION_CONFIRM_MS, None);

        assert!(applied, "confirmed no-active-session should stop playback");
        let snap = clock.snapshot();
        assert_eq!(
            snap.item.as_ref().map(|item| item.uri.as_str()),
            Some("spotify:track:a")
        );
        assert!(snap.device.is_none());
        assert!(!snap.is_playing);
        assert!(snap.progress_ms >= 50_000);
        assert_eq!(snap.source, Some(PlaybackStateSource::RecentFallback));

        assert!(!clock.apply_web_api_poll(
            &empty,
            1,
            1,
            5_000 + (NO_ACTIVE_SESSION_CONFIRM_MS * 2),
            None
        ));
        let snap = clock.snapshot();
        assert_eq!(
            snap.item.as_ref().map(|item| item.uri.as_str()),
            Some("spotify:track:a")
        );
        assert_eq!(snap.source, Some(PlaybackStateSource::RecentFallback));
    }

    #[test]
    fn clock_web_api_poll_clobbered_by_player_event_priority() {
        let clock = PlaybackClock::new();
        // PlayerEvent at sampled_at=10
        clock.apply_player_event(
            &PlayerEvent::PlaybackStarted {
                uri: ResourceUri::parse("spotify:track:a").unwrap(),
                position_ms: 1_000,
            },
            10,
        );
        // Web API poll, same URI, slightly older sample → rejected.
        let pb = Playback {
            item: Some(track("spotify:track:a", 200_000)),
            is_playing: true,
            progress_ms: 5_000,
            ..Default::default()
        };
        let applied = clock.apply_web_api_poll(&pb, 1, 1, 9, None);
        assert!(
            applied,
            "same-URI poll should report metadata enrichment as a clock change"
        );
        let snap = clock.snapshot();
        assert!(snap.progress_ms < 3_000, "player event should still win");
        assert_eq!(
            snap.item.as_ref().map(|item| item.duration_ms),
            Some(200_000),
            "same-URI poll may fill metadata without rebasing progress"
        );
    }

    #[test]
    fn clock_uri_change_always_rebases_even_from_web_api() {
        let clock = PlaybackClock::new();
        clock.apply_player_event(
            &PlayerEvent::PlaybackStarted {
                uri: ResourceUri::parse("spotify:track:a").unwrap(),
                position_ms: 50_000,
            },
            1,
        );
        let pb = Playback {
            item: Some(track("spotify:track:b", 200_000)),
            is_playing: true,
            progress_ms: 0,
            ..Default::default()
        };
        let applied = clock.apply_web_api_poll(&pb, 7, 7, 2, None);
        assert!(applied);
        assert_eq!(
            clock.snapshot().item.as_ref().map(|i| i.uri.as_str()),
            Some("spotify:track:b")
        );
    }

    #[test]
    fn clock_clamps_to_track_duration() {
        let clock = PlaybackClock::new();
        let pb = Playback {
            item: Some(track("spotify:track:a", 1_000)),
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
                uri: ResourceUri::parse("spotify:track:a").unwrap(),
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
            item: Some(track("spotify:track:a", 100_000)),
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
        assert_eq!(
            snap.device.as_ref().and_then(|d| d.volume_percent),
            Some(60)
        );
        assert_eq!(
            snap.device.as_ref().and_then(|d| d.id.as_deref()),
            Some("dev-embedded")
        );
    }

    #[test]
    fn apply_device_volume_updates_existing_device_in_place() {
        let clock = PlaybackClock::new();
        let pb = Playback {
            item: Some(track("spotify:track:a", 100_000)),
            device: Some(device("dev-embedded", Some(40))),
            is_playing: true,
            progress_ms: 1_000,
            ..Default::default()
        };
        clock.apply_command_result(&pb, 1);
        // Seed closure must NOT run when a device already exists — updating
        // in place preserves the richer poll/command device fields.
        let mut seed_called = false;
        clock.apply_device_volume(
            75,
            || {
                seed_called = true;
                None
            },
            2,
        );
        assert!(!seed_called, "seed must not run");
        let snap = clock.snapshot();
        assert_eq!(
            snap.device.as_ref().and_then(|d| d.volume_percent),
            Some(75)
        );
        assert_eq!(
            snap.device.as_ref().and_then(|d| d.id.as_deref()),
            Some("dev-embedded")
        );
    }

    #[test]
    fn mark_audio_stalled_freezes_and_stops_when_playing() {
        let clock = PlaybackClock::new();
        clock.apply_player_event(
            &PlayerEvent::PlaybackStarted {
                uri: ResourceUri::parse("spotify:track:a").unwrap(),
                position_ms: 5_000,
            },
            1,
        );
        assert!(clock.snapshot().is_playing);
        assert!(clock.mark_audio_stalled(2), "should report a state change");
        let snap = clock.snapshot();
        assert!(!snap.is_playing, "stall flips is_playing false");
        assert!(snap.item.is_some(), "track stays loaded, just not audible");
    }

    #[test]
    fn mark_audio_stalled_noop_when_not_playing() {
        let clock = PlaybackClock::new();
        clock.apply_player_event(
            &PlayerEvent::PlaybackStarted {
                uri: ResourceUri::parse("spotify:track:a").unwrap(),
                position_ms: 5_000,
            },
            1,
        );
        clock.apply_player_event(&PlayerEvent::PlaybackPaused, 2);
        assert!(!clock.snapshot().is_playing);
        assert!(
            !clock.mark_audio_stalled(3),
            "must not fight a legitimate pause"
        );
    }

    #[test]
    fn mark_audio_stalled_overridden_by_later_player_event() {
        let clock = PlaybackClock::new();
        clock.apply_player_event(
            &PlayerEvent::PlaybackStarted {
                uri: ResourceUri::parse("spotify:track:a").unwrap(),
                position_ms: 5_000,
            },
            1,
        );
        clock.mark_audio_stalled(2);
        assert!(!clock.snapshot().is_playing);
        // A genuine resume (same trust tier, newer) re-asserts playing.
        clock.apply_player_event(&PlayerEvent::PlaybackResumed, 3);
        assert!(
            clock.snapshot().is_playing,
            "a real resume overrides the watchdog stall"
        );
    }
}
