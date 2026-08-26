//! In-memory PlayerBackend for tests.
//!
//! Records every call and emits matching `PlayerEvent`s through a
//! channel the test owns. Daemon-level integration tests reach for this
//! via the `test-support` feature so they can drive playback paths
//! without touching a real provider.
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
    DeviceId, PlayerBackend, PlayerError, PlayerEvent, PlayerResult, ProviderId, RepeatMode,
    ResourceUri, UriScheme,
};
use spotuify_core::MediaKind;

/// Every PlayerBackend method invocation, captured in order for tests.
/// Variants carry the arguments so tests can assert exact dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordedCall {
    RegisterDevice(String),
    PlayUri { uri: ResourceUri, position_ms: u32 },
    Pause,
    Resume,
    Next,
    Previous,
    Seek(u32),
    Volume(u8),
    Shuffle(bool),
    Repeat(RepeatMode),
    PreloadUri(ResourceUri),
    QueueAdd(ResourceUri),
    Shutdown,
}

#[derive(Debug, Default)]
struct PrimedErrors {
    register_device: Option<PlayerError>,
    volume: Option<PlayerError>,
    play_uri: Option<PlayerError>,
    preload_uri: Option<PlayerError>,
}

pub struct MockPlayerBackend {
    provider_id: ProviderId,
    uri_scheme: UriScheme,
    events_tx: mpsc::UnboundedSender<PlayerEvent>,
    calls: Mutex<Vec<RecordedCall>>,
    state: Mutex<State>,
    primed: Mutex<PrimedErrors>,
}

#[derive(Debug, Default)]
struct State {
    registered: bool,
    /// Once set, every async transport method parks forever. Models a
    /// wedged backend (a librespot Spirc that stopped answering) so daemon
    /// tests can prove the caller gives up instead of hanging with it.
    stalled: bool,
    /// Answer podcast-rate changes like a backend that owns decoded audio.
    /// Off by default so the mock keeps the trait's `Unsupported` answer.
    accepts_podcast_speed: bool,
}

impl MockPlayerBackend {
    /// Construct the backend and the receiving end of its event
    /// channel. Tests drain the stream to assert event ordering.
    pub fn new() -> (Self, UnboundedReceiverStream<PlayerEvent>) {
        Self::new_for_provider(
            ProviderId::new("mock").expect("built-in provider id is valid"),
            UriScheme::new("mock").expect("built-in URI scheme is valid"),
        )
    }

    /// Construct a backend paired to an explicit provider registry entry.
    pub fn new_for_provider(
        provider_id: ProviderId,
        uri_scheme: UriScheme,
    ) -> (Self, UnboundedReceiverStream<PlayerEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let backend = Self {
            provider_id,
            uri_scheme,
            events_tx: tx,
            calls: Mutex::new(Vec::new()),
            state: Mutex::new(State::default()),
            primed: Mutex::new(PrimedErrors::default()),
        };
        (backend, UnboundedReceiverStream::new(rx))
    }

    /// Boxed variant for callers that want type-erasure (e.g. older
    /// tests using `Pin<Box<dyn Stream>>`).
    pub fn new_boxed() -> (Self, Pin<Box<dyn Stream<Item = PlayerEvent> + Send>>) {
        let (backend, stream) = Self::new();
        (backend, Box::pin(stream))
    }

    /// Snapshot of every recorded call so far, in invocation order.
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().clone()
    }

    /// Clone the test-only event sender so integration tests can queue a
    /// lifecycle event before the daemon takes ownership of the backend.
    pub fn event_sender(&self) -> mpsc::UnboundedSender<PlayerEvent> {
        self.events_tx.clone()
    }

    /// Make the next `volume()` call return the given error. Useful
    /// for verifying daemon error-path handling without spinning up a
    /// real provider failure.
    pub fn prime_volume_error(&mut self, err: PlayerError) {
        self.primed.lock().volume = Some(err);
    }

    pub fn prime_register_device_error(&mut self, err: PlayerError) {
        self.primed.lock().register_device = Some(err);
    }

    pub fn prime_play_uri_error(&mut self, err: PlayerError) {
        self.primed.lock().play_uri = Some(err);
    }

    pub fn prime_preload_uri_error(&mut self, err: PlayerError) {
        self.primed.lock().preload_uri = Some(err);
    }

    /// Wedge every subsequent async transport call. `register_device` still
    /// answers, so a test can bring the device up and only then jam it.
    pub fn stall_transport(&mut self) {
        self.state.lock().stalled = true;
    }

    /// Take podcast-rate changes instead of rejecting them, so a test can
    /// exercise the "the backend accepted it but is not the device playing"
    /// case that separates accepted from applied.
    pub fn accept_podcast_speed(&mut self) {
        self.state.lock().accepts_podcast_speed = true;
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

    /// Park forever when stalled. Reads the flag into a local first: a
    /// `parking_lot` guard is `!Send` and would be held across the await if
    /// the lock lived in the `if` condition.
    async fn maybe_stall(&self) {
        let stalled = self.state.lock().stalled;
        if stalled {
            std::future::pending::<()>().await;
        }
    }

    fn emit(&self, event: PlayerEvent) {
        let _ = self.events_tx.send(event);
    }

    fn ensure_owned_uri(&self, uri: &ResourceUri) -> PlayerResult<()> {
        if uri.scheme() == &self.uri_scheme {
            Ok(())
        } else {
            Err(PlayerError::InvalidArg(format!(
                "backend for `{}` cannot play resource `{uri}`",
                self.uri_scheme
            )))
        }
    }

    fn test_track_uri(&self, id: &str) -> ResourceUri {
        ResourceUri::new(self.uri_scheme.clone(), MediaKind::Track, id)
            .expect("mock track id is canonical")
    }
}

