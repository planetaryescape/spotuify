//! Merging a freshly fetched `/me/player/queue` snapshot with the
//! previously cached queue.
//!
//! Spotify caps the queue endpoint at ~20 upcoming items, and embedded
//! librespot sessions frequently answer with `currently_playing` and
//! ZERO items. When a context (album/playlist) is playing, the cached
//! queue — seeded in full by the `play-context` snapshot — knows the
//! long tail. Every queue write path (the daemon's refresh apply AND
//! the background sync loop) must run the fetched snapshot through
//! [`reattach_cached_queue_tail`] before persisting, or the truncated
//! fetch wipes the tail the user sees. That split (daemon merged, sync
//! loop didn't) was a live bug: the queue rail collapsed from a full
//! playlist to ≤20 items within one 15s sync cadence.

use crate::{MediaItem, Queue};

/// How fresh a cached snapshot must be for the "don't clobber a
/// bigger cache on an anchor miss" tiebreak to apply.
const ANCHOR_MISS_PRESERVE_WINDOW_MS: i64 = 10_000;

/// Re-attach the cached queue's tail to a freshly fetched (and
/// already pending-append-overlaid) queue snapshot.
///
/// `anchor_uri` must come from the RAW fetch (its last upstream
/// upcoming item, falling back to its `currently_playing`), not from
/// the overlaid queue — optimistic appends are ours, not Spotify's.
///
/// Behavior:
/// - Under shuffle the context order no longer predicts playback, so
///   the fetch is returned untouched.
/// - If the anchor is the cached `currently_playing` or appears in the
///   cached upcoming list, everything after it is the tail the API
///   truncated — appended, skipping URIs already present (the queue is
///   a set; never duplicate).
/// - Anchor miss (e.g. a wrong `Next` prediction was cached, then
///   Spotify advanced to an unexpected track): if the fetch is
///   effectively empty while the cache is fresh
///   (< [`ANCHOR_MISS_PRESERVE_WINDOW_MS`]) and strictly richer, keep
///   the cached upcoming list under the fetched `currently_playing`
///   instead of wiping the queue with `X + []`. A genuinely new
///   context replaces the cache on the next non-empty fetch.
pub fn reattach_cached_queue_tail(
    mut queue: Queue,
    anchor_uri: Option<&str>,
    cached: &Queue,
    shuffle: bool,
    now_ms: i64,
) -> Queue {
    if shuffle {
        return queue;
    }
    let Some(anchor) = anchor_uri else {
        return queue;
    };

    let tail_start = if cached
        .currently_playing
        .as_ref()
        .is_some_and(|item| item.uri == anchor)
    {
        Some(0)
    } else {
        cached
            .items
            .iter()
            .position(|item| item.uri == anchor)
            .map(|pos| pos + 1)
    };

    let Some(tail_start) = tail_start else {
        // Anchor miss. Preserve a fresh, richer cache instead of
        // letting an empty fetch wipe it (wrong-prediction recovery).
        let cache_is_fresh =
            now_ms.saturating_sub(cached.as_of_ms) < ANCHOR_MISS_PRESERVE_WINDOW_MS;
        if queue.items.is_empty() && cache_is_fresh && !cached.items.is_empty() {
            let current_uri = queue
                .currently_playing
                .as_ref()
                .map(|item| item.uri.clone());
            queue.items = cached
                .items
                .iter()
                .filter(|item| Some(&item.uri) != current_uri.as_ref())
                .cloned()
                .collect();
            queue.session_active = true;
            queue.as_of_ms = now_ms;
        }
        return queue;
    };

    let existing: std::collections::HashSet<&str> =
        queue.items.iter().map(|item| item.uri.as_str()).collect();
    let tail: Vec<MediaItem> = cached.items[tail_start..]
        .iter()
        .filter(|item| !existing.contains(item.uri.as_str()))
        .cloned()
        .collect();
    queue.items.extend(tail);
    queue.as_of_ms = now_ms;
    queue
}

