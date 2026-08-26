//! Forward-compatibility contract for the daemon → client event stream.
//!
//! Adding an event variant or field never bumps `IPC_PROTOCOL_VERSION`, so an
//! old client must survive a newer daemon. These tests pin the two rules from
//! the crate docs: unknown tags decode to `Unknown` instead of erroring, and an
//! extra field on a known tag is ignored.

#![allow(clippy::panic, clippy::unwrap_used)]

use serde_json::{json, Value};
use spotuify_protocol::{DaemonEvent, IpcMessage, IpcPayload};

/// One frame per kind in `DaemonEvent::all_kind_labels()`, written as wire JSON
/// so the samples double as a record of each event's required fields.
fn sample_frames() -> Vec<Value> {
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

fn decode(frame: &Value) -> DaemonEvent {
    serde_json::from_value(frame.clone()).expect("DaemonEvent decoding never errors")
}

/// Why a frame failed the strict path. `decode` swallows that error by design
/// (that is the whole tolerance feature), so tests surface it themselves rather
/// than leaving a maintainer with a bare "did not decode".
fn strict_error(frame: &Value) -> String {
    // Inherent fn from `#[serde(remote = "Self")]` — the strict, derived codec.
    DaemonEvent::deserialize(frame).map_or_else(|err| err.to_string(), |_| String::new())
}

#[test]
fn samples_cover_every_kind_in_the_roster() {
    let decoded: Vec<DaemonEvent> = sample_frames().iter().map(decode).collect();
    for (frame, event) in sample_frames().iter().zip(&decoded) {
        // `Unknown` borrows the wire tag for `kind_label`, so check the variant
        // itself: a sample with a wrong field would otherwise pass silently.
        assert!(
            !matches!(event, DaemonEvent::Unknown { .. }),
            "sample frame did not decode into its variant: {frame}\n  cause: {}",
            strict_error(frame)
        );
        assert_eq!(
            event.kind_label(),
            frame["event"].as_str().unwrap(),
            "sample frame decoded to the wrong variant: {frame}"
        );
    }

    let mut labels: Vec<&str> = decoded.iter().map(DaemonEvent::kind_label).collect();
    labels.sort_unstable();
    assert_eq!(
        labels,
        DaemonEvent::all_kind_labels(),
        "sample frames and all_kind_labels disagree; add the new event to both"
    );
}

#[test]
fn every_known_variant_round_trips() {
    for frame in sample_frames() {
        let event = decode(&frame);
        let reencoded = serde_json::to_value(&event).unwrap();
        assert_eq!(
            decode(&reencoded),
            event,
            "round trip changed the event: {frame}"
        );
        assert_eq!(
            reencoded["event"], frame["event"],
            "round trip changed the wire tag: {frame}"
        );
    }
}

#[test]
fn an_extra_field_from_a_newer_daemon_is_ignored() {
    // Rule 2 from the crate docs, in the direction serde gives us for free:
    // an old client must not choke on a field a newer daemon added.
    for frame in sample_frames() {
        let expected = decode(&frame);
        let mut extended = frame.clone();
        extended["from_the_future"] = json!({"nested": [1, 2, 3]});
        assert_eq!(
            decode(&extended),
            expected,
            "an additive field broke decoding: {frame}"
        );
    }
}

#[test]
fn an_unknown_tag_decodes_to_unknown_and_keeps_the_raw_frame() {
    let frame = json!({"event": "from-the-future", "x": 1});
    let event = decode(&frame);
    let DaemonEvent::Unknown { event: tag, raw } = &event else {
        panic!("expected Unknown, got {event:?}");
    };
    assert_eq!(tag, "from-the-future");
    assert_eq!(raw, &frame);
    // `--kind` filters on what the daemon sent, and a relay re-emits the frame
    // verbatim rather than this build's guess at it.
    assert_eq!(event.kind_label(), "from-the-future");
    assert_eq!(serde_json::to_value(&event).unwrap(), frame);
}

#[test]
fn a_known_tag_missing_a_required_field_degrades_instead_of_killing_the_stream() {
    // The compatible fix is `#[serde(default)]` on the new field; this asserts
    // the failure mode when someone forgets is one lost event, not a dead
    // stream (the client sees Unknown and can log the tag).
    let event = decode(&json!({"event": "player-ready", "device_id": "dev-1"}));
    let DaemonEvent::Unknown { event: tag, .. } = &event else {
        panic!("expected Unknown, got {event:?}");
    };
    assert_eq!(tag, "player-ready");
}

#[test]
fn an_unknown_event_survives_the_ipc_envelope() {
    // The tolerance has to hold where clients actually read events: nested in
    // an IpcMessage, decoded by the same codec `IpcClient::next_event` uses.
    let raw = serde_json::to_vec(&json!({
        "id": 1,
        "payload": {"type": "Event", "event": "from-the-future", "detail": {"x": 1}},
    }))
    .unwrap();
    let message: IpcMessage = serde_json::from_slice(&raw).unwrap();
    let IpcPayload::Event(DaemonEvent::Unknown { event, .. }) = message.payload else {
        panic!("expected an unknown event payload");
    };
    assert_eq!(event, "from-the-future");
}
