//! Phase 6 — `NowPlayingView`.
//!
//! Per-frame canonical "what should the player UI show" derivation.
//! Before this, every render site (bottom title, cover, queue fullscreen
//! hero, lyrics line picker) derived state ad-hoc from a mix of
//! `app.playback`, `app.queue.currently_playing`, and historical fallback
//! state.
//! Result: progress from one track plotted against another's title, cover
//! art lagging behind playback, lyrics anchored to a stale URI.
//!
//! `NowPlayingView` is the single derivation, used by every render call.
//! Daemon-pushed `Playback` (Phase 3) is the source of truth; queue
//! `currently_playing` is treated as advisory metadata and never as
//! progress truth.

use spotuify_core::{Device, MediaItem, Playback, PlaybackStateSource, Queue};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackDisplayState {
    /// Active item present, currently playing.
    Playing,
    /// Active item present, paused.
    Paused,
    /// No live playback item available.
    Empty,
}

/// Canonical now-playing snapshot for one frame. Borrowed from `App`,
/// so building it is allocation-free. Used by every render call.
#[derive(Debug)]
pub struct NowPlayingView<'a> {
    pub item: Option<&'a MediaItem>,
    pub state: PlaybackDisplayState,
    /// Progress ms tied to `item`. Zero when state is not live playback.
    pub progress_ms: u64,
    /// Total duration of `item`; zero when unknown / no item.
    pub duration_ms: u64,
    pub is_playing: bool,
    pub device: Option<&'a Device>,
    pub volume_percent: Option<u8>,
    /// URI of the active playback item.
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
    pub fn derive(playback: &'a Playback, queue: &'a Queue, devices: &'a [Device]) -> Self {
        // Show the playback item whenever the daemon has one — including a
        // cached / recent-fallback "last played" track — so the player
        // remembers where you left off when the Spotify session goes idle
        // (matching the macOS client, which never blanks the last track).
        // Only a genuinely live session reads as Playing; a paused, cached, or
        // recent-fallback snapshot shows the track frozen/paused so progress
        // never ticks against a stale source.
        let is_live = playback_item_is_live(playback);
        let (item, state, progress_ms, duration_ms, is_playing) = match playback.item.as_ref() {
            Some(item) => {
                let playing = is_live && playback.is_playing;
                let state = if playing {
                    PlaybackDisplayState::Playing
                } else {
                    PlaybackDisplayState::Paused
                };
                (
                    Some(item),
                    state,
                    playback.progress_ms,
                    item.duration_ms,
                    playing,
                )
            }
            None => (None, PlaybackDisplayState::Empty, 0, 0, false),
        };
        let active_uri = match state {
            PlaybackDisplayState::Playing | PlaybackDisplayState::Paused => {
                item.map(|i| i.uri.as_str())
            }
            PlaybackDisplayState::Empty => None,
        };
        let art_url = item.and_then(|i| i.image_url.as_deref());
        let queue_current_uri = queue.currently_playing.as_ref().map(|i| i.uri.as_str());
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
        let volume_percent = device.and_then(|d| d.volume_percent).or_else(|| {
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

fn playback_item_is_live(playback: &Playback) -> bool {
    playback.item.is_some()
        && !matches!(
            playback.source,
            Some(PlaybackStateSource::Cache | PlaybackStateSource::RecentFallback)
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use spotuify_core::{Device, MediaItem, Playback, PlaybackStateSource, Queue};

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
        let v = NowPlayingView::derive(&playback, &queue, &[]);
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
        let v = NowPlayingView::derive(&playback, &queue, &[]);
        assert!(!v.uri_mismatch);
    }

    #[test]
    fn now_playing_view_hides_last_played_when_no_playback() {
        let playback = Playback::default();
        let queue = Queue::default();
        let v = NowPlayingView::derive(&playback, &queue, &[]);
        assert_eq!(v.state, PlaybackDisplayState::Empty);
        assert_eq!(v.item, None);
        assert_eq!(v.progress_ms, 0);
        assert_eq!(v.active_uri, None);
    }

    #[test]
    fn now_playing_view_shows_cached_playback_item_paused() {
        // A cached "last played" snapshot keeps the track on screen (paused),
        // so the player remembers where you left off instead of blanking.
        let playback = Playback {
            item: Some(item("track:cached", 100_000)),
            progress_ms: 42_000,
            source: Some(PlaybackStateSource::Cache),
            ..Default::default()
        };
        let queue = Queue::default();
        let v = NowPlayingView::derive(&playback, &queue, &[]);
        assert_eq!(v.state, PlaybackDisplayState::Paused);
        assert_eq!(v.item.map(|i| i.uri.as_str()), Some("track:cached"));
        assert_eq!(v.active_uri, Some("track:cached"));
        assert_eq!(v.progress_ms, 42_000);
        assert!(!v.is_playing);
    }

    #[test]
    fn now_playing_view_shows_recent_fallback_playback_item_paused() {
        let playback = Playback {
            item: Some(item("track:recent", 100_000)),
            progress_ms: 5_000,
            source: Some(PlaybackStateSource::RecentFallback),
            ..Default::default()
        };
        let queue = Queue::default();
        let v = NowPlayingView::derive(&playback, &queue, &[]);
        assert_eq!(v.state, PlaybackDisplayState::Paused);
        assert_eq!(v.item.map(|i| i.uri.as_str()), Some("track:recent"));
        assert_eq!(v.active_uri, Some("track:recent"));
        assert!(!v.is_playing);
    }

    #[test]
    fn now_playing_view_recent_fallback_never_reads_as_playing() {
        // Even if a stale cached snapshot claims is_playing, a non-live source
        // must render Paused so progress never ticks against it.
        let playback = Playback {
            item: Some(item("track:stale", 100_000)),
            is_playing: true,
            source: Some(PlaybackStateSource::RecentFallback),
            ..Default::default()
        };
        let queue = Queue::default();
        let v = NowPlayingView::derive(&playback, &queue, &[]);
        assert_eq!(v.state, PlaybackDisplayState::Paused);
        assert!(!v.is_playing);
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
        let v = NowPlayingView::derive(&playback, &queue, &devs);
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
        let v = NowPlayingView::derive(&playback, &queue, &devs);
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
        let v = NowPlayingView::derive(&playback, &queue, &devs);
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
        let v = NowPlayingView::derive(&playback, &queue, &[]);
        assert!(v.lyrics_match(Some("track:a")));
        assert!(!v.lyrics_match(Some("track:b")));
        assert!(!v.lyrics_match(None));
    }

    #[test]
    fn lyrics_match_false_without_live_playback() {
        let playback = Playback::default();
        let queue = Queue::default();
        let v = NowPlayingView::derive(&playback, &queue, &[]);
        assert!(!v.lyrics_match(Some("track:z")));
    }
}
