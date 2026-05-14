//! Phase 6.8 — token refresh scheduling.

use std::time::Duration;

use spotuify_spotify::refresh_planner::{next_refresh_in, should_refresh, PROACTIVE_HEADROOM};

#[test]
fn unset_expires_at_triggers_refresh() {
    assert!(should_refresh(1_700_000_000, 0, PROACTIVE_HEADROOM));
}

#[test]
fn already_expired_triggers_refresh() {
    assert!(should_refresh(
        1_700_000_000,
        1_699_999_999,
        PROACTIVE_HEADROOM
    ));
}

#[test]
fn within_headroom_triggers_proactive_refresh() {
    // 30 seconds remaining; headroom is 60s -> proactive refresh.
    assert!(should_refresh(
        1_700_000_000,
        1_700_000_030,
        PROACTIVE_HEADROOM
    ));
}

#[test]
fn beyond_headroom_does_not_refresh() {
    // 600 seconds remaining; well above headroom.
    assert!(!should_refresh(
        1_700_000_000,
        1_700_000_600,
        PROACTIVE_HEADROOM
    ));
}

#[test]
fn exactly_at_headroom_boundary_refreshes() {
    // remaining = headroom exactly -> refresh (<=, inclusive).
    assert!(should_refresh(
        1_700_000_000,
        1_700_000_060,
        PROACTIVE_HEADROOM
    ));
}

#[test]
fn next_refresh_in_returns_none_when_refresh_due() {
    assert_eq!(next_refresh_in(1_700_000_000, 0, PROACTIVE_HEADROOM), None);
    assert_eq!(
        next_refresh_in(1_700_000_000, 1_700_000_030, PROACTIVE_HEADROOM),
        None
    );
}

#[test]
fn next_refresh_in_computes_target_minus_headroom() {
    // expires_at = now + 200s, headroom = 60s -> refresh in 140s.
    let d = next_refresh_in(1_700_000_000, 1_700_000_200, PROACTIVE_HEADROOM).unwrap();
    assert_eq!(d, Duration::from_secs(140));
}

#[test]
fn next_refresh_in_handles_long_lived_token() {
    // Spotify access tokens are 1h; headroom 60s -> ~3540s.
    let d = next_refresh_in(1_700_000_000, 1_700_003_600, PROACTIVE_HEADROOM).unwrap();
    assert_eq!(d, Duration::from_secs(3540));
}

#[test]
fn custom_headroom_respected() {
    // 200s headroom; refresh threshold moves earlier.
    let h = Duration::from_secs(200);
    assert!(should_refresh(1_700_000_000, 1_700_000_150, h));
    assert!(!should_refresh(1_700_000_000, 1_700_000_300, h));
}
