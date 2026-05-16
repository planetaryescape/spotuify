//! Phase 9.0 — MockPlayerBackend contract tests.
//!
//! The mock is the test double daemon-level tests reach for. These
//! tests guarantee its observable behaviour: every method call is
//! recorded, every successful command emits a matching `PlayerEvent`,
//! and errors plumbed through a script knob actually fire.
//!
//! Adversarial focus: assert event stream order, not just "last call".
//! A tautological version would inspect a `last_uri` field and never
//! catch a regression where two methods are swapped.

#![cfg(feature = "test-support")]

use std::time::Duration;

use futures::StreamExt;
use spotuify_player::backends::mock::{MockPlayerBackend, RecordedCall};
use spotuify_player::{BackendKind, DeviceId, PlayerBackend, PlayerError, PlayerEvent, RepeatMode};

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("test runtime should build")
}

fn ready_event(event: &PlayerEvent) -> Option<(&DeviceId, &str)> {
    match event {
        PlayerEvent::Ready { device_id, name } => Some((device_id, name)),
        _ => None,
    }
}

fn playback_started_uri(event: &PlayerEvent) -> Option<&str> {
    match event {
        PlayerEvent::PlaybackStarted { uri, .. } => Some(uri),
        _ => None,
    }
}

fn position_tick(event: &PlayerEvent) -> Option<u32> {
    match event {
        PlayerEvent::PositionTick { position_ms } => Some(*position_ms),
        _ => None,
    }
}

fn collect_events_with_timeout<S>(mut stream: S, n: usize) -> Vec<PlayerEvent>
where
    S: futures::Stream<Item = PlayerEvent> + Unpin,
{
    let runtime = runtime();

    runtime.block_on(async move {
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let event = tokio::time::timeout(Duration::from_millis(500), stream.next())
                .await
                .expect("expected event within 500ms")
                .expect("stream closed prematurely");
            out.push(event);
        }
        out
    })
}

#[test]
fn kind_is_visible_for_diagnostics() {
    // Adversarial: doctor surfaces backend.kind(). Without this getter
    // the diagnostics report would be empty even when a backend is
    // registered.
    let (backend, _events) = MockPlayerBackend::new();
    assert_eq!(backend.kind(), BackendKind::Embedded);
}

#[test]
fn register_device_emits_ready_and_returns_id() {
    let (mut backend, events) = MockPlayerBackend::new();

    let runtime = runtime();
    let device_id = runtime
        .block_on(backend.register_device("spotuify-test"))
        .expect("register_device should succeed");

    assert_eq!(device_id, DeviceId::new("mock-spotuify-test"));

    let collected = collect_events_with_timeout(events, 1);
    let (device_id, name) = ready_event(&collected[0]).expect("expected Ready event");
    assert_eq!(device_id.as_str(), "mock-spotuify-test");
    assert_eq!(name, "spotuify-test");
}

#[test]
fn scripted_sequence_produces_matching_event_stream_in_order() {
    let (mut backend, events) = MockPlayerBackend::new();
    let runtime = runtime();

    runtime.block_on(async {
        backend
            .register_device("spotuify")
            .await
            .expect("register_device should succeed");
        backend
            .play_uri("spotify:track:abc", 0)
            .await
            .expect("play_uri should succeed");
        backend.pause().await.expect("pause should succeed");
        backend.seek(15_000).await.expect("seek should succeed");
        backend.resume().await.expect("resume should succeed");
        backend.next().await.expect("next should succeed");
        backend.shutdown().await.expect("shutdown should succeed");
    });

    let collected = collect_events_with_timeout(events, 6);

    // Adversarial: lock the order, not just the set. Catches the bug
    // where pause and resume are accidentally swapped.
    assert!(
        matches!(collected[0], PlayerEvent::Ready { .. }),
        "got {:?}",
        collected[0]
    );
    assert_eq!(
        playback_started_uri(&collected[1]).expect("expected PlaybackStarted event"),
        "spotify:track:abc"
    );
    assert!(matches!(collected[2], PlayerEvent::PlaybackPaused));
    assert_eq!(
        position_tick(&collected[3]).expect("expected PositionTick from seek"),
        15_000
    );
    assert!(matches!(collected[4], PlayerEvent::PlaybackResumed));
    assert!(matches!(collected[5], PlayerEvent::TrackChanged { .. }));
}