#[async_trait]
impl PlayerBackend for MockPlayerBackend {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn uri_scheme(&self) -> &UriScheme {
        &self.uri_scheme
    }

    fn set_podcast_speed(&mut self, _speed: f32) -> PlayerResult<()> {
        if self.state.lock().accepts_podcast_speed {
            Ok(())
        } else {
            Err(PlayerError::Unsupported("set_podcast_speed".to_string()))
        }
    }

    async fn register_device(&mut self, name: &str) -> PlayerResult<DeviceId> {
        self.record(RecordedCall::RegisterDevice(name.to_string()));
        if let Some(err) = self.primed.lock().register_device.take() {
            return Err(err);
        }
        let device_id = DeviceId::new(format!("mock-{name}"));
        self.state.lock().registered = true;
        self.emit(PlayerEvent::Ready {
            device_id: device_id.clone(),
            name: name.to_string(),
        });
        Ok(device_id)
    }

    async fn play_uri(&mut self, uri: &ResourceUri, position_ms: u32) -> PlayerResult<()> {
        self.record(RecordedCall::PlayUri {
            uri: uri.clone(),
            position_ms,
        });
        self.maybe_stall().await;
        if let Some(err) = self.primed.lock().play_uri.take() {
            return Err(err);
        }
        self.ensure_registered()?;
        self.ensure_owned_uri(uri)?;
        self.emit(PlayerEvent::PlaybackStarted {
            uri: uri.clone(),
            position_ms,
        });
        Ok(())
    }

    async fn pause(&mut self) -> PlayerResult<()> {
        self.record(RecordedCall::Pause);
        self.maybe_stall().await;
        self.ensure_registered()?;
        self.emit(PlayerEvent::PlaybackPaused);
        Ok(())
    }

    async fn resume(&mut self) -> PlayerResult<()> {
        self.record(RecordedCall::Resume);
        self.maybe_stall().await;
        self.ensure_registered()?;
        self.emit(PlayerEvent::PlaybackResumed);
        Ok(())
    }

    async fn next(&mut self) -> PlayerResult<()> {
        self.record(RecordedCall::Next);
        self.maybe_stall().await;
        self.ensure_registered()?;
        self.emit(PlayerEvent::TrackChanged {
            uri: self.test_track_uri("mock-next"),
            position_ms: 0,
        });
        Ok(())
    }

    async fn previous(&mut self) -> PlayerResult<()> {
        self.record(RecordedCall::Previous);
        self.maybe_stall().await;
        self.ensure_registered()?;
        self.emit(PlayerEvent::TrackChanged {
            uri: self.test_track_uri("mock-prev"),
            position_ms: 0,
        });
        Ok(())
    }

    async fn seek(&mut self, position_ms: u32) -> PlayerResult<()> {
        self.record(RecordedCall::Seek(position_ms));
        self.maybe_stall().await;
        self.ensure_registered()?;
        self.emit(PlayerEvent::PositionTick { position_ms });
        Ok(())
    }

    async fn volume(&mut self, percent: u8) -> PlayerResult<()> {
        self.record(RecordedCall::Volume(percent));
        self.maybe_stall().await;
        if let Some(err) = self.primed.lock().volume.take() {
            return Err(err);
        }
        self.ensure_registered()?;
        Ok(())
    }

    async fn shuffle(&mut self, _on: bool) -> PlayerResult<()> {
        self.record(RecordedCall::Shuffle(_on));
        self.maybe_stall().await;
        self.ensure_registered()?;
        Ok(())
    }

    async fn repeat(&mut self, mode: RepeatMode) -> PlayerResult<()> {
        self.record(RecordedCall::Repeat(mode));
        self.maybe_stall().await;
        self.ensure_registered()?;
        Ok(())
    }

    async fn preload_uri(&mut self, uri: &ResourceUri) -> PlayerResult<()> {
        self.record(RecordedCall::PreloadUri(uri.clone()));
        self.maybe_stall().await;
        if let Some(err) = self.primed.lock().preload_uri.take() {
            return Err(err);
        }
        self.ensure_registered()?;
        self.ensure_owned_uri(uri)
    }

    async fn queue_add(&mut self, uri: &ResourceUri) -> PlayerResult<()> {
        self.record(RecordedCall::QueueAdd(uri.clone()));
        self.maybe_stall().await;
        self.ensure_registered()?;
        self.ensure_owned_uri(uri)
    }

    async fn is_connected(&self) -> bool {
        self.state.lock().registered
    }

    async fn shutdown(&mut self) -> PlayerResult<()> {
        self.record(RecordedCall::Shutdown);
        self.state.lock().registered = false;
        Ok(())
    }
}
