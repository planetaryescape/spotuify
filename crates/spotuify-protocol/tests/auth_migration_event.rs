#![allow(clippy::panic, clippy::unwrap_used)]

//! Wire contract for `DaemonEvent::AuthMigrationRecommended`, the proactive
//! advisory the daemon emits when it resolves to first-party-only Spotify
//! auth. The macOS client hand-decodes this shape, so the tag key
//! (`event`), the kebab-case tag value, and the snake_case `can_login_dev_app`
//! field name are a load-bearing contract — assert them exactly.

use spotuify_protocol::DaemonEvent;

#[test]
fn auth_migration_recommended_wire_shape_is_kebab_case_and_tagged() {
    let raw = serde_json::to_string(&DaemonEvent::AuthMigrationRecommended {
        can_login_dev_app: true,
    })
    .unwrap();

    assert!(
        raw.contains("\"event\":\"auth-migration-recommended\""),
        "raw: {raw}"
    );
    assert!(raw.contains("\"can_login_dev_app\":true"), "raw: {raw}");
}

#[test]
fn auth_migration_recommended_round_trips_true() {
    let original = DaemonEvent::AuthMigrationRecommended {
        can_login_dev_app: true,
    };
    let raw = serde_json::to_string(&original).unwrap();
    let decoded: DaemonEvent = serde_json::from_str(&raw).unwrap();

    match decoded {
        DaemonEvent::AuthMigrationRecommended { can_login_dev_app } => {
            assert!(can_login_dev_app);
        }
        other => panic!("expected AuthMigrationRecommended, got {other:?}"),
    }
}

#[test]
fn auth_migration_recommended_round_trips_false() {
    let original = DaemonEvent::AuthMigrationRecommended {
        can_login_dev_app: false,
    };
    let raw = serde_json::to_string(&original).unwrap();
    let decoded: DaemonEvent = serde_json::from_str(&raw).unwrap();

    match decoded {
        DaemonEvent::AuthMigrationRecommended { can_login_dev_app } => {
            assert!(!can_login_dev_app);
        }
        other => panic!("expected AuthMigrationRecommended, got {other:?}"),
    }
}

#[test]
fn unknown_can_login_dev_app_flag_defaults_are_not_invented() {
    // A future daemon might drop the field; today it is always present, so
    // decoding an object missing it must fail rather than silently invent a
    // migration recommendation. Guards the client banner against a garbled
    // advisory.
    let raw = "{\"event\":\"auth-migration-recommended\"}";
    let decoded: Result<DaemonEvent, _> = serde_json::from_str(raw);
    assert!(decoded.is_err(), "decoded unexpectedly: {decoded:?}");
}
