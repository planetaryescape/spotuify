//! Phase 6.6/6.7 — extended receipt + new DaemonEvent variants.
//!
//! Verifies serde round-trip stability and the event tag scheme. The
//! receipt lifecycle wrapping in the daemon handler is a separate
//! integration test in the daemon crate; here we lock the wire format
//! so any breaking change to the protocol is visible in this file.

use spotuify_protocol::{
    ApiErrorSummary, AuthErrorKind, DaemonEvent, IpcErrorKind, Receipt, ReceiptId, ReceiptStatus,
};

#[test]
fn receipt_id_v7_is_unique_and_round_trips_through_json() {
    let a = ReceiptId::new_v7();
    let b = ReceiptId::new_v7();
    assert_ne!(a, b, "v7 receipt ids must differ across calls");

    let json = serde_json::to_string(&a).unwrap();
    let back: ReceiptId = serde_json::from_str(&json).unwrap();
    assert_eq!(a, back);
}

#[test]
fn pending_receipt_round_trips_with_optional_fields_omitted() {
    let id = ReceiptId::new_v7();
    let r = Receipt {
        receipt_id: id,
        action: "playlist_add".to_string(),
        status: ReceiptStatus::Pending,
        message: "queued".to_string(),
        started_at_ms: 1_000_000_000_000,
        finished_at_ms: None,
        error: None,
    };
    let json = serde_json::to_value(&r).unwrap();

    // Pending receipts omit finished_at_ms + error.
    assert!(json.get("finished_at_ms").is_none(), "got {json}");
    assert!(json.get("error").is_none(), "got {json}");

    let back: Receipt = serde_json::from_value(json).unwrap();
    assert_eq!(back, r);
}

#[test]
fn confirmed_receipt_includes_finished_at_ms() {
    let r = Receipt {
        receipt_id: ReceiptId::new_v7(),
        action: "library_save".to_string(),
        status: ReceiptStatus::Confirmed,
        message: "saved".to_string(),
        started_at_ms: 1_000_000_000_000,
        finished_at_ms: Some(1_000_000_000_500),
        error: None,
    };
    let json = serde_json::to_value(&r).unwrap();
    assert_eq!(
        json.get("finished_at_ms").and_then(|v| v.as_i64()),
        Some(1_000_000_000_500)
    );
    assert_eq!(
        json.get("status").and_then(|v| v.as_str()),
        Some("confirmed")
    );
}

#[test]
fn failed_receipt_carries_typed_error_summary() {
    let r = Receipt {
        receipt_id: ReceiptId::new_v7(),
        action: "playlist_add".to_string(),
        status: ReceiptStatus::Failed,
        message: "rate limited".to_string(),
        started_at_ms: 1,
        finished_at_ms: Some(2),
        error: Some(ApiErrorSummary {
            kind: IpcErrorKind::RateLimited,
            message: "retry in 60s".to_string(),
            retry_after_secs: Some(60),
        }),
    };

    let json = serde_json::to_value(&r).unwrap();
    let err = json
        .get("error")
        .expect("error field present on failed receipt");
    assert_eq!(
        err.get("kind").and_then(|v| v.as_str()),
        Some("rate_limited")
    );
    assert_eq!(
        err.get("retry_after_secs").and_then(|v| v.as_u64()),
        Some(60)
    );

    let back: Receipt = serde_json::from_value(json).unwrap();
    assert_eq!(back, r);
}

#[test]
fn rate_limited_event_serializes_with_kebab_case_tag() {
    let event = DaemonEvent::RateLimited {
        retry_after_secs: 30,
        scope: "GET /me/player".to_string(),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(
        json.get("event").and_then(|v| v.as_str()),
        Some("rate-limited")
    );
    assert_eq!(
        json.get("retry_after_secs").and_then(|v| v.as_u64()),
        Some(30)
    );

    let back: DaemonEvent = serde_json::from_value(json).unwrap();
    assert_eq!(back, event);
}

#[test]
fn auth_error_event_carries_typed_kind() {
    let event = DaemonEvent::AuthError {
        kind: AuthErrorKind::ExpiredRefresh,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(
        json.get("event").and_then(|v| v.as_str()),
        Some("auth-error")
    );
    assert_eq!(
        json.get("kind").and_then(|v| v.as_str()),
        Some("expired_refresh")
    );
}

#[test]
fn mutation_accepted_event_carries_receipt_id_and_action() {
    let id = ReceiptId::new_v7();
    let event = DaemonEvent::MutationAccepted {
        receipt_id: id,
        action: "playlist_add".to_string(),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(
        json.get("event").and_then(|v| v.as_str()),
        Some("mutation-accepted")
    );
    assert_eq!(
        json.get("action").and_then(|v| v.as_str()),
        Some("playlist_add")
    );

    let back: DaemonEvent = serde_json::from_value(json).unwrap();
    assert_eq!(back, event);
}

#[test]
fn mutation_finalized_event_carries_status_and_message() {
    let event = DaemonEvent::MutationFinalized {
        receipt_id: ReceiptId::new_v7(),
        status: ReceiptStatus::Confirmed,
        message: "added 5 tracks".to_string(),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(
        json.get("event").and_then(|v| v.as_str()),
        Some("mutation-finalized")
    );
    assert_eq!(
        json.get("status").and_then(|v| v.as_str()),
        Some("confirmed")
    );

    let back: DaemonEvent = serde_json::from_value(json).unwrap();
    assert_eq!(back, event);
}

#[test]
fn schema_compat_event_round_trips_with_missing_keys_list() {
    let event = DaemonEvent::SchemaCompat {
        endpoint: "GET /tracks/abc".to_string(),
        missing_keys: vec!["available_markets".into(), "external_ids".into()],
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(
        json.get("event").and_then(|v| v.as_str()),
        Some("schema-compat")
    );

    let back: DaemonEvent = serde_json::from_value(json).unwrap();
    assert_eq!(back, event);
}

#[test]
fn existing_legacy_events_still_round_trip_after_additions() {
    // Asserts the additions are non-breaking for established consumers.
    let event = DaemonEvent::PlaybackChanged {
        action: "play".to_string(),
        playback: None,
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(
        json.get("event").and_then(|v| v.as_str()),
        Some("playback-changed")
    );
    let back: DaemonEvent = serde_json::from_value(json).unwrap();
    assert_eq!(back, event);
}
