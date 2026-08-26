//! Shared fixtures for the event tests. Lives in `tests/common/` so both
//! `event_tolerance.rs` and `event_kinds_roster.rs` build against one copy —
//! the roster fixture the macOS client reads is generated from these frames, so
//! a sample and the contract it documents cannot drift apart.

#![allow(clippy::panic, clippy::unwrap_used)]

use serde_json::{json, Value};

/// One frame per kind in `DaemonEvent::all_kind_labels()`, written as wire JSON
/// so the samples double as a record of each event's required fields.
pub fn sample_frames() -> Vec<Value> {
    vec![
        json!({"event": "shutdown-requested"}),
        json!({"event": "playback-changed", "action": "play"}),
        json!({"event": "queue-changed", "action": "add", "uris": ["spotify:track:1"]}),
        json!({"event": "devices-changed", "action": "refresh"}),
        json!({"event": "playlists-changed", "action": "create", "playlist": "p1"}),
        json!({"event": "library-changed", "action": "save", "uris": []}),
        json!({"event": "search-updated", "query": "chaka", "count": 3}),
        json!({
            "event": "search-page",
            "query": "chaka",
            "kind": "track",
            "offset": 0,
            "version": 1,
            "items": [],
        }),
        json!({"event": "search-complete", "query": "chaka", "version": 1}),
        json!({"event": "search-failed", "query": "chaka", "version": 1, "message": "boom"}),
        json!({"event": "event-stream-lagged", "skipped": 4}),
        json!({"event": "sync-started", "target": "all"}),
        json!({
            "event": "sync-finished",
            "summary": {
                "target": "all",
                "playback_snapshots": 1,
                "devices": 2,
                "playlists": 3,
                "playlist_items": 4,
                "recent_items": 5,
                "library_items": 6,
                "media_items": 7,
            },
        }),
        json!({"event": "mutation-finished", "action": "like", "message": "saved"}),
        json!({"event": "rate-limited", "retry_after_secs": 3, "scope": "library"}),
        json!({"event": "auth-error", "kind": "expired_refresh"}),
        json!({
            "event": "mutation-accepted",
            "receipt_id": "01890000-0000-7000-8000-000000000001",
            "action": "like",
        }),
        json!({
            "event": "mutation-finalized",
            "receipt_id": "01890000-0000-7000-8000-000000000001",
            "status": "confirmed",
            "message": "done",
        }),
        json!({"event": "schema-compat", "endpoint": "/v1/me/player", "missing_keys": ["item"]}),
        json!({"event": "player-ready", "device_id": "dev-1", "name": "spotuify-hume"}),
        json!({"event": "player-degraded", "reason": "spirc timeout"}),
        json!({"event": "provider-policy", "provider": "spotify", "reason": "region restricted"}),
        json!({
            "event": "provider-policy-cleared",
            "provider": "spotify",
            "reason": "region restricted",
        }),
        json!({"event": "premium-required"}),
        json!({"event": "session-disconnected", "reason": "ap dropped"}),
        json!({"event": "player-failed", "reason": "sink panic", "restarts": 3}),
        json!({
            "event": "listen-qualified",
            "track_uri": "spotify:track:1",
            "duration_ms": 210_000,
            "audible_ms": 120_000,
        }),
        json!({
            "event": "analytics-import-progress",
            "run_id": "run-1",
            "provider": "lastfm",
            "username": "bk",
            "phase": "fetch",
            "fetched": 10,
            "stored": 9,
            "resolved": 8,
            "promoted": 7,
            "unresolved": 1,
        }),
        json!({
            "event": "operation-recorded",
            "operation_id": "01890000-0000-7000-8000-000000000002",
            "kind": "queue_add",
            "source": "cli",
        }),
        json!({
            "event": "operation-undone",
            "undo_op_id": "01890000-0000-7000-8000-000000000003",
            "original_op_id": "01890000-0000-7000-8000-000000000002",
            "success": true,
        }),
        json!({"event": "config-reloaded"}),
        json!({"event": "client-preferences-changed", "preferences": {}}),
        json!({"event": "spectrum-frame", "bands": vec![0.0_f32; 12], "peak": 0.5, "timestamp_ms": 42}),
        json!({"event": "viz-source-changed", "active": "sink", "configured": "auto"}),
        json!({
            "event": "reminder-due",
            "notification": {
                "id": "n1",
                "reminder_id": "r1",
                "media_uri": "spotify:album:1",
                "media_kind": "album",
                "name": "Naughty",
                "subtitle": "Chaka Khan",
                "due_at_ms": 1,
                "fired_at_ms": 2,
                "state": "unseen",
            },
        }),
        json!({"event": "reminders-changed", "action": "create"}),
        json!({"event": "bookmarks-changed", "action": "create"}),
        json!({
            "event": "eq-changed",
            "settings": {"preset": "Flat", "bands": vec![0.0_f32; 10]},
            "applied": true,
        }),
        json!({
            "event": "update-available",
            "latest_version": "9.9.9",
            "upgrade": {"method": "homebrew", "command": "brew upgrade spotuify"},
        }),
        json!({"event": "auth-migration-recommended", "can_login_dev_app": true}),
    ]
}