#[test]
fn recorded_calls_capture_every_invocation_in_order() {
    let (mut backend, _events) = MockPlayerBackend::new();
    let runtime = runtime();

    runtime.block_on(async {
        backend
            .register_device("spotuify")
            .await
            .expect("register_device should succeed");
        backend.volume(50).await.expect("volume should succeed");
        backend.shuffle(true).await.expect("shuffle should succeed");
        backend
            .repeat(RepeatMode::Track)
            .await
            .expect("repeat should succeed");
        backend
            .preload_uri("spotify:track:warm")
            .await
            .expect("preload should succeed");
        backend.previous().await.expect("previous should succeed");
    });

    let calls = backend.calls();
    // Adversarial: assert the full sequence + arguments. A test that
    // only checked `calls.len() == 5` would pass even if every method
    // recorded "Pause" — useless.
    assert_eq!(
        calls,
        vec![
            RecordedCall::RegisterDevice("spotuify".to_string()),
            RecordedCall::Volume(50),
            RecordedCall::Shuffle(true),
            RecordedCall::Repeat(RepeatMode::Track),
            RecordedCall::PreloadUri("spotify:track:warm".to_string()),
            RecordedCall::Previous,
        ]
    );
}

#[test]
fn primed_error_for_volume_propagates_no_active_device() {
    let (mut backend, _events) = MockPlayerBackend::new();
    backend.prime_volume_error(PlayerError::NoActiveDevice);
    let runtime = runtime();

    let result = runtime.block_on(backend.volume(40));

    assert!(
        matches!(result, Err(PlayerError::NoActiveDevice)),
        "got {result:?}"
    );
    // Adversarial: the call must still be recorded so daemon tests can
    // assert the command was attempted before failing.
    assert!(matches!(
        backend.calls().last(),
        Some(RecordedCall::Volume(40))
    ));
}

#[test]
fn commands_before_register_device_return_not_initialised() {
    // Adversarial: a daemon bug where playback commands race ahead of
    // register_device should surface a typed error, not silently
    // succeed against an unwired backend.
    let (mut backend, _events) = MockPlayerBackend::new();
    let runtime = runtime();

    let result = runtime.block_on(backend.play_uri("spotify:track:abc", 0));
    assert!(matches!(result, Err(PlayerError::NotInitialised)));
}

#[test]
fn shutdown_clears_state_and_blocks_further_commands() {
    let (mut backend, _events) = MockPlayerBackend::new();
    let runtime = runtime();

    runtime.block_on(async {
        backend
            .register_device("spotuify")
            .await
            .expect("register_device should succeed");
        backend.shutdown().await.expect("shutdown should succeed");
        let after = backend.play_uri("spotify:track:abc", 0).await;
        assert!(matches!(after, Err(PlayerError::NotInitialised)));
    });
}

#[test]
fn is_connected_flips_with_register_and_shutdown() {
    let (mut backend, _events) = MockPlayerBackend::new();
    let runtime = runtime();

    runtime.block_on(async {
        assert!(!backend.is_connected().await);
        backend
            .register_device("spotuify")
            .await
            .expect("register_device should succeed");
        assert!(backend.is_connected().await);
        backend.shutdown().await.expect("shutdown should succeed");
        assert!(!backend.is_connected().await);
    });
}

#[test]
fn web_api_token_is_none_by_default_and_settable_for_token_bridge_tests() {
    let (mut backend, _events) = MockPlayerBackend::new();
    let runtime = runtime();
    assert!(runtime.block_on(backend.web_api_token()).is_none());

    backend.set_web_api_token(Some("fake-token-xyz".to_string()));
    assert_eq!(
        runtime.block_on(backend.web_api_token()),
        Some("fake-token-xyz".to_string())
    );
}
