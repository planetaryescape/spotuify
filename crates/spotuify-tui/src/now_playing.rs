//! Phase 6 — `NowPlayingView`.
//!
//! Per-frame canonical "what should the player UI show" derivation.
//! Before this, every render site (bottom title, cover, queue fullscreen
//! hero, lyrics line picker) derived state ad-hoc from a mix of
//! `app.playback`, `app.queue.currently_playing`, and `app.last_played`.
//! Result: progress from one track plotted against another's title, cover
//! art lagging behind playback, lyrics anchored to a stale URI.
//!
//! `NowPlayingView` is the single derivation, used by every render call.
//! Daemon-pushed `Playback` (Phase 3) is the source of truth; queue
//! `currently_playing` is treated as advisory metadata and never as
//! progress truth.

use spotuify_core::{Device, MediaItem, Playback, Queue};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackDisplayState {
    /// Active item present, currently playing.
    Playing,
    /// Active item present, paused.
    Paused,
    /// No active item; falling back to `last_played` for the title slot.
    /// Progress reads as 0; cover renders the gradient seed for the URI.
    LastPlayed,
    /// Neither playback nor last_played available — fresh install before
    /// any sync ran.
    Empty,
}

/// Canonical now-playing snapshot for one frame. Borrowed from `App`,
/// so building it is allocation-free. Used by every render call.
#[derive(Debug)]
pub struct NowPlayingView<'a> {
    pub item: Option<&'a MediaItem>,
    pub state: PlaybackDisplayState,
    /// Progress ms tied to `item`. Zero when state is `LastPlayed` or
    /// `Empty` (the user hasn't asked for that track right now).
    pub progress_ms: u64,
    /// Total duration of `item`; zero when unknown / no item.
    pub duration_ms: u64,
    pub is_playing: bool,
    pub device: Option<&'a Device>,
    pub volume_percent: Option<u8>,
    /// URI of the active playback item (None when state is `LastPlayed`
    /// or `Empty`).
    pub active_uri: Option<&'a str>,
    /// Art URL keyed on `active_uri`. Cover renderer uses this to detect
    /// stale image protocols.
    pub art_url: Option<&'a str>,
    /// Queue's `currently_playing` URI — for mismatch logging only.
    /// Render sites do NOT use this for hero progress.
    pub queue_current_uri: Option<&'a str>,
    pub uri_mismatch: bool,
}

impl<'a> NowPlayingView<'a> {
    /// Build the view from app state. Cheap: lifts references; no clones.
    pub fn derive(
        playback: &'a Playback,
        queue: &'a Queue,
        devices: &'a [Device],
        last_played: Option<&'a MediaItem>,
    ) -> Self {
        let (item, state, progress_ms, duration_ms, is_playing) = match (
            playback.item.as_ref(),
            playback.is_playing,
            last_played,
        ) {
            (Some(item), true, _) => (
                Some(item),
                PlaybackDisplayState::Playing,
                playback.progress_ms,
                item.duration_ms,
                true,
            ),
            (Some(item), false, _) => (
                Some(item),
                PlaybackDisplayState::Paused,
                playback.progress_ms,
                item.duration_ms,
                false,
            ),
            (None, _, Some(last)) => (
                Some(last),
                PlaybackDisplayState::LastPlayed,
                0,
                last.duration_ms,
                false,
            ),
            (None, _, None) => (None, PlaybackDisplayState::Empty, 0, 0, false),
        };
        let active_uri = match state {
            PlaybackDisplayState::Playing | PlaybackDisplayState::Paused => {
                item.map(|i| i.uri.as_str())
            }
            _ => None,
        };
        let art_url = item.and_then(|i| i.image_url.as_deref());
        let queue_current_uri = queue
            .currently_playing
            .as_ref()
            .map(|i| i.uri.as_str());
        let uri_mismatch = match (active_uri, queue_current_uri) {
            (Some(a), Some(q)) => a != q,
            _ => false,
        };
        let device = playback.device.as_ref();
        // Volume preference: active device's reported volume first.
        // Fallback: devices cache entry with same id — covers the case
        // where the playback snapshot device lacked `volume_percent` but
        // the separate devices list has it. Never use a different
        // device's volume.
        let volume_percent = device
            .and_then(|d| d.volume_percent)
            .or_else(|| {
                let active_id = device.and_then(|d| d.id.as_deref());
                active_id.and_then(|id| {
                    devices
                        .iter()
                        .find(|d| d.id.as_deref() == Some(id))
                        .and_then(|d| d.volume_percent)
                })
            });
        Self {
            item,
            state,
            progress_ms,
            duration_ms,
            is_playing,
            device,
            volume_percent,
            active_uri,
            art_url,
            queue_current_uri,
            uri_mismatch,
        }
    }

