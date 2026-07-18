#![allow(clippy::panic, clippy::unwrap_used)]

//! Phase 6.1 — typed `SpotifyError` classifier tests.
//!
//! Adversarial coverage: every test injects a Spotify-flavoured response
//! shape and asserts on the typed variant produced. No HTTP is involved —
//! the classifier takes pre-extracted (status, header, body) tuples.
//!
//! Test names describe observable behaviour, not implementation. Deleting
//! the implementation should fail every test (no tautology, no sycophancy).

use std::time::Duration;

use chrono::{TimeZone, Utc};
use spotuify_spotify::error::{
    classify_response, parse_retry_after, SpotifyError, DEFAULT_RETRY_AFTER_SECS,
    MAX_RETRY_AFTER_SECS,
};

fn now() -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap()
}

fn expect_rate_limited(err: SpotifyError) -> Result<(Duration, String), SpotifyError> {
    match err {
        SpotifyError::RateLimited { retry_after, scope } => Ok((retry_after, scope)),
        other => Err(other),
    }
}

fn expect_forbidden_scope(err: SpotifyError) -> Result<String, SpotifyError> {
    match err {
        SpotifyError::Forbidden { scope } => Ok(scope),
        other => Err(other),
    }
}

fn expect_deprecated_endpoint(err: SpotifyError) -> Result<&'static str, SpotifyError> {
    match err {
        SpotifyError::Deprecated { endpoint } => Ok(endpoint),
        other => Err(other),
    }
}

fn expect_api_error(err: SpotifyError) -> Result<(u16, String, String), SpotifyError> {
    match err {
        SpotifyError::Api {
            status,
            endpoint,
            message,
            ..
        } => Ok((status, endpoint, message)),
        other => Err(other),
    }
}

#[test]
fn test_429_with_retry_after_yields_rate_limited() {
    let err = classify_response(429, Some("7"), "GET /me/player", "", now());
    let (retry_after, scope) = expect_rate_limited(err).expect("response should be rate limited");
    assert_eq!(retry_after, Duration::from_secs(7));
    assert_eq!(scope, "GET /me/player");
}

#[test]
fn test_429_without_retry_after_defaults_to_60s_per_rfc9110() {
    let err = classify_response(429, None, "GET /me/player/recently-played", "", now());
    let (retry_after, _) = expect_rate_limited(err).expect("response should be rate limited");
    assert_eq!(retry_after, Duration::from_secs(DEFAULT_RETRY_AFTER_SECS));
}

#[test]
fn test_429_with_http_date_retry_after_parses_correctly() {
    // "now" is 2026-05-13 12:00:00 UTC. The header says wait until +30s.
    let when = Utc
        .with_ymd_and_hms(2026, 5, 13, 12, 0, 30)
        .unwrap()
        .to_rfc2822();
    let err = classify_response(429, Some(&when), "GET /playlists/x", "", now());
    let (retry_after, _) = expect_rate_limited(err).expect("response should be rate limited");
    assert!(
        (retry_after.as_secs() as i64 - 30).abs() <= 1,
        "expected ~30s, got {retry_after:?}"
    );
}

#[test]
fn test_429_with_excessive_retry_after_clamps_to_ceiling() {
    let err = classify_response(429, Some("999999"), "GET /me/player", "", now());
    let (retry_after, _) = expect_rate_limited(err).expect("response should be rate limited");
    assert_eq!(retry_after, Duration::from_secs(MAX_RETRY_AFTER_SECS));
}

#[test]
fn test_429_with_malformed_retry_after_falls_back_to_default() {
    let err = classify_response(429, Some("¯\\_(ツ)_/¯"), "GET /me/player", "", now());
    let (retry_after, _) = expect_rate_limited(err).expect("response should be rate limited");
    assert_eq!(retry_after, Duration::from_secs(DEFAULT_RETRY_AFTER_SECS));
}

#[test]
fn test_401_yields_auth_expired() {
    let body = r#"{"error":{"status":401,"message":"The access token expired"}}"#;
    let err = classify_response(401, None, "GET /me", body, now());
    assert!(matches!(err, SpotifyError::AuthExpired));
}

#[test]
fn test_403_yields_forbidden_with_scope_when_message_mentions_scope() {
    let body = r#"{"error":{"status":403,"message":"Insufficient client scope"}}"#;
    let err = classify_response(403, None, "PUT /me/player", body, now());
    let scope = expect_forbidden_scope(err).expect("response should be forbidden");
    assert!(
        scope.to_lowercase().contains("scope"),
        "got scope {scope:?}"
    );
}

#[test]
fn test_403_without_scope_message_surfaces_spotify_body() {
    // A 403 that *isn't* a scope failure (e.g. "Premium required",
    // "Restriction violated", "Player command failed") should NOT
    // be relabelled as a scope problem — that sent users on a
    // re-auth chase that fixed nothing. Surface Spotify's actual
    // message via the `Api` variant instead.
    let body = r#"{"error":{"status":403,"message":"Premium required"}}"#;
    let err = classify_response(403, None, "PUT /me/player", body, now());
    let (status, _, message) = expect_api_error(err).expect("non-scope 403 should be API error");
    assert_eq!(status, 403);
    assert_eq!(message, "Premium required");
}

