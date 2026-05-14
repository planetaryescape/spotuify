//! Phase 6.5 — sync refetch gate decision tests.

use spotuify_sync::{should_refetch_playlist_tracks, should_refetch_saved_tracks};

// --- Playlist snapshot-id gate ---

#[test]
fn first_sync_with_no_local_snapshot_refetches() {
    assert!(should_refetch_playlist_tracks(None, Some("snap-1")));
}

#[test]
fn matching_snapshots_skip_refetch() {
    assert!(!should_refetch_playlist_tracks(
        Some("snap-1"),
        Some("snap-1")
    ));
}

#[test]
fn differing_snapshots_trigger_refetch() {
    assert!(should_refetch_playlist_tracks(
        Some("snap-1"),
        Some("snap-2")
    ));
}

#[test]
fn missing_remote_snapshot_refetches_defensively() {
    // The Spotify response didn't include snapshot_id -- we can't
    // prove unchanged, so refetch.
    assert!(should_refetch_playlist_tracks(Some("snap-1"), None));
}

#[test]
fn both_missing_snapshots_refetches() {
    // Cold start with a playlist that never carries snapshot_id.
    assert!(should_refetch_playlist_tracks(None, None));
}

#[test]
fn empty_string_snapshot_is_distinct_from_missing() {
    // Implementation detail: empty string is a valid (if degenerate)
    // snapshot id; it shouldn't be treated as None.
    assert!(!should_refetch_playlist_tracks(Some(""), Some("")));
    assert!(should_refetch_playlist_tracks(Some(""), Some("real-snap")));
}

// --- Saved-tracks page-0 unchanged shortcut ---

#[test]
fn matching_total_and_first_ids_skips_refetch() {
    let local = ["track:1", "track:2", "track:3"];
    let remote = ["track:1", "track:2", "track:3"];
    assert!(!should_refetch_saved_tracks(100, &local, 100, &remote));
}

#[test]
fn differing_total_triggers_refetch() {
    let local = ["track:1", "track:2"];
    let remote = ["track:1", "track:2"];
    // total changed even though the visible page matches -- maybe a
    // delete at the bottom. Refetch to be safe.
    assert!(should_refetch_saved_tracks(100, &local, 99, &remote));
}

#[test]
fn new_track_at_top_changes_first_ids_and_refetches() {
    let local = ["old-1", "old-2"];
    let remote = ["new-1", "old-1", "old-2"];
    assert!(should_refetch_saved_tracks(100, &local, 101, &remote));
}

#[test]
fn same_total_but_different_first_ids_refetches() {
    // Rare reorder + replace where total stays equal. Refetch.
    let local = ["a", "b", "c"];
    let remote = ["b", "a", "c"];
    assert!(should_refetch_saved_tracks(50, &local, 50, &remote));
}

#[test]
fn empty_library_matches_empty_library() {
    let empty: [&str; 0] = [];
    assert!(!should_refetch_saved_tracks(0, &empty, 0, &empty));
}

#[test]
fn zero_local_versus_nonzero_remote_refetches() {
    let empty: [&str; 0] = [];
    let remote = ["track:1"];
    assert!(should_refetch_saved_tracks(0, &empty, 1, &remote));
}