/// The anchor for [`reattach_cached_queue_tail`], computed from the
/// RAW fetched queue before any optimistic overlay: the last upstream
/// upcoming item, or the playing track when the upstream list is empty
/// (the common embedded-librespot shape).
pub fn queue_tail_anchor(raw_fetched: &Queue) -> Option<String> {
    raw_fetched
        .items
        .last()
        .map(|item| item.uri.clone())
        .or_else(|| {
            raw_fetched
                .currently_playing
                .as_ref()
                .map(|item| item.uri.clone())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MediaKind;

    fn item(uri: &str) -> MediaItem {
        MediaItem {
            uri: uri.to_string(),
            name: uri.to_string(),
            kind: MediaKind::Track,
            ..Default::default()
        }
    }

    fn queue(current: Option<&str>, items: &[&str], as_of_ms: i64) -> Queue {
        Queue {
            currently_playing: current.map(item),
            items: items.iter().map(|uri| item(uri)).collect(),
            session_active: true,
            as_of_ms,
        }
    }

    #[test]
    fn truncated_fetch_reattaches_tail_from_anchor_item() {
        let fetched = queue(Some("u:cur"), &["u:n1", "u:n2"], 0);
        let cached = queue(Some("u:cur"), &["u:n1", "u:n2", "u:n3", "u:n4"], 0);
        let merged = reattach_cached_queue_tail(fetched, Some("u:n2"), &cached, false, 100);
        assert_eq!(
            merged
                .items
                .iter()
                .map(|i| i.uri.as_str())
                .collect::<Vec<_>>(),
            ["u:n1", "u:n2", "u:n3", "u:n4"]
        );
        assert_eq!(merged.as_of_ms, 100);
    }

    #[test]
    fn empty_fetch_anchors_on_currently_playing() {
        let fetched = queue(Some("u:cur"), &[], 0);
        let cached = queue(Some("u:cur"), &["u:n1", "u:n2"], 0);
        let merged = reattach_cached_queue_tail(fetched, Some("u:cur"), &cached, false, 100);
        assert_eq!(
            merged
                .items
                .iter()
                .map(|i| i.uri.as_str())
                .collect::<Vec<_>>(),
            ["u:n1", "u:n2"]
        );
    }

    #[test]
    fn shuffle_returns_fetch_untouched() {
        let fetched = queue(Some("u:cur"), &["u:x"], 0);
        let cached = queue(Some("u:cur"), &["u:x", "u:y"], 0);
        let merged = reattach_cached_queue_tail(fetched.clone(), Some("u:x"), &cached, true, 100);
        assert_eq!(merged.items.len(), fetched.items.len());
    }

    #[test]
    fn anchor_in_middle_of_cached_items_resumes_after_it() {
        // Wrong-prediction recovery: Spotify advanced to n2 (skipping
        // the predicted n1); n2 is mid-cache, tail resumes from n3.
        let fetched = queue(Some("u:n2"), &[], 0);
        let cached = queue(Some("u:cur"), &["u:n1", "u:n2", "u:n3", "u:n4"], 0);
        let merged = reattach_cached_queue_tail(fetched, Some("u:n2"), &cached, false, 100);
        assert_eq!(
            merged
                .items
                .iter()
                .map(|i| i.uri.as_str())
                .collect::<Vec<_>>(),
            ["u:n3", "u:n4"]
        );
    }

    #[test]
    fn anchor_miss_with_fresh_richer_cache_preserves_upcoming_list() {
        // Autoplay/phone divergence: playing track unknown to the
        // cache, fetch is empty, cache is fresh — keep the tail under
        // the new now-playing instead of wiping to X + [].
        let fetched = queue(Some("u:autoplay"), &[], 0);
        let cached = queue(Some("u:predicted"), &["u:n1", "u:n2"], 95);
        let merged = reattach_cached_queue_tail(fetched, Some("u:autoplay"), &cached, false, 100);
        assert_eq!(
            merged
                .items
                .iter()
                .map(|i| i.uri.as_str())
                .collect::<Vec<_>>(),
            ["u:n1", "u:n2"]
        );
        assert_eq!(
            merged.currently_playing.map(|i| i.uri),
            Some("u:autoplay".to_string())
        );
    }

    #[test]
    fn anchor_miss_with_stale_cache_accepts_the_fetch() {
        let fetched = queue(Some("u:new"), &[], 0);
        let cached = queue(Some("u:old"), &["u:n1"], 0);
        // Cache is 100s old — a genuinely new session; accept the fetch.
        let merged =
            reattach_cached_queue_tail(fetched.clone(), Some("u:new"), &cached, false, 100_000);
        assert!(merged.items.is_empty());
    }

    #[test]
    fn anchor_miss_with_nonempty_fetch_accepts_the_fetch() {
        // A non-empty fetch that doesn't match the cache is a new
        // context — the fetch wins even when the cache is fresh.
        let fetched = queue(Some("u:new"), &["u:other1"], 0);
        let cached = queue(Some("u:old"), &["u:n1", "u:n2"], 95);
        let merged = reattach_cached_queue_tail(fetched, Some("u:other1"), &cached, false, 100);
        assert_eq!(
            merged
                .items
                .iter()
                .map(|i| i.uri.as_str())
                .collect::<Vec<_>>(),
            ["u:other1"]
        );
    }

    #[test]
    fn never_duplicates_queue_entries() {
        let fetched = queue(Some("u:cur"), &["u:n1", "u:n2"], 0);
        let cached = queue(Some("u:cur"), &["u:n1", "u:n2"], 0);
        let merged = reattach_cached_queue_tail(fetched, Some("u:n2"), &cached, false, 100);
        assert_eq!(merged.items.len(), 2);
    }

    #[test]
    fn anchor_prefers_last_upstream_item_then_currently_playing() {
        assert_eq!(
            queue_tail_anchor(&queue(Some("u:cur"), &["u:a", "u:b"], 0)).as_deref(),
            Some("u:b")
        );
        assert_eq!(
            queue_tail_anchor(&queue(Some("u:cur"), &[], 0)).as_deref(),
            Some("u:cur")
        );
        assert_eq!(queue_tail_anchor(&queue(None, &[], 0)), None);
    }
}
