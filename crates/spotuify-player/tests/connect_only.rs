//! Phase 9.0 — ConnectOnlyBackend wiremock contract tests.
//!
//! ConnectOnly is the "no local audio" backend: it remote-controls
//! whatever Spotify Connect device the user has active via the Web API.
//! The adversarial cases below pin the request shapes Spotify expects
//! and lock in error mapping for the three failures users hit most
//! often (Premium gate, no active device, expired token).

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use spotuify_player::backends::connect_only::{ConnectOnlyBackend, StaticTokenProvider};
use spotuify_player::{BackendKind, PlayerBackend, PlayerError, PlayerEvent, RepeatMode};
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn build_backend(
    server: &MockServer,
) -> (
    ConnectOnlyBackend,
    tokio_stream::wrappers::UnboundedReceiverStream<PlayerEvent>,
) {
    let token = Arc::new(StaticTokenProvider::new("test-token"));
    let (backend, stream) = ConnectOnlyBackend::with_base_url(server.uri(), token);
    (backend, stream)
}

async fn drain_one(
    stream: &mut tokio_stream::wrappers::UnboundedReceiverStream<PlayerEvent>,
) -> PlayerEvent {
    tokio::time::timeout(Duration::from_millis(500), stream.next())
        .await
        .expect("event timeout")
        .expect("stream closed")
}

#[tokio::test]
async fn kind_reports_connect() {
    let server = MockServer::start().await;
    let (backend, _events) = build_backend(&server);
    assert_eq!(backend.kind(), BackendKind::Connect);
}

#[tokio::test]
async fn register_device_emits_ready_and_does_not_hit_the_api() {
    // Adversarial: a Free-tier user must be able to `register_device`
    // without any Web API call. If we made an HTTP request here a 401
    // (no token) would surface as a startup error.
    let server = MockServer::start().await;
    let (mut backend, mut events) = build_backend(&server);

    let id = backend.register_device("listening-room").await.unwrap();
    assert!(
        id.as_str().starts_with("connect-only-"),
        "device id should be a synthetic placeholder, got {id}"
    );

    let event = drain_one(&mut events).await;
    match event {
        PlayerEvent::Ready { name, .. } => assert_eq!(name, "listening-room"),
        other => panic!("expected Ready, got {other:?}"),
    }

    // No HTTP calls should have been made.
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "register_device must not hit Spotify",
    );
}

#[tokio::test]
async fn play_uri_puts_correct_json_body() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/me/player/play"))
        .and(body_json(serde_json::json!({
            "uris": ["spotify:track:abc"],
            "position_ms": 12345_u64,
        })))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let (mut backend, _events) = build_backend(&server);
    backend.register_device("x").await.unwrap();
    backend
        .play_uri("spotify:track:abc", 12345)
        .await
        .expect("play_uri must succeed against a 204");

    let calls = server.received_requests().await.unwrap();
    assert!(calls.iter().any(|r| r.url.path() == "/v1/me/player/play"));
}

#[tokio::test]
async fn play_uri_403_maps_to_premium_required() {
    // Adversarial: Free accounts hit 403 here. The user-facing error
    // message must explain *why*, so the daemon can route it to the
    // PremiumRequired banner instead of a generic auth error.
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/me/player/play"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let (mut backend, _events) = build_backend(&server);
    backend.register_device("x").await.unwrap();
    let err = backend
        .play_uri("spotify:track:abc", 0)
        .await
        .expect_err("403 must propagate");

    assert!(
        matches!(err, PlayerError::PremiumRequired),
        "got {err:?}; expected PremiumRequired"
    );
}

#[tokio::test]
async fn play_uri_404_maps_to_no_active_device() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/me/player/play"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let (mut backend, _events) = build_backend(&server);
    backend.register_device("x").await.unwrap();
    let err = backend
        .play_uri("spotify:track:abc", 0)
        .await
        .expect_err("404 must propagate");
    assert!(
        matches!(err, PlayerError::NoActiveDevice),
        "got {err:?}; expected NoActiveDevice"
    );
}

#[tokio::test]
async fn play_uri_401_maps_to_auth() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/me/player/play"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let (mut backend, _events) = build_backend(&server);
    backend.register_device("x").await.unwrap();
    let err = backend
        .play_uri("spotify:track:abc", 0)
        .await
        .expect_err("401 must propagate");
    assert!(
        matches!(err, PlayerError::Auth(_)),
        "got {err:?}; expected Auth"
    );
}

