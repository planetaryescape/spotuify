//! Phase 9 — new DaemonEvent variants for the embedded librespot player.
//!
//! Adversarial assertions: wire shape (kebab-case `event` tag), round-trip
//! data preservation, LoggedEvent mapping decisions for each variant
//! (positive signals ignored; degraded/required/failed lifted into the
//! ring buffer), and findings_from severity/category for the lifted
//! kinds.

use spotuify_protocol::{
    findings_from, DaemonEvent, DoctorFindingCategory, DoctorFindingSeverity, LoggedEvent,
    LoggedKind,
};

fn now_ms() -> i64 {
    1_700_000_000_000
}

// ---------- wire-format shape ----------

#[test]
fn player_ready_wire_shape_is_kebab_case_and_tagged() {
    let raw = serde_json::to_string(&DaemonEvent::PlayerReady {
        device_id: "abc123".to_string(),
        name: "spotuify".to_string(),
    })
    .unwrap();

    assert!(raw.contains("\"event\":\"player-ready\""), "raw: {raw}");
    assert!(raw.contains("\"device_id\":\"abc123\""), "raw: {raw}");
    assert!(raw.contains("\"name\":\"spotuify\""), "raw: {raw}");
}

#[test]
fn player_degraded_wire_shape_includes_reason() {
    let raw = serde_json::to_string(&DaemonEvent::PlayerDegraded {
        reason: "spirc-outer-timeout".to_string(),
    })
    .unwrap();

    assert!(raw.contains("\"event\":\"player-degraded\""), "raw: {raw}");
    assert!(
        raw.contains("\"reason\":\"spirc-outer-timeout\""),
        "raw: {raw}"
    );
}

#[test]
fn premium_required_is_unit_variant_on_the_wire() {
    let raw = serde_json::to_string(&DaemonEvent::PremiumRequired).unwrap();

    // Adversarial: unit variants on a tagged enum must produce
    // `{"event":"premium-required"}` and nothing else. Catches the bug
    // where someone accidentally adds a field and breaks all clients.
    assert_eq!(raw, "{\"event\":\"premium-required\"}");
}

#[test]
fn session_disconnected_wire_shape_includes_reason() {
    let raw = serde_json::to_string(&DaemonEvent::SessionDisconnected {
        reason: "session-invalid".to_string(),
    })
    .unwrap();

    assert!(
        raw.contains("\"event\":\"session-disconnected\""),
        "raw: {raw}"
    );
    assert!(raw.contains("\"reason\":\"session-invalid\""), "raw: {raw}");
}

#[test]
fn player_failed_wire_shape_includes_reason_and_restarts() {
    let raw = serde_json::to_string(&DaemonEvent::PlayerFailed {
        reason: "max-restarts-exceeded".to_string(),
        restarts: 5,
    })
    .unwrap();

    assert!(raw.contains("\"event\":\"player-failed\""), "raw: {raw}");
    assert!(
        raw.contains("\"reason\":\"max-restarts-exceeded\""),
        "raw: {raw}"
    );
    assert!(raw.contains("\"restarts\":5"), "raw: {raw}");
}

// ---------- round-trip data preservation ----------

#[test]
fn player_ready_round_trips_through_json() {
    let original = DaemonEvent::PlayerReady {
        device_id: "device-7".to_string(),
        name: "studio mac".to_string(),
    };
    let raw = serde_json::to_string(&original).unwrap();
    let decoded: DaemonEvent = serde_json::from_str(&raw).unwrap();

    match decoded {
        DaemonEvent::PlayerReady { device_id, name } => {
            assert_eq!(device_id, "device-7");
            assert_eq!(name, "studio mac");
        }
        other => panic!("expected PlayerReady, got {other:?}"),
    }
}

#[test]
fn player_failed_round_trips_with_restart_count() {
    let original = DaemonEvent::PlayerFailed {
        reason: "sink-panic-budget".to_string(),
        restarts: 5,
    };
    let raw = serde_json::to_string(&original).unwrap();
    let decoded: DaemonEvent = serde_json::from_str(&raw).unwrap();

    match decoded {
        DaemonEvent::PlayerFailed { reason, restarts } => {
            assert_eq!(reason, "sink-panic-budget");
            assert_eq!(restarts, 5);
        }
        other => panic!("expected PlayerFailed, got {other:?}"),
    }
}

// ---------- LoggedEvent::from exhaustive decisions ----------

#[test]
fn logged_event_ignores_player_ready_positive_signal() {
    // PlayerReady is a success signal; the ring buffer drives doctor
    // findings, so ready should not occupy a slot. Adversarial: catches
    // the regression where someone adds a "log everything" pass and
    // floods the buffer with ready events on every reconnect.
    let event = DaemonEvent::PlayerReady {
        device_id: "x".to_string(),
        name: "spotuify".to_string(),
    };
    assert!(LoggedEvent::from(&event, now_ms()).is_none());
}

#[test]
fn logged_event_ignores_transient_player_degraded() {
    // PlayerDegraded is transient (Spirc retry expected). Doctor findings
    // for it would be noisy; we wait for the persistent PlayerFailed.
    let event = DaemonEvent::PlayerDegraded {
        reason: "spirc-timeout".to_string(),
    };
    assert!(LoggedEvent::from(&event, now_ms()).is_none());
}

