//! In-memory PlayerBackend for tests.
//!
//! Records every call and emits matching `PlayerEvent`s through a
//! channel the test owns. Daemon-level integration tests reach for this
//! via the `test-support` feature so they can drive playback paths
//! without touching real Spotify.
//!
//! Design choices:
//! - Calls accumulate in a `Vec<RecordedCall>` rather than a "last
//!   call" field so tests can assert full sequences. Catches the bug
//!   where two methods are swapped — a last-call field would miss it.
//! - Errors are primed per-method via `prime_*_error` setters; nothing
//!   is faked by default so happy-path tests stay readable.
//! - `is_connected` flips with `register_device`/`shutdown` so daemon
//!   tests can assert the device-lifecycle invariant without poking at
//!   private state.

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::{
    BackendKind, DeviceId, PlayerBackend, PlayerError, PlayerEvent, PlayerResult, RepeatMode,
};

/// Every PlayerBackend method invocation, captured in order for tests.
/// Variants carry the arguments so tests can assert exact dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordedCall {
    RegisterDevice(String),
    PlayUri { uri: String, position_ms: u32 },
    Pause,
    Resume,
    Next,
    Previous,
    Seek(u32),
    Volume(u8),
    Shuffle(bool),
    Repeat(RepeatMode),
    Shutdown,
}

#[derive(Debug, Default)]
struct PrimedErrors {
    volume: Option<PlayerError>,
    play_uri: Option<PlayerError>,
}

pub struct MockPlayerBackend {
    events_tx: mpsc::UnboundedSender<PlayerEvent>,
    calls: Mutex<Vec<RecordedCall>>,
    state: Mutex<State>,
    primed: Mutex<PrimedErrors>,
}

#[derive(Debug, Default)]
struct State {
    registered: bool,
    web_api_token: Option<String>,
}

impl MockPlayerBackend {
    /// Construct the backend and the receiving end of its event
    /// channel. Tests drain the stream to assert event ordering.
    pub fn new() -> (Self, Pin<Box<dyn Stream<Item = PlayerEvent> + Send>>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let backend = Self {
            events_tx: tx,
            calls: Mutex::new(Vec::new()),
            state: Mutex::new(State::default()),
            primed: Mutex::new(PrimedErrors::default()),
        };
        let stream = Box::pin(UnboundedReceiverStream::new(rx));
        (backend, stream)
    }

    /// Snapshot of every recorded call so far, in invocation order.
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().clone()
    }

    /// Inject a one-shot token so token-bridge tests can verify the
    /// daemon prefers the backend's token over the keyring fallback.
    pub fn set_web_api_token(&mut self, token: Option<String>) {
        self.state.lock().web_api_token = token;
    }

    /// Make the next `volume()` call return the given error. Useful
    /// for verifying daemon error-path handling without spinning up a
    /// real Spotify failure.
    pub fn prime_volume_error(&mut self, err: PlayerError) {
        self.primed.lock().volume = Some(err);
    }

    pub fn prime_play_uri_error(&mut self, err: PlayerError) {
        self.primed.lock().play_uri = Some(err);
    }

    fn record(&self, call: RecordedCall) {
        self.calls.lock().push(call);
    }

    fn ensure_registered(&self) -> PlayerResult<()> {
        if self.state.lock().registered {
            Ok(())
        } else {
            Err(PlayerError::NotInitialised)
        }
    }

    fn emit(&self, event: PlayerEvent) {
        let _ = self.events_tx.send(event);
    }
}

#[async_trait]
impl PlayerBackend for MockPlayerBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Spotifyd
    }

    async fn register_device(&mut self, name: &str) -> PlayerResult<DeviceId> {
        self.record(RecordedCall::RegisterDevice(name.to_string()));
        let device_id = DeviceId::new(format!("mock-{name}"));
        self.state.lock().registered = true;
        self.emit(PlayerEvent::Ready {
            device_id: device_id.clone(),
            name: name.to_string(),
        });
        Ok(device_id)
    }

    async fn play_uri(&mut self, uri: &str, position_ms: u32) -> PlayerResult<()> {
        self.record(RecordedCall::PlayUri {
            uri: uri.to_string(),
            position_ms,
        });
        if let Some(err) = self.primed.lock().play_uri.take() {
            return Err(err);
        }
        self.ensure_registered()?;
        self.emit(PlayerEvent::PlaybackStarted {
            uri: uri.to_string(),
            position_ms,
        });
        Ok(())
    }

    async fn pause(&mut self) -> PlayerResult<()> {
        self.record(RecordedCall::Pause);
        self.ensure_registered()?;
        self.emit(PlayerEvent::PlaybackPaused);
        Ok(())
    }

    async fn resume(&mut self) -> PlayerResult<()> {
        self.record(RecordedCall::Resume);
        self.ensure_registered()?;
        self.emit(PlayerEvent::PlaybackResumed);
        Ok(())
    }

    async fn next(&mut self) -> PlayerResult<()> {
        self.record(RecordedCall::Next);
        self.ensure_registered()?;
        self.emit(PlayerEvent::TrackChanged {
            uri: "spotify:track:mock-next".to_string(),
            position_ms: 0,
        });
        Ok(())
    }

    async fn previous(&mut self) -> PlayerResult<()> {
        self.record(RecordedCall::Previous);
        self.ensure_registered()?;
        self.emit(PlayerEvent::TrackChanged {
            uri: "spotify:track:mock-prev".to_string(),
            position_ms: 0,
        });
        Ok(())
    }

    async fn seek(&mut self, position_ms: u32) -> PlayerResult<()> {
        self.record(RecordedCall::Seek(position_ms));
        self.ensure_registered()?;
        self.emit(PlayerEvent::PositionTick { position_ms });
        Ok(())
    }

    async fn volume(&mut self, percent: u8) -> PlayerResult<()> {
        self.record(RecordedCall::Volume(percent));
        if let Some(err) = self.primed.lock().volume.take() {
            return Err(err);
        }
        self.ensure_registered()?;
        Ok(())
    }

    async fn shuffle(&mut self, _on: bool) -> PlayerResult<()> {
        self.record(RecordedCall::Shuffle(_on));
        self.ensure_registered()?;
        Ok(())
    }

    async fn repeat(&mut self, mode: RepeatMode) -> PlayerResult<()> {
        self.record(RecordedCall::Repeat(mode));
        self.ensure_registered()?;
        Ok(())
    }

    async fn is_connected(&self) -> bool {
        self.state.lock().registered
    }

    async fn web_api_token(&self) -> Option<String> {
        self.state.lock().web_api_token.clone()
    }

    async fn shutdown(&mut self) -> PlayerResult<()> {
        self.record(RecordedCall::Shutdown);
        self.state.lock().registered = false;
        Ok(())
    }
}
