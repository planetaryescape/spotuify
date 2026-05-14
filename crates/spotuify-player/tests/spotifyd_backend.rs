//! Phase 9.1 — SpotifydBackend contract tests.
//!
//! SpotifydBackend wraps the legacy `spotuify_spotify::spotifyd` helper
//! and routes playback commands through the Web API. These tests pin
//! the wiring: kind() reports Spotifyd, register_device emits Ready,
//! playback delegates to Web API, autostart=false doesn't blow up
//! when spotifyd isn't installed.
//!
//! We avoid spawning real spotifyd processes by toggling autostart off
//! and asserting the subprocess path is not invoked.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use spotuify_player::backends::connect_only::StaticTokenProvider;
use spotuify_player::backends::spotifyd::{SpotifydBackend, SpotifydSettings};
use spotuify_player::{BackendKind, PlayerBackend, PlayerEvent};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn build(
    server: &MockServer,
    autostart: bool,
) -> (
    SpotifydBackend,
    tokio_stream::wrappers::UnboundedReceiverStream<PlayerEvent>,
) {
    let token = Arc::new(StaticTokenProvider::new("test-token"));
    SpotifydBackend::with_settings(
        server.uri(),
        token,
        SpotifydSettings {
            autostart,
            // A path that can't exist — guards against accidentally
            // spawning spotifyd if autostart slipped through.
            spotifyd_config_path: std::path::PathBuf::from("/dev/null/no-such-config"),
        },
    )
}

#[tokio::test]
async fn kind_reports_spotifyd() {
    let server = MockServer::start().await;
    let (backend, _events) = build(&server, false);
    assert_eq!(backend.kind(), BackendKind::Spotifyd);
}

#[tokio::test]
async fn register_device_with_autostart_disabled_emits_ready() {
    // Adversarial: even when autostart=false (user runs spotifyd
    // themselves, e.g. via launchd), register_device must still
    // succeed. A regression here would brick the spotifyd backend for
    // anyone using a sibling supervisor.
    let server = MockServer::start().await;
    let (mut backend, mut events) = build(&server, false);
    let id = backend.register_device("listening-room").await.unwrap();

    assert!(
        id.as_str().contains("listening-room"),
        "device id should embed the requested name, got {id}"
    );
    let evt = tokio::time::timeout(Duration::from_millis(500), events.next())
        .await
        .expect("event timeout")
        .expect("stream closed");
    assert!(matches!(evt, PlayerEvent::Ready { .. }), "got {evt:?}");
}

#[tokio::test]
async fn play_uri_delegates_to_web_api() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/me/player/play"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let (mut backend, _events) = build(&server, false);
    backend.register_device("x").await.unwrap();
    backend
        .play_uri("spotify:track:abc", 0)
        .await
        .expect("play_uri must delegate to Web API");

    let calls = server.received_requests().await.unwrap();
    assert!(
        calls.iter().any(|r| r.url.path() == "/v1/me/player/play"),
        "expected /v1/me/player/play call, got {:?}",
        calls
            .iter()
            .map(|r| r.url.path().to_string())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn is_connected_flips_with_register_and_shutdown() {
    let server = MockServer::start().await;
    let (mut backend, _events) = build(&server, false);
    assert!(!backend.is_connected().await);
    backend.register_device("x").await.unwrap();
    assert!(backend.is_connected().await);
    backend.shutdown().await.unwrap();
    assert!(!backend.is_connected().await);
}
