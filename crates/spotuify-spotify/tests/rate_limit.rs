//! Phase 6.3 — rate-limit middleware tests.
//!
//! Focus on the testable pure-function core: retry decisions, backoff
//! math, persistent budget state. The reqwest-bound RateLimitedClient
//! integration would need wiremock and is deferred until the daemon's
//! SpotifyClient gets migrated to consume it.

use std::time::Duration;

use chrono::{TimeZone, Utc};
use rand::SeedableRng;
use spotuify_spotify::error::SpotifyError;
use spotuify_spotify::rate_limit::{
    decide_retry, jittered_backoff, BackoffState, RetryAction, BACKOFF_BASE_MS, BACKOFF_CEILING_MS,
    MAX_TRANSIENT_RETRIES,
};

fn now() -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap()
}

fn seeded_rng() -> rand::rngs::StdRng {
    rand::rngs::StdRng::seed_from_u64(42)
}

#[test]
fn test_200_yields_success() {
    let mut rng = seeded_rng();
    let action = decide_retry(0, 200, None, "GET /me", "", now(), &mut rng);
    assert_eq!(action, RetryAction::Success);
}

#[test]
fn test_304_yields_success_not_modified() {
    let mut rng = seeded_rng();
    let action = decide_retry(0, 304, None, "GET /me", "", now(), &mut rng);
    assert_eq!(action, RetryAction::Success);
}

#[test]
fn test_429_yields_retry_with_header_value() {
    let mut rng = seeded_rng();
    let action = decide_retry(0, 429, Some("5"), "GET /me", "", now(), &mut rng);
    match action {
        RetryAction::Retry { delay } => assert_eq!(delay, Duration::from_secs(5)),
        other => panic!("expected Retry, got {other:?}"),
    }
}

#[test]
fn test_429_without_retry_after_defaults_to_60s() {
    let mut rng = seeded_rng();
    let action = decide_retry(0, 429, None, "GET /me", "", now(), &mut rng);
    match action {
        RetryAction::Retry { delay } => assert_eq!(delay, Duration::from_secs(60)),
        other => panic!("expected Retry, got {other:?}"),
    }
}

#[test]
fn test_429_clamps_to_ceiling_one_hour() {
    let mut rng = seeded_rng();
    let action = decide_retry(0, 429, Some("999999"), "GET /me", "", now(), &mut rng);
    match action {
        RetryAction::Retry { delay } => assert_eq!(delay, Duration::from_secs(3600)),
        other => panic!("expected Retry, got {other:?}"),
    }
}

#[test]
fn test_401_yields_give_up_auth_expired_not_retry() {
    let mut rng = seeded_rng();
    let action = decide_retry(0, 401, None, "GET /me", "", now(), &mut rng);
    match action {
        RetryAction::GiveUp(SpotifyError::AuthExpired) => {}
        other => panic!("expected GiveUp(AuthExpired), got {other:?}"),
    }
}

#[test]
fn test_5xx_first_attempts_retry_with_exponential_backoff() {
    let mut rng = seeded_rng();
    // attempt 0 = first attempt just made; retry should fire (becoming attempt 1)
    let action = decide_retry(0, 502, None, "GET /me", "", now(), &mut rng);
    match action {
        RetryAction::Retry { delay } => {
            // base is 250ms, jitter ±25%
            assert!(
                delay.as_millis() >= 180 && delay.as_millis() <= 320,
                "first-attempt delay {delay:?} should be ~250ms ± 25%"
            );
        }
        other => panic!("expected Retry, got {other:?}"),
    }
}

#[test]
fn test_5xx_second_retry_doubles_backoff_base() {
    let mut rng = seeded_rng();
    // attempt 1 (already retried once); delay should ~= 500ms ± 25%
    let action = decide_retry(1, 503, None, "GET /me", "", now(), &mut rng);
    match action {
        RetryAction::Retry { delay } => {
            assert!(
                delay.as_millis() >= 370 && delay.as_millis() <= 640,
                "second-retry delay {delay:?} should be ~500ms ± 25%"
            );
        }
        other => panic!("expected Retry, got {other:?}"),
    }
}