#[test]
fn logged_event_lifts_premium_required() {
    let event = DaemonEvent::PremiumRequired;
    let logged = LoggedEvent::from(&event, now_ms()).unwrap();
    assert!(matches!(logged.kind, LoggedKind::PremiumRequired));
    assert_eq!(logged.at_ms, now_ms());
}

#[test]
fn logged_event_lifts_session_disconnected_with_reason() {
    let event = DaemonEvent::SessionDisconnected {
        reason: "session-invalid".to_string(),
    };
    let logged = LoggedEvent::from(&event, now_ms()).unwrap();
    match logged.kind {
        LoggedKind::SessionDisconnected { reason } => {
            assert_eq!(reason, "session-invalid");
        }
        other => panic!("expected SessionDisconnected, got {other:?}"),
    }
}

#[test]
fn logged_event_lifts_player_failed_with_restart_count() {
    let event = DaemonEvent::PlayerFailed {
        reason: "max-restarts-exceeded".to_string(),
        restarts: 5,
    };
    let logged = LoggedEvent::from(&event, now_ms()).unwrap();
    match logged.kind {
        LoggedKind::PlayerFailed { reason, restarts } => {
            assert_eq!(reason, "max-restarts-exceeded");
            assert_eq!(restarts, 5);
        }
        other => panic!("expected PlayerFailed, got {other:?}"),
    }
}

// ---------- findings_from severity decisions ----------

#[test]
fn premium_required_emits_sticky_player_error_finding() {
    // "Ever" lookback: PremiumRequired sticks until the user upgrades.
    // Adversarial: assert it survives even when the event is hours old.
    let events = vec![LoggedEvent {
        at_ms: now_ms() - (4 * 60 * 60 * 1000), // 4 hours ago
        kind: LoggedKind::PremiumRequired,
    }];
    let findings = findings_from(&events, now_ms());
    let player_findings: Vec<_> = findings
        .iter()
        .filter(|f| f.category == DoctorFindingCategory::Player)
        .collect();

    assert_eq!(player_findings.len(), 1);
    assert_eq!(player_findings[0].severity, DoctorFindingSeverity::Error);
    assert!(
        player_findings[0]
            .message
            .to_lowercase()
            .contains("premium"),
        "msg: {}",
        player_findings[0].message
    );
}

#[test]
fn player_failed_emits_sticky_player_error_finding() {
    let events = vec![LoggedEvent {
        at_ms: now_ms() - (2 * 60 * 60 * 1000), // 2 hours ago
        kind: LoggedKind::PlayerFailed {
            reason: "sink-panic-budget".to_string(),
            restarts: 5,
        },
    }];
    let findings = findings_from(&events, now_ms());
    let player_findings: Vec<_> = findings
        .iter()
        .filter(|f| f.category == DoctorFindingCategory::Player)
        .collect();

    assert_eq!(player_findings.len(), 1);
    assert_eq!(player_findings[0].severity, DoctorFindingSeverity::Error);
    assert!(
        player_findings[0].message.contains("sink-panic-budget"),
        "msg: {}",
        player_findings[0].message
    );
}

#[test]
fn recent_session_disconnect_emits_warning_finding() {
    let events = vec![LoggedEvent {
        at_ms: now_ms() - 60_000, // 1 minute ago
        kind: LoggedKind::SessionDisconnected {
            reason: "session-invalid".to_string(),
        },
    }];
    let findings = findings_from(&events, now_ms());
    let player_findings: Vec<_> = findings
        .iter()
        .filter(|f| f.category == DoctorFindingCategory::Player)
        .collect();

    assert_eq!(player_findings.len(), 1);
    assert_eq!(player_findings[0].severity, DoctorFindingSeverity::Warning);
    assert!(
        player_findings[0]
            .message
            .to_lowercase()
            .contains("session"),
        "msg: {}",
        player_findings[0].message
    );
}

#[test]
fn old_session_disconnect_beyond_lookback_does_not_emit_finding() {
    // SessionDisconnected uses a rolling lookback (5 min) — once the
    // session recovers the warning shouldn't linger. Adversarial: this
    // is the rolling-window bug from spirc reconnect cycles.
    let events = vec![LoggedEvent {
        at_ms: now_ms() - (10 * 60 * 1000), // 10 minutes ago
        kind: LoggedKind::SessionDisconnected {
            reason: "x".to_string(),
        },
    }];
    let findings = findings_from(&events, now_ms());
    let player_findings: Vec<_> = findings
        .iter()
        .filter(|f| f.category == DoctorFindingCategory::Player)
        .collect();

    assert!(
        player_findings.is_empty(),
        "stale session-disconnect should not surface as a finding"
    );
}

#[test]
fn no_phase_9_events_means_no_phase_9_findings() {
    // Belt-and-braces: empty buffer must not invent player findings.
    let findings = findings_from(&[], now_ms());
    let player_findings: Vec<_> = findings
        .iter()
        .filter(|f| f.category == DoctorFindingCategory::Player)
        .collect();
    assert!(player_findings.is_empty());
}
