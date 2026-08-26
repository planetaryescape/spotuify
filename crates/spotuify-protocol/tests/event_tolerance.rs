//! Forward-compatibility contract for the daemon → client event stream.
//!
//! Adding an event variant or field never bumps `IPC_PROTOCOL_VERSION`, so an
//! old client must survive a newer daemon. These tests pin the two rules from
//! the crate docs: unknown tags decode to `Unknown` instead of erroring, and an
//! extra field on a known tag is ignored.

#![allow(clippy::panic, clippy::unwrap_used)]

use serde_json::{json, Value};
use spotuify_protocol::{DaemonEvent, IpcMessage, IpcPayload, UnknownReason};

mod common;

use common::sample_frames;

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
fn every_sample_field_is_required() {
    // The samples are minimal by contract, and the macOS parity test leans on
    // that: it drops one key at a time and demands a degrade, which is only a
    // valid probe if every key present is genuinely required. An optional key
    // in a sample would make that test assert something false, so pin it here
    // where the strict decoder can answer.
    for frame in sample_frames() {
        let kind = frame["event"].as_str().unwrap().to_string();
        let fields: Vec<String> = frame
            .as_object()
            .unwrap()
            .keys()
            .filter(|key| *key != "event")
            .cloned()
            .collect();
        for field in fields {
            let mut without = frame.clone();
            without.as_object_mut().unwrap().remove(&field);
            assert!(
                matches!(decode(&without), DaemonEvent::Unknown { .. }),
                "{kind}.{field} is optional, so the sample is not minimal: drop it"
            );
        }
    }
}

#[test]
fn an_unknown_tag_decodes_to_unknown_and_keeps_the_raw_frame() {
    let frame = json!({"event": "from-the-future", "x": 1});
    let event = decode(&frame);
    let DaemonEvent::Unknown {
        event: tag,
        reason,
        raw,
    } = &event
    else {
        panic!("expected Unknown, got {event:?}");
    };
    assert_eq!(tag, "from-the-future");
    // Nothing was lost: this build was never meant to understand the frame.
    assert_eq!(*reason, UnknownReason::UnknownTag);
    assert_eq!(raw, &frame);
    // `--kind` filters on what the daemon sent, and a relay re-emits the frame
    // verbatim rather than this build's guess at it.
    assert_eq!(event.kind_label(), "from-the-future");
    assert_eq!(serde_json::to_value(&event).unwrap(), frame);
}

#[test]
fn a_known_tag_missing_a_required_field_reports_an_undecodable_known_tag() {
    // The compatible fix is `#[serde(default)]` on the new field. When someone
    // forgets, the stream survives but an event the daemon meant us to act on
    // is lost — so the reason has to say so, loudly enough that clients can
    // re-seed instead of shrugging the way they do for a tag from the future.
    let event = decode(&json!({"event": "player-ready", "device_id": "dev-1"}));
    let DaemonEvent::Unknown {
        event: tag, reason, ..
    } = &event
    else {
        panic!("expected Unknown, got {event:?}");
    };
    assert_eq!(tag, "player-ready");
    assert_eq!(*reason, UnknownReason::UndecodableKnownTag);
}

#[test]
fn every_roster_kind_can_be_undecodable() {
    // The reason is derived from the roster, so a kind missing from
    // `all_kind_labels()` would silently downgrade to UnknownTag and rob its
    // clients of the re-seed. Check the whole roster, not one sample.
    for kind in DaemonEvent::all_kind_labels() {
        // A frame with the tag and nothing else: either it decodes (all fields
        // optional) or it degrades, and degrading must name the right reason.
        let event = decode(&json!({ "event": kind }));
        if let DaemonEvent::Unknown { reason, .. } = &event {
            assert_eq!(
                *reason,
                UnknownReason::UndecodableKnownTag,
                "{kind} degraded as if it were a tag from the future"
            );
        }
    }
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
    let IpcPayload::Event(DaemonEvent::Unknown { event, reason, .. }) = message.payload else {
        panic!("expected an unknown event payload");
    };
    assert_eq!(event, "from-the-future");
    assert_eq!(reason, UnknownReason::UnknownTag);
}
