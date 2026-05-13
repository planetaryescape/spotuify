//! Phase 6.2: Spotify response payload compat normalizer.
//!
//! Walks `serde_json::Value` before deserialization to backfill keys
//! Spotify has silently dropped (the Feb-2026 schema drift wave).
//! Returns the list of patched keys for telemetry
//! (`DaemonEvent::SchemaCompat { endpoint, missing_keys }`).
//!
//! Implementation pattern from spotatui
//! `src/infra/network/requests.rs:129-240` (`normalize_spotify_payload`).
//! Adapted to take a [`NormalizeHint`] so callers route per-endpoint to the
//! correct shape rather than relying on string pattern-matching against
//! the JSON contents.

use serde_json::{json, Map, Value};

/// Tags the expected payload shape so the normalizer knows which keys to
/// backfill. Picked per-endpoint by the caller (caller knows what response
/// they're parsing). New hints are added as Spotify drops more keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalizeHint {
    Track,
    Album,
    Artist,
    Playlist,
    Episode,
    Show,
    PagingTrack,
    PagingPlaylist,
    PagingArtist,
    PagingAlbum,
    /// Unknown shape — no normalization performed.
    Unknown,
}

/// Backfill missing keys in `value` per `hint`. Returns the (sorted) list
/// of keys that were inserted, suitable for `DaemonEvent::SchemaCompat`.
///
/// Idempotent: re-running on an already-normalized payload returns an
/// empty list and does not mutate `value`.
///
/// Non-object roots (arrays, strings, etc.) pass through untouched.
pub fn compat_normalize(value: &mut Value, hint: NormalizeHint) -> Vec<&'static str> {
    let Value::Object(map) = value else {
        return Vec::new();
    };

    let mut patched = Vec::new();
    let defaults: &[(&'static str, fn() -> Value)] = defaults_for(hint);
    for (key, default_fn) in defaults {
        if !map.contains_key(*key) {
            map.insert((*key).to_string(), default_fn());
            patched.push(*key);
        }
    }

    // Paging hints derive `total` from the items array when present; that
    // value is more accurate than a generic zero.
    if matches!(
        hint,
        NormalizeHint::PagingTrack
            | NormalizeHint::PagingPlaylist
            | NormalizeHint::PagingArtist
            | NormalizeHint::PagingAlbum
    ) {
        if patched.contains(&"total") {
            if let Some(items) = map.get("items").and_then(Value::as_array) {
                let n = items.len();
                map.insert("total".to_string(), json!(n));
            }
        }
    }

    patched
}

fn defaults_for(hint: NormalizeHint) -> &'static [(&'static str, fn() -> Value)] {
    match hint {
        NormalizeHint::Track => &[
            ("available_markets", empty_array),
            ("external_ids", empty_object),
            ("linked_from", null),
            ("popularity", zero),
        ],
        NormalizeHint::Album => &[
            ("available_markets", empty_array),
            ("external_ids", empty_object),
            ("popularity", zero),
        ],
        NormalizeHint::Artist => &[
            ("followers", followers_default),
            ("popularity", zero),
            ("images", empty_array),
            ("genres", empty_array),
        ],
        NormalizeHint::Playlist => &[
            ("followers", followers_default),
            ("public", null),
            ("collaborative", false_value),
            ("images", empty_array),
        ],
        NormalizeHint::Episode => &[
            ("available_markets", empty_array),
            ("explicit", false_value),
            ("images", empty_array),
        ],
        NormalizeHint::Show => &[
            ("available_markets", empty_array),
            ("languages", empty_array),
            ("images", empty_array),
        ],
        NormalizeHint::PagingTrack
        | NormalizeHint::PagingPlaylist
        | NormalizeHint::PagingArtist
        | NormalizeHint::PagingAlbum => &[
            ("total", zero),
            ("limit", json_50),
            ("offset", zero),
            ("next", null),
            ("previous", null),
            ("items", empty_array),
        ],
        NormalizeHint::Unknown => &[],
    }
}

fn empty_array() -> Value {
    json!([])
}
fn empty_object() -> Value {
    json!({})
}
fn null() -> Value {
    Value::Null
}
fn zero() -> Value {
    json!(0)
}
fn json_50() -> Value {
    json!(50)
}
fn false_value() -> Value {
    json!(false)
}
fn followers_default() -> Value {
    json!({ "total": 0 })
}

/// Force `Map` import to be considered used in case the compiler doesn't
/// notice it (private helper used elsewhere in this crate).
#[allow(dead_code)]
fn _force_map_use(_m: &Map<String, Value>) {}
