#![allow(clippy::panic, clippy::unwrap_used)]

//! Phase 6.9 — recent event log + doctor finding derivation.

use spotuify_protocol::{
    findings_from, AuthErrorKind, DaemonEvent, DoctorFindingCategory, DoctorFindingSeverity,
    EventLog, LoggedEvent, LoggedKind,
};

fn now_ms() -> i64 {
    1_700_000_000_000
}

#[test]
fn from_daemon_event_lifts_rate_limited() {
    let event = DaemonEvent::RateLimited {
        retry_after_secs: 30,
        scope: "GET /me/player".to_string(),
        provider: None,
    };
    let logged = LoggedEvent::from(&event, now_ms()).unwrap();
    assert!(matches!(
        logged.kind,
        LoggedKind::RateLimited {
            retry_after_secs: 30,
            ..
        }
    ));
}

#[test]
fn from_daemon_event_lifts_auth_error() {
    let event = DaemonEvent::AuthError {
        kind: AuthErrorKind::ExpiredRefresh,
        provider: None,
    };
    let logged = LoggedEvent::from(&event, now_ms()).unwrap();
    assert!(matches!(logged.kind, LoggedKind::AuthError { .. }));
}

#[test]
fn from_daemon_event_lifts_schema_compat() {
    let event = DaemonEvent::SchemaCompat {
        endpoint: "GET /tracks/x".to_string(),
        missing_keys: vec!["external_ids".into()],
    };
    let logged = LoggedEvent::from(&event, now_ms()).unwrap();
    assert!(matches!(logged.kind, LoggedKind::SchemaCompat { .. }));
}

#[test]
fn from_daemon_event_ignores_unrelated_variants() {
    let event = DaemonEvent::PlaybackChanged {
        action: "play".to_string(),
        playback: None,
    };
    assert!(LoggedEvent::from(&event, now_ms()).is_none());
}

#[test]
fn recent_rate_limit_within_lookback_emits_warning_finding() {
    let events = vec![LoggedEvent {
        at_ms: now_ms() - 60_000, // 1 minute ago
        kind: LoggedKind::RateLimited {
            retry_after_secs: 45,
            scope: "GET /me/player".to_string(),
        },
    }];
    let findings = findings_from(&events, now_ms());
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].category, DoctorFindingCategory::Network);
    assert_eq!(findings[0].severity, DoctorFindingSeverity::Warning);
    assert!(findings[0].message.contains("45s"));
    assert!(findings[0].message.contains("GET /me/player"));
}

#[test]
fn old_rate_limit_beyond_lookback_does_not_emit_finding() {
    let events = vec![LoggedEvent {
        at_ms: now_ms() - (10 * 60 * 1000), // 10 minutes ago
        kind: LoggedKind::RateLimited {
            retry_after_secs: 45,
            scope: "x".to_string(),
        },
    }];
    let findings = findings_from(&events, now_ms());
    assert!(findings.is_empty());
}

#[test]
fn auth_error_emits_error_finding() {
    let events = vec![LoggedEvent {
        at_ms: now_ms() - 1000,
        kind: LoggedKind::AuthError {
            kind_str: "ExpiredRefresh".to_string(),
        },
    }];
    let findings = findings_from(&events, now_ms());
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].category, DoctorFindingCategory::Auth);
    assert_eq!(findings[0].severity, DoctorFindingSeverity::Error);
    assert!(findings[0].message.to_lowercase().contains("login"));
}

#[test]
fn schema_compat_within_lookback_emits_info_finding() {
    let events = vec![LoggedEvent {
        at_ms: now_ms() - (10 * 60 * 1000), // 10 minutes ago
        kind: LoggedKind::SchemaCompat {
            endpoint: "GET /tracks/abc".to_string(),
            missing_keys: vec!["external_ids".into()],
        },
    }];
    let findings = findings_from(&events, now_ms());
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].severity, DoctorFindingSeverity::Info);
    assert!(findings[0].message.contains("/tracks/abc"));
}

#[test]
fn old_schema_compat_beyond_one_hour_does_not_emit_finding() {
    let events = vec![LoggedEvent {
        at_ms: now_ms() - (90 * 60 * 1000), // 1.5 hours ago
        kind: LoggedKind::SchemaCompat {
            endpoint: "x".to_string(),
            missing_keys: vec![],
        },
    }];
    let findings = findings_from(&events, now_ms());
    assert!(findings.is_empty());
}

#[test]
fn multiple_findings_concatenate_in_a_stable_order() {
    let events = vec![
        LoggedEvent {
            at_ms: now_ms() - 30_000,
            kind: LoggedKind::RateLimited {
                retry_after_secs: 5,
                scope: "s".to_string(),
            },
        },
        LoggedEvent {
            at_ms: now_ms() - 60_000,
            kind: LoggedKind::AuthError {
                kind_str: "ExpiredRefresh".into(),
            },
        },
        LoggedEvent {
            at_ms: now_ms() - 5 * 60_000,
            kind: LoggedKind::SchemaCompat {
                endpoint: "e".into(),
                missing_keys: vec![],
            },
        },
    ];
    let findings = findings_from(&events, now_ms());
    let categories: Vec<_> = findings.iter().map(|f| f.category).collect();
    assert_eq!(
        categories,
        vec![
            DoctorFindingCategory::Network,
            DoctorFindingCategory::Auth,
            DoctorFindingCategory::Network,
        ]
    );
}

// --- EventLog FIFO behaviour ---

#[test]
fn event_log_pushes_until_capacity() {
    let mut log = EventLog::new(3);
    for i in 0..3 {
        log.push(LoggedEvent {
            at_ms: i as i64,
            kind: LoggedKind::AuthError {
                kind_str: format!("e{i}"),
            },
        });
    }
    assert_eq!(log.len(), 3);
}

#[test]
fn event_log_drops_oldest_when_over_capacity() {
    let mut log = EventLog::new(2);
    for i in 0..5 {
        log.push(LoggedEvent {
            at_ms: i as i64,
            kind: LoggedKind::AuthError {
                kind_str: format!("e{i}"),
            },
        });
    }
    let snap = log.snapshot();
    assert_eq!(snap.len(), 2);
    assert_eq!(snap[0].at_ms, 3);
    assert_eq!(snap[1].at_ms, 4);
}

#[test]
fn event_log_starts_empty() {
    let log = EventLog::new(10);
    assert!(log.is_empty());
    assert_eq!(log.len(), 0);
}