#[tokio::test]
async fn pause_resume_next_previous_hit_documented_endpoints() {
    let server = MockServer::start().await;
    for (verb, route) in &[
        ("PUT", "/v1/me/player/pause"),
        ("PUT", "/v1/me/player/play"), // resume re-uses play
        ("POST", "/v1/me/player/next"),
        ("POST", "/v1/me/player/previous"),
    ] {
        Mock::given(method(*verb))
            .and(path(*route))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
    }

    let (mut backend, _events) = build_backend(&server);
    backend.register_device("x").await.unwrap();
    backend.pause().await.unwrap();
    backend.resume().await.unwrap();
    backend.next().await.unwrap();
    backend.previous().await.unwrap();

    let calls = server.received_requests().await.unwrap();
    let visited: Vec<_> = calls.iter().map(|r| r.url.path().to_string()).collect();
    // Adversarial: assert each endpoint was actually reached. A typo
    // like /player/skip vs /player/next would silently 404 in prod;
    // a per-endpoint mock + receipt check makes that loud here.
    assert!(
        visited.iter().any(|p| p == "/v1/me/player/pause"),
        "{visited:?}"
    );
    assert!(
        visited.iter().any(|p| p == "/v1/me/player/play"),
        "{visited:?}"
    );
    assert!(
        visited.iter().any(|p| p == "/v1/me/player/next"),
        "{visited:?}"
    );
    assert!(
        visited.iter().any(|p| p == "/v1/me/player/previous"),
        "{visited:?}"
    );
}

#[tokio::test]
async fn seek_volume_shuffle_repeat_send_correct_query_params() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/me/player/seek"))
        .and(query_param("position_ms", "30000"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/v1/me/player/volume"))
        .and(query_param("volume_percent", "42"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/v1/me/player/shuffle"))
        .and(query_param("state", "true"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/v1/me/player/repeat"))
        .and(query_param("state", "track"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let (mut backend, _events) = build_backend(&server);
    backend.register_device("x").await.unwrap();
    backend.seek(30_000).await.unwrap();
    backend.volume(42).await.unwrap();
    backend.shuffle(true).await.unwrap();
    backend.repeat(RepeatMode::Track).await.unwrap();
}

#[tokio::test]
async fn volume_above_100_is_rejected_locally_without_an_http_call() {
    // Adversarial: Spotify accepts 0-100. A bug that sends 150 will
    // 400 from Spotify but cost a round-trip + a confusing error.
    // Local validation catches it cheaper.
    let server = MockServer::start().await;
    let (mut backend, _events) = build_backend(&server);
    backend.register_device("x").await.unwrap();
    let err = backend
        .volume(150)
        .await
        .expect_err("volume>100 must error");
    assert!(matches!(err, PlayerError::InvalidArg(_)), "got {err:?}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn slow_response_triggers_bounded_timeout() {
    // Bounded timeout: ConnectOnly must not let a hung Spotify API hang
    // the daemon. 5s is the doc-suggested ceiling for command-style
    // calls.
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/me/player/play"))
        .respond_with(ResponseTemplate::new(204).set_delay(Duration::from_secs(10)))
        .mount(&server)
        .await;

    let (mut backend, _events) = build_backend(&server);
    backend.register_device("x").await.unwrap();
    let err = backend
        .play_uri("spotify:track:abc", 0)
        .await
        .expect_err("hanging response must time out");
    assert!(matches!(err, PlayerError::Timeout(_)), "got {err:?}");
}

#[tokio::test]
async fn shutdown_is_a_noop_and_clears_state() {
    let server = MockServer::start().await;
    let (mut backend, _events) = build_backend(&server);
    backend.register_device("x").await.unwrap();
    assert!(backend.is_connected().await);
    backend.shutdown().await.unwrap();
    assert!(!backend.is_connected().await);
}

#[tokio::test]
async fn missing_token_returns_typed_auth_error() {
    // Adversarial: if no token is available (user hasn't logged in
    // yet), we must surface Auth — not network or generic.
    let server = MockServer::start().await;
    let token = Arc::new(StaticTokenProvider::missing());
    let (mut backend, _events) = ConnectOnlyBackend::with_base_url(server.uri(), token);
    backend.register_device("x").await.unwrap();
    let err = backend
        .play_uri("spotify:track:abc", 0)
        .await
        .expect_err("missing token must error");
    assert!(matches!(err, PlayerError::Auth(_)), "got {err:?}");
}