    /// Returns `true` when lyrics fetched for `lyrics_uri` should render
    /// right now. Use at the lyrics-render call site so stale lyrics
    /// never paint against the wrong track.
    pub fn lyrics_match(&self, lyrics_uri: Option<&str>) -> bool {
        match (self.active_uri, lyrics_uri) {
            (Some(a), Some(l)) => a == l,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spotuify_core::{Device, MediaItem, Playback, Queue};

    fn item(uri: &str, duration_ms: u64) -> MediaItem {
        MediaItem {
            uri: uri.to_string(),
            duration_ms,
            ..Default::default()
        }
    }

    fn device(id: &str, volume: Option<u8>) -> Device {
        Device {
            id: Some(id.to_string()),
            name: "Test".to_string(),
            kind: "computer".to_string(),
            is_active: true,
            is_restricted: false,
            volume_percent: volume,
            supports_volume: true,
        }
    }

    #[test]
    fn now_playing_view_uri_mismatch_flag() {
        let playback = Playback {
            item: Some(item("track:a", 100_000)),
            is_playing: true,
            progress_ms: 5_000,
            ..Default::default()
        };
        let queue = Queue {
            currently_playing: Some(item("track:b", 200_000)),
            ..Default::default()
        };
        let v = NowPlayingView::derive(&playback, &queue, &[], None);
        assert!(v.uri_mismatch);
        assert_eq!(v.active_uri, Some("track:a"));
        assert_eq!(v.queue_current_uri, Some("track:b"));
    }

    #[test]
    fn now_playing_view_no_mismatch_when_matching() {
        let playback = Playback {
            item: Some(item("track:a", 100_000)),
            is_playing: true,
            progress_ms: 5_000,
            ..Default::default()
        };
        let queue = Queue {
            currently_playing: Some(item("track:a", 100_000)),
            ..Default::default()
        };
        let v = NowPlayingView::derive(&playback, &queue, &[], None);
        assert!(!v.uri_mismatch);
    }

    #[test]
    fn now_playing_view_falls_back_to_last_played_when_no_playback() {
        let playback = Playback::default();
        let queue = Queue::default();
        let last = item("track:z", 60_000);
        let v = NowPlayingView::derive(&playback, &queue, &[], Some(&last));
        assert_eq!(v.state, PlaybackDisplayState::LastPlayed);
        assert_eq!(v.progress_ms, 0);
        // Last-played is not "active" — lyrics/cover shouldn't lock to it.
        assert_eq!(v.active_uri, None);
    }

    #[test]
    fn now_playing_view_volume_prefers_playback_device() {
        let playback = Playback {
            item: Some(item("track:a", 100_000)),
            is_playing: true,
            device: Some(device("dev1", Some(70))),
            ..Default::default()
        };
        let queue = Queue::default();
        let devs = vec![device("dev1", Some(20))];
        let v = NowPlayingView::derive(&playback, &queue, &devs, None);
        assert_eq!(v.volume_percent, Some(70));
    }

    #[test]
    fn now_playing_view_volume_falls_back_to_devices_for_same_id() {
        let playback = Playback {
            item: Some(item("track:a", 100_000)),
            is_playing: true,
            // playback's device has no volume reported
            device: Some(device("dev1", None)),
            ..Default::default()
        };
        let queue = Queue::default();
        let devs = vec![device("dev1", Some(55))];
        let v = NowPlayingView::derive(&playback, &queue, &devs, None);
        assert_eq!(v.volume_percent, Some(55));
    }

    #[test]
    fn now_playing_view_volume_does_not_borrow_other_device() {
        let playback = Playback {
            item: Some(item("track:a", 100_000)),
            is_playing: true,
            device: Some(device("dev1", None)),
            ..Default::default()
        };
        let queue = Queue::default();
        // Devices cache has a *different* device with a volume — must
        // NOT be used.
        let devs = vec![device("dev2", Some(99))];
        let v = NowPlayingView::derive(&playback, &queue, &devs, None);
        assert_eq!(v.volume_percent, None);
    }

    #[test]
    fn lyrics_match_only_when_active_uri_matches() {
        let playback = Playback {
            item: Some(item("track:a", 100_000)),
            is_playing: true,
            ..Default::default()
        };
        let queue = Queue::default();
        let v = NowPlayingView::derive(&playback, &queue, &[], None);
        assert!(v.lyrics_match(Some("track:a")));
        assert!(!v.lyrics_match(Some("track:b")));
        assert!(!v.lyrics_match(None));
    }

    #[test]
    fn lyrics_match_false_in_last_played_state() {
        let playback = Playback::default();
        let queue = Queue::default();
        let last = item("track:z", 60_000);
        let v = NowPlayingView::derive(&playback, &queue, &[], Some(&last));
        assert!(!v.lyrics_match(Some("track:z")));
    }
}