#[test]
fn test_5xx_after_max_attempts_yields_give_up_api_error() {
    let mut rng = seeded_rng();
    let action = decide_retry(
        MAX_TRANSIENT_RETRIES - 1, // already retried twice; the "next" decision is to give up
        500,
        None,
        "GET /me",
        r#"{"error":{"status":500,"message":"server error"}}"#,
        now(),
        &mut rng,
    );
    match action {
        RetryAction::GiveUp(SpotifyError::Api {
            status, message, ..
        }) => {
            assert_eq!(status, 500);
            assert_eq!(message, "server error");
        }
        other => panic!("expected GiveUp(Api), got {other:?}"),
    }
}

#[test]
fn test_404_yields_give_up_not_found_no_retry() {
    let mut rng = seeded_rng();
    let action = decide_retry(0, 404, None, "GET /playlists/x", "", now(), &mut rng);
    match action {
        RetryAction::GiveUp(SpotifyError::NotFound) => {}
        other => panic!("expected GiveUp(NotFound), got {other:?}"),
    }
}

#[test]
fn test_jittered_backoff_doubles_per_attempt_within_jitter_bounds() {
    let mut rng = seeded_rng();
    let d0 = jittered_backoff(0, &mut rng).as_millis();
    let d1 = jittered_backoff(1, &mut rng).as_millis();
    let d2 = jittered_backoff(2, &mut rng).as_millis();
    // Each successive attempt should be at least 50% larger after jitter
    // (worst case: prev * 1.25 vs next * 0.75 -> next/prev >= 1.2)
    assert!(
        d1 > d0 || (d1 as f64 / d0 as f64) > 1.2,
        "d1 {d1} not larger than d0 {d0}"
    );
    assert!(
        d2 > d1 || (d2 as f64 / d1 as f64) > 1.2,
        "d2 {d2} not larger than d1 {d1}"
    );
}

#[test]
fn test_jittered_backoff_clamps_at_ceiling() {
    let mut rng = seeded_rng();
    // attempt 20 = base * 2^20 = 250ms * 1M = 250 seconds -- way over ceiling
    let d = jittered_backoff(20, &mut rng);
    assert!(
        d.as_millis() <= BACKOFF_CEILING_MS as u128,
        "backoff {d:?} exceeded ceiling {BACKOFF_CEILING_MS}ms"
    );
}

#[test]
fn test_jittered_backoff_base_attempt_zero_is_about_base_ms() {
    let mut rng = seeded_rng();
    let d = jittered_backoff(0, &mut rng);
    let lower = (BACKOFF_BASE_MS as f64 * 0.75) as u128;
    let upper = (BACKOFF_BASE_MS as f64 * 1.25) as u128;
    assert!(
        d.as_millis() >= lower && d.as_millis() <= upper,
        "attempt-0 backoff {d:?} outside [{lower}, {upper}]ms"
    );
}

// --- Backoff state persistence ---

#[test]
fn test_backoff_state_default_has_no_wait() {
    let state = BackoffState::default();
    assert_eq!(state.wait_ms("any", 1_000_000), 0);
}

#[test]
fn test_record_rate_limit_pushes_next_eligible_forward() {
    let mut state = BackoffState::default();
    let now_ms = 1_000_000_000_000;
    state.record_rate_limit("GET /me", now_ms, Duration::from_secs(5));
    assert_eq!(state.wait_ms("GET /me", now_ms), 5000);
    assert_eq!(state.wait_ms("GET /me", now_ms + 3000), 2000);
    assert_eq!(state.wait_ms("GET /me", now_ms + 5500), 0);
}

#[test]
fn test_clear_resets_eligibility_for_scope() {
    let mut state = BackoffState::default();
    let now_ms = 1_000_000_000_000;
    state.record_rate_limit("scope", now_ms, Duration::from_secs(60));
    state.clear("scope");
    assert_eq!(state.wait_ms("scope", now_ms), 0);
}

#[test]
fn test_backoff_state_persists_across_save_and_load() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    drop(tmp);

    let mut state = BackoffState::default();
    state.record_rate_limit("scope-a", 1_000_000, Duration::from_secs(30));
    state.save(&path).unwrap();

    let loaded = BackoffState::load(&path);
    assert_eq!(loaded.wait_ms("scope-a", 1_000_000), 30_000);
    assert_eq!(loaded.wait_ms("scope-a", 1_030_001), 0);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_load_from_missing_path_returns_default_state() {
    let path = std::path::PathBuf::from("/tmp/nonexistent-rate-limit-state-xyz789.json");
    let loaded = BackoffState::load(&path);
    assert!(loaded.scopes.is_empty());
}