#[test]
fn test_404_yields_not_found() {
    let err = classify_response(404, None, "GET /playlists/missing", "", now());
    assert!(matches!(err, SpotifyError::NotFound));
}

#[test]
fn test_410_for_deprecated_endpoint_yields_deprecated_variant() {
    let err = classify_response(410, None, "GET /recommendations", "", now());
    assert_eq!(
        expect_deprecated_endpoint(err).expect("response should be deprecated"),
        "recommendations"
    );
}

#[test]
fn test_410_for_audio_features_yields_deprecated_audio_features() {
    let err = classify_response(410, None, "GET /audio-features/abc", "", now());
    assert_eq!(
        expect_deprecated_endpoint(err).expect("response should be deprecated"),
        "audio-features"
    );
}

#[test]
fn test_500_yields_api_error_with_status() {
    let body = r#"{"error":{"status":500,"message":"server error"}}"#;
    let err = classify_response(500, None, "GET /playlists/x", body, now());
    let (status, endpoint, message) = expect_api_error(err).expect("response should be API error");
    assert_eq!(status, 500);
    assert_eq!(endpoint, "GET /playlists/x");
    assert_eq!(message, "server error");
}

#[test]
fn test_502_504_are_retryable() {
    for status in [500, 502, 503, 504] {
        let err = classify_response(status, None, "GET /any", "", now());
        assert!(err.is_retryable(), "status {status} should be retryable");
    }
}

#[test]
fn test_400_404_not_retryable() {
    for status in [400, 404, 422] {
        let err = classify_response(status, None, "GET /any", "", now());
        assert!(
            !err.is_retryable(),
            "status {status} should not be retryable"
        );
    }
}

#[test]
fn test_429_and_401_are_retryable() {
    let r1 = classify_response(429, Some("1"), "x", "", now());
    let r2 = classify_response(401, None, "x", "", now());
    assert!(r1.is_retryable());
    assert!(r2.is_retryable());
}

#[test]
fn test_spotify_error_to_ipc_error_kind_preserves_classification() {
    use spotuify_protocol::IpcErrorKind;

    let cases: Vec<(SpotifyError, IpcErrorKind)> = vec![
        (
            SpotifyError::RateLimited {
                retry_after: Duration::from_secs(1),
                scope: "x".to_string(),
            },
            IpcErrorKind::RateLimited,
        ),
        (SpotifyError::AuthRequired, IpcErrorKind::Auth),
        (SpotifyError::AuthExpired, IpcErrorKind::Auth),
        (SpotifyError::AuthRevoked, IpcErrorKind::AuthRevoked),
        (
            SpotifyError::Forbidden {
                scope: "x".to_string(),
            },
            IpcErrorKind::Auth,
        ),
        (SpotifyError::NotFound, IpcErrorKind::Provider),
        (
            SpotifyError::Deprecated {
                endpoint: "recommendations",
            },
            IpcErrorKind::Provider,
        ),
        (
            SpotifyError::InvalidInput {
                message: "bad URI".to_string(),
            },
            IpcErrorKind::InvalidRequest,
        ),
        (
            SpotifyError::Network {
                endpoint: "x".to_string(),
                message: "x".to_string(),
            },
            IpcErrorKind::Network,
        ),
        (
            SpotifyError::Decode {
                endpoint: "x".to_string(),
                message: "x".to_string(),
            },
            IpcErrorKind::Provider,
        ),
        (
            SpotifyError::Api {
                status: 502,
                endpoint: "x".to_string(),
                message: "x".to_string(),
                body: String::new(),
            },
            IpcErrorKind::Provider,
        ),
    ];

    for (err, want) in cases {
        let got = err.ipc_kind();
        assert_eq!(got, want, "mismatch for {err:?}");
    }
}

#[test]
fn parse_retry_after_none_yields_default() {
    let d = parse_retry_after(None, now());
    assert_eq!(d, Duration::from_secs(DEFAULT_RETRY_AFTER_SECS));
}

#[test]
fn parse_retry_after_zero_yields_zero() {
    let d = parse_retry_after(Some("0"), now());
    assert_eq!(d, Duration::ZERO);
}

#[test]
fn parse_retry_after_past_http_date_yields_zero() {
    // header time is 30s in the past relative to `now()` — wait 0s
    let when = Utc
        .with_ymd_and_hms(2026, 5, 13, 11, 59, 30)
        .unwrap()
        .to_rfc2822();
    let d = parse_retry_after(Some(&when), now());
    assert_eq!(d, Duration::ZERO);
}

#[test]
fn spotify_error_extracts_message_from_body() {
    use spotuify_spotify::error::spotify_error_message;
    let body = r#"{"error":{"status":401,"message":"hello"}}"#;
    assert_eq!(spotify_error_message(body), "hello");
}

#[test]
fn spotify_error_message_empty_when_body_not_json() {
    use spotuify_spotify::error::spotify_error_message;
    assert_eq!(spotify_error_message("Service Unavailable"), "");
}
