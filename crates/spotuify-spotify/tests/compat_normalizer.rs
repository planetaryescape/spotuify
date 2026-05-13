//! Phase 6.2 — Spotify payload compat normalizer.
//!
//! Verifies that the normalizer backfills keys Spotify silently drops
//! before deserialization. Pattern adopted from spotatui's Feb-2026 fix
//! (`src/infra/network/requests.rs:129-240`).
//!
//! Tests are adversarial: each one strips a specific key from a known-good
//! payload and asserts the normalizer reinserts the expected default AND
//! reports the missing key for telemetry. The "already complete" test
//! ensures we don't churn telemetry on healthy responses.

use serde_json::json;
use spotuify_spotify::compat::{compat_normalize, NormalizeHint};

#[test]
fn test_track_payload_missing_external_ids_normalizes_to_empty_object() {
    let mut value = json!({
        "name": "Never Too Much",
        "uri": "spotify:track:1",
        "duration_ms": 180_000,
        "track_number": 1,
        "album": {"name": "x"},
        "artists": [{"name": "y"}],
        "available_markets": ["US"],
        "linked_from": null,
        "popularity": 50
    });

    let patched = compat_normalize(&mut value, NormalizeHint::Track);

    assert!(patched.contains(&"external_ids"));
    assert_eq!(value.get("external_ids"), Some(&json!({})));
}

#[test]
fn test_track_payload_missing_available_markets_normalizes_to_empty_array() {
    let mut value = json!({
        "name": "Track",
        "uri": "spotify:track:1",
        "duration_ms": 1,
        "track_number": 1,
        "album": {"name": "a"},
        "artists": [],
        "external_ids": {},
        "linked_from": null,
        "popularity": 0
    });

    let patched = compat_normalize(&mut value, NormalizeHint::Track);

    assert!(patched.contains(&"available_markets"));
    assert_eq!(value.get("available_markets"), Some(&json!([])));
}

#[test]
fn test_track_payload_missing_linked_from_normalizes_to_null() {
    let mut value = json!({
        "name": "Track",
        "uri": "spotify:track:1",
        "duration_ms": 1,
        "track_number": 1,
        "album": {"name": "a"},
        "artists": [],
        "external_ids": {},
        "available_markets": [],
        "popularity": 0
    });

    let patched = compat_normalize(&mut value, NormalizeHint::Track);

    assert!(patched.contains(&"linked_from"));
    assert_eq!(value.get("linked_from"), Some(&serde_json::Value::Null));
}

#[test]
fn test_track_payload_missing_popularity_normalizes_to_zero() {
    let mut value = json!({
        "name": "Track",
        "uri": "spotify:track:1",
        "duration_ms": 1,
        "track_number": 1,
        "album": {"name": "a"},
        "artists": [],
        "external_ids": {},
        "available_markets": [],
        "linked_from": null
    });

    let patched = compat_normalize(&mut value, NormalizeHint::Track);

    assert!(patched.contains(&"popularity"));
    assert_eq!(value.get("popularity"), Some(&json!(0)));
}

#[test]
fn test_artist_payload_missing_followers_normalizes() {
    let mut value = json!({
        "name": "Luther Vandross",
        "uri": "spotify:artist:1",
        "id": "1"
    });

    let patched = compat_normalize(&mut value, NormalizeHint::Artist);

    assert!(patched.contains(&"followers"));
    assert_eq!(value["followers"], json!({"total": 0}));
}

#[test]
fn test_playlist_payload_missing_followers_normalizes() {
    let mut value = json!({
        "name": "My Playlist",
        "uri": "spotify:playlist:1",
        "id": "1",
        "owner": {"id": "user"}
    });

    let patched = compat_normalize(&mut value, NormalizeHint::Playlist);

    assert!(patched.contains(&"followers"));
    assert_eq!(value["followers"], json!({"total": 0}));
}

#[test]
fn test_paging_payload_missing_total_handles_with_item_count() {
    let mut value = json!({
        "items": [{"name": "a"}, {"name": "b"}, {"name": "c"}],
        "limit": 50,
        "offset": 0,
        "next": null,
        "previous": null
    });

    let patched = compat_normalize(&mut value, NormalizeHint::PagingTrack);

    assert!(patched.contains(&"total"));
    assert_eq!(value["total"], json!(3));
}

#[test]
fn test_paging_payload_missing_items_normalizes_to_empty_array() {
    let mut value = json!({
        "total": 0,
        "limit": 50,
        "offset": 0,
        "next": null,
        "previous": null
    });

    let patched = compat_normalize(&mut value, NormalizeHint::PagingTrack);

    assert!(patched.contains(&"items"));
    assert_eq!(value["items"], json!([]));
}

#[test]
fn test_already_complete_payload_unchanged_and_no_telemetry() {
    let original = json!({
        "name": "Track",
        "uri": "spotify:track:1",
        "duration_ms": 1,
        "track_number": 1,
        "album": {"name": "a"},
        "artists": [],
        "external_ids": {},
        "available_markets": [],
        "linked_from": null,
        "popularity": 0
    });
    let mut value = original.clone();

    let patched = compat_normalize(&mut value, NormalizeHint::Track);

    assert!(patched.is_empty(), "no key should be patched on a complete payload, got {patched:?}");
    assert_eq!(value, original, "complete payload must not be mutated");
}

#[test]
fn test_normalize_returns_all_patched_keys_in_telemetry_set() {
    let mut value = json!({
        "name": "Track",
        "uri": "spotify:track:1",
        "duration_ms": 1,
        "track_number": 1,
        "album": {"name": "a"},
        "artists": []
        // missing: available_markets, external_ids, linked_from, popularity
    });

    let patched = compat_normalize(&mut value, NormalizeHint::Track);
    let set: std::collections::BTreeSet<_> = patched.into_iter().collect();
    let want: std::collections::BTreeSet<_> = ["available_markets", "external_ids", "linked_from", "popularity"]
        .into_iter()
        .collect();
    assert_eq!(set, want);
}

#[test]
fn test_unknown_shape_does_not_mutate_or_report_keys() {
    let original = json!({"hello": "world", "n": 42});
    let mut value = original.clone();

    let patched = compat_normalize(&mut value, NormalizeHint::Unknown);

    assert!(patched.is_empty());
    assert_eq!(value, original);
}

#[test]
fn test_normalize_handles_non_object_root_gracefully() {
    let mut value = json!("just a string");
    let patched = compat_normalize(&mut value, NormalizeHint::Track);
    assert!(patched.is_empty(), "non-object root should be a no-op");
    assert_eq!(value, json!("just a string"));
}

#[test]
fn test_episode_payload_missing_available_markets_normalizes() {
    let mut value = json!({
        "name": "Episode",
        "uri": "spotify:episode:1",
        "id": "1",
        "duration_ms": 10000
    });

    let patched = compat_normalize(&mut value, NormalizeHint::Episode);

    assert!(patched.contains(&"available_markets"));
    assert_eq!(value["available_markets"], json!([]));
}

#[test]
fn test_album_payload_missing_external_ids_normalizes() {
    let mut value = json!({
        "name": "Album",
        "uri": "spotify:album:1",
        "id": "1",
        "album_type": "album"
    });

    let patched = compat_normalize(&mut value, NormalizeHint::Album);

    assert!(patched.contains(&"external_ids"));
    assert_eq!(value["external_ids"], json!({}));
}
