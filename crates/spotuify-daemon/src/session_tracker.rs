//! Phase 10 — listening-session tracker.
//!
//! Subscribes to the player's `PlayerEvent` stream and maintains a
//! per-session state machine (Idle → Playing → Paused → …). At each
//! finalisation point it builds a `ListenFact`, applies the
//! qualification rule, and writes to the analytics store.
//!
//! Pass 1 (F11): scaffold only. Holds the state struct and exposes
//! `observe()` so the player-event worker can fan events in. Pass 2
//! (P10.1) fills in transitions, sink-tap reads, and emit paths.
//!
//! The tracker is owned by `DaemonState`; one instance per daemon
//! lifetime, lock-free observe via tokio::sync::Mutex on the state
//! field. Heavy lifting (qualification math, fact insertion) is async
//! so we never block the player-event worker.

use std::sync::Arc;

use spotuify_core::{
    qualify_listen, BackendLabel, ListenFact, MeasurementKind, PlaybackSource, SkipReason,
};
use spotuify_player::backends::audio_counter_tap::AudioCounterHandle;
use spotuify_player::PlayerEvent;
use spotuify_protocol::DaemonEvent;
use spotuify_store::{PlaybackProgressSample, Store};
use tokio::sync::{broadcast, Mutex};

/// Per-track session bookkeeping. `Idle` means no track is loaded;
/// `Playing` and `Paused` track the URI + accumulated audible time
/// so the finalize step can compute `audible_ms` correctly even when
/// the user pauses and resumes mid-track multiple times.
#[derive(Debug, Clone)]
pub enum SessionState {
    Idle,
    Playing {
        session_id: String,
        uri: String,
        started_at_ms: i64,
        last_position_ms: u32,
        accumulated_paused_ms: i64,
        /// Sink-tap `audible_ms()` sampled when this session began. The
        /// finalize delta against the live counter is sink-accurate
        /// audible time (network stalls / buffer drops don't advance it).
        /// 0 when no embedded counter is wired (wall-clock fallback).
        audible_baseline_ms: u64,
        /// Playback context (playlist/album/artist URI) the track started
        /// from, captured at session start for playlist-level analytics.
        context_uri: Option<String>,
        #[allow(dead_code)]
        source: PlaybackSource,
        #[allow(dead_code)]
        backend: BackendLabel,
        #[allow(dead_code)]
        private_session: bool,
    },
    Paused {
        session_id: String,
        uri: String,
        started_at_ms: i64,
        paused_at_ms: i64,
        last_position_ms: u32,
        accumulated_paused_ms: i64,
        audible_baseline_ms: u64,
        context_uri: Option<String>,
        #[allow(dead_code)]
        source: PlaybackSource,
        #[allow(dead_code)]
        backend: BackendLabel,
        #[allow(dead_code)]
        private_session: bool,
    },
}

impl SessionState {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Playing { .. } => "playing",
            Self::Paused { .. } => "paused",
        }
    }
}

/// Phase 10 session tracker. Owns the per-track state machine, builds
/// `ListenFact` rows at finalisation time, applies the qualification
/// rule, and emits `DaemonEvent::ListenQualified` for downstream
/// surfaces (TUI toast, shell hook, scrobblers).
pub struct SessionTracker {
    state: Mutex<SessionState>,
    /// Optional store handle. When `None` (e.g. test harness), the
    /// tracker still maintains state machine transitions but skips
    /// the listen_facts write. Production wiring passes a real Store.
    store: Option<Arc<Store>>,
    /// PCM counter exposed by embedded playback. When unavailable the
    /// tracker falls back to bounded wall-clock audible time.
    audio_counter: parking_lot::RwLock<Option<Arc<AudioCounterHandle>>>,
    /// Daemon event broadcast — used for `ListenQualified` emission.
    event_tx: Option<broadcast::Sender<spotuify_protocol::IpcMessage>>,
    /// Current playback context (playlist/album/artist URI), set by the
    /// daemon when a play-with-context command runs and captured into the
    /// session at the next `PlaybackStarted`. `None` for context-less play.
    current_context: parking_lot::Mutex<Option<String>>,
}

impl Default for SessionTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionTracker {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(SessionState::Idle),
            store: None,
            audio_counter: parking_lot::RwLock::new(None),
            event_tx: None,
            current_context: parking_lot::Mutex::new(None),
        }
    }

    /// Live sink-tap audible time, or 0 when no counter is wired.
    fn audible_now(&self) -> u64 {
        self.audio_counter
            .read()
            .as_ref()
            .map_or(0, |counter| counter.audible_ms())
    }

    /// Set the playback context (playlist/album/artist URI) the next
    /// started track plays from. The daemon calls this when a
    /// play-with-context command runs; `None` clears it (context-less play).
    pub fn set_current_context(&self, context_uri: Option<String>) {
        *self.current_context.lock() = context_uri;
    }

    #[cfg(test)]
    pub(crate) fn current_context(&self) -> Option<String> {
        self.current_context.lock().clone()
    }

    /// Construct with a store + event broadcast. Production callers
    /// use this; tests use [`Self::new`] when they don't care about
    /// persistence side-effects.
    pub fn with_store(
        store: Arc<Store>,
        event_tx: broadcast::Sender<spotuify_protocol::IpcMessage>,
        audio_counter: Option<Arc<AudioCounterHandle>>,
    ) -> Self {
        Self::with_store_and_audio_counter(store, event_tx, audio_counter)
    }

    pub fn with_store_and_audio_counter(
        store: Arc<Store>,
        event_tx: broadcast::Sender<spotuify_protocol::IpcMessage>,
        audio_counter: Option<Arc<AudioCounterHandle>>,
    ) -> Self {
        Self {
            state: Mutex::new(SessionState::Idle),
            store: Some(store),
            audio_counter: parking_lot::RwLock::new(audio_counter),
            event_tx: Some(event_tx),
            current_context: parking_lot::Mutex::new(None),
        }
    }

    pub(crate) fn set_audio_counter(&self, counter: Option<Arc<AudioCounterHandle>>) {
        *self.audio_counter.write() = counter;
    }

    /// Observe a `PlayerEvent`. Foundation pass: log + advance the
    /// state machine label only. Feature pass (P10.1) fills in
    /// finalize / qualification / emit_listen_qualified.
    pub async fn observe(self: &Arc<Self>, event: &PlayerEvent) {
        let mut state = self.state.lock().await;
        match event {
            PlayerEvent::PlaybackStarted { uri, position_ms } => {
                *state = SessionState::Playing {
                    session_id: new_session_id(),
                    uri: uri.as_uri(),
                    started_at_ms: spotuify_core::now_ms(),
                    last_position_ms: *position_ms,
                    accumulated_paused_ms: 0,
                    audible_baseline_ms: self.audible_now(),
                    context_uri: self.current_context.lock().clone(),
                    source: PlaybackSource::Unknown,
                    backend: BackendLabel::Embedded,
                    private_session: false,
                };
            }
            PlayerEvent::PlaybackPaused => {
                if let SessionState::Playing {
                    session_id,
                    uri,
                    started_at_ms,
                    last_position_ms,
                    accumulated_paused_ms,
                    audible_baseline_ms,
                    context_uri,
                    source,
                    backend,
                    private_session,
                } = &*state
                {
                    *state = SessionState::Paused {
                        session_id: session_id.clone(),
                        uri: uri.clone(),
                        started_at_ms: *started_at_ms,
                        paused_at_ms: spotuify_core::now_ms(),
                        last_position_ms: *last_position_ms,
                        accumulated_paused_ms: *accumulated_paused_ms,
                        audible_baseline_ms: *audible_baseline_ms,
                        context_uri: context_uri.clone(),
                        source: *source,
                        backend: *backend,
                        private_session: *private_session,
                    };
                }
            }
            PlayerEvent::PlaybackResumed => {
                if let SessionState::Paused {
                    session_id,
                    uri,
                    started_at_ms,
                    paused_at_ms,
                    last_position_ms,
                    accumulated_paused_ms,
                    audible_baseline_ms,
                    context_uri,
                    source,
                    backend,
                    private_session,
                } = &*state
                {
                    let pause_delta = spotuify_core::now_ms().saturating_sub(*paused_at_ms);
                    *state = SessionState::Playing {
                        session_id: session_id.clone(),
                        uri: uri.clone(),
                        started_at_ms: *started_at_ms,
                        last_position_ms: *last_position_ms,
                        accumulated_paused_ms: accumulated_paused_ms.saturating_add(pause_delta),
                        audible_baseline_ms: *audible_baseline_ms,
                        context_uri: context_uri.clone(),
                        source: *source,
                        backend: *backend,
                        private_session: *private_session,
                    };
                }
            }
            PlayerEvent::PositionTick { position_ms } => {
                let sample = if let SessionState::Playing {
                    session_id,
                    uri,
                    last_position_ms,
                    ..
                } = &mut *state
                {
                    *last_position_ms = *position_ms;
                    Some((session_id.clone(), uri.clone(), *position_ms))
                } else {
                    None
                };
                drop(state);
                if let (Some(store), Some((session_id, uri, position_ms))) =
                    (self.store.clone(), sample)
                {
                    let audio_counter = self.audio_counter.read().clone();
                    tokio::spawn(async move {
                        let (audible_samples, sample_rate, channels) = audio_counter
                            .as_ref()
                            .map(|counter| {
                                let snapshot = counter.snapshot();
                                (
                                    snapshot.samples as i64,
                                    snapshot.sample_rate as i64,
                                    snapshot.channels as i64,
                                )
                            })
                            .unwrap_or_else(|| (position_ms as i64, 1000, 1));
                        let _ = store
                            .insert_playback_progress_sample(&PlaybackProgressSample {
                                session_id: &session_id,
                                track_uri: &uri,
                                sampled_at_ms: spotuify_core::now_ms(),
                                position_ms: position_ms as i64,
                                audible_samples,
                                sample_rate,
                                channels,
                            })
                            .await;
                    });
                }
            }
            PlayerEvent::TrackChanged { .. } => {
                let snapshot = std::mem::replace(&mut *state, SessionState::Idle);
                drop(state);
                self.spawn_finalize(snapshot, SkipReason::UserNext);
            }
            PlayerEvent::EndOfTrack { .. } => {
                let snapshot = std::mem::replace(&mut *state, SessionState::Idle);
                drop(state);
                self.spawn_finalize(snapshot, SkipReason::TrackEnd);
            }
            PlayerEvent::SessionDisconnected { .. } => {
                // Blueprint: never qualify a track when the session
                // dies mid-play, regardless of accumulated audible time.
                let snapshot = std::mem::replace(&mut *state, SessionState::Idle);
                drop(state);
                self.spawn_finalize(snapshot, SkipReason::SessionDied);
            }
            _ => {}
        }
    }

    /// Spawn [`Self::finalize`] in a background task. The forwarder
    /// task must stay non-blocking — a synchronous finalize at every
    /// track boundary holds the forwarder while SQLite writes to
    /// `listen_facts` and `track_metrics`, blocking the next
    /// `PlayerEvent` (including the next `PlaybackStarted`). Spawning
    /// detaches the write so the forwarder returns immediately.
    fn spawn_finalize(self: &Arc<Self>, snapshot: SessionState, reason: SkipReason) {
        let tracker = self.clone();
        tokio::spawn(async move {
            tracker.finalize(snapshot, reason).await;
        });
    }

    /// Build a `ListenFact` from the captured session state, apply the
    /// qualification rule, persist if a store is wired, and emit the
    /// `ListenQualified` event when applicable. `session_died` forces
    /// `qualified = false` regardless of audible time accrued.
    /// Build a `ListenFact` from a captured `SessionState`, apply the
    /// qualification rule, persist it (if a store is wired), and emit
    /// the `ListenQualified` event when applicable. Exposed for
    /// external integration tests that need to assert post-conditions
    /// against deterministic state — production callers go through
    /// `observe()`.
    pub async fn finalize(&self, snapshot: SessionState, reason: SkipReason) {
        let (
            session_id,
            uri,
            started_at_ms,
            last_position_ms,
            accumulated_paused_ms,
            audible_baseline_ms,
            context_uri,
            _src,
            _backend,
            private_session,
        ) = match snapshot {
            SessionState::Idle => return,
            SessionState::Playing {
                session_id,
                uri,
                started_at_ms,
                last_position_ms,
                accumulated_paused_ms,
                audible_baseline_ms,
                context_uri,
                source,
                backend,
                private_session,
                ..
            }
            | SessionState::Paused {
                session_id,
                uri,
                started_at_ms,
                last_position_ms,
                accumulated_paused_ms,
                audible_baseline_ms,
                context_uri,
                source,
                backend,
                private_session,
                ..
            } => (
                session_id,
                uri,
                started_at_ms,
                last_position_ms,
                accumulated_paused_ms,
                audible_baseline_ms,
                context_uri,
                source,
                backend,
                private_session,
            ),
        };

        let ended_at_ms = spotuify_core::now_ms();
        let elapsed_ms = ended_at_ms.saturating_sub(started_at_ms).max(0);
        // Wall-clock fallback: elapsed minus accumulated paused intervals.
        let wall_clock_audible_ms = elapsed_ms.saturating_sub(accumulated_paused_ms).max(0);
        // Prefer the sink tap when an embedded counter is wired: the delta
        // against the session baseline is audible time from real written
        // PCM, so network stalls / buffer drops count as less audible time.
        // Guard against a mid-session counter reset (current < baseline) and
        // a zero reading by falling back to wall-clock.
        let audio_counter = self.audio_counter.read().clone();
        let audible_ms = match audio_counter.as_ref() {
            Some(counter) => {
                let current = counter.audible_ms();
                let tap_delta = current.saturating_sub(audible_baseline_ms);
                if current >= audible_baseline_ms && tap_delta > 0 {
                    (tap_delta as i64).min(elapsed_ms.max(0))
                } else {
                    wall_clock_audible_ms
                }
            }
            None => wall_clock_audible_ms,
        };
        let store = self.store.as_ref();
        let duration_ms = track_duration_ms(store, &uri)
            .await
            .filter(|duration| *duration > 0)
            .unwrap_or(last_position_ms as i64);

        let qualification = qualify_listen(duration_ms, audible_ms);
        let qualified = qualification.qualified && reason != SkipReason::SessionDied;
        let completion_ratio = if duration_ms > 0 {
            (audible_ms as f64 / duration_ms as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let (artist_uri, album_uri) = listen_context_uris(store, &uri).await;

        let fact = ListenFact {
            id: None,
            session_id,
            track_uri: uri.clone(),
            artist_uri: artist_uri.clone(),
            album_uri: album_uri.clone(),
            context_uri,
            started_at_ms,
            ended_at_ms,
            duration_ms,
            elapsed_ms,
            audible_ms,
            completion_ratio,
            qualified,
            qualification_rule_version: spotuify_core::QUALIFICATION_RULE_VERSION,
            skip_reason: Some(reason),
            source: Some(PlaybackSource::Unknown),
            backend: Some(BackendLabel::Embedded),
            private_session,
            measurement_kind: MeasurementKind::ObservedPlayback,
            external_scrobble_id: None,
            created_at_ms: ended_at_ms,
        };

        if let Some(store) = store {
            let _ = store.insert_listen_fact(&fact).await;
            let _ = store
                .upsert_track_metric(&uri, qualified, audible_ms, ended_at_ms)
                .await;
            if let Some(artist_uri) = artist_uri.as_deref() {
                let _ = store
                    .upsert_artist_metric(artist_uri, qualified, audible_ms, ended_at_ms)
                    .await;
            }
            if let Some(album_uri) = album_uri.as_deref() {
                let _ = store
                    .upsert_album_metric(album_uri, qualified, audible_ms, ended_at_ms)
                    .await;
            }
        }

        if qualified && !private_session {
            if let Some(tx) = self.event_tx.as_ref() {
                let _ = tx.send(spotuify_protocol::IpcMessage {
                    id: 0,
                    source: None,
                    mutation_id: None,
                    payload: spotuify_protocol::IpcPayload::Event(DaemonEvent::ListenQualified {
                        track_uri: uri,
                        duration_ms,
                        audible_ms,
                        artist_uri,
                        album_uri,
                    }),
                });
            }
        }
    }

    /// Test hook: read the current state label.
    pub async fn current_state(&self) -> &'static str {
        self.state.lock().await.label()
    }
}

async fn track_duration_ms(store: Option<&Arc<Store>>, uri: &str) -> Option<i64> {
    let store = store?;
    let items = store.media_items_by_uris(&[uri.to_string()]).await.ok()?;
    items
        .into_iter()
        .find(|item| item.uri == uri)
        .map(|item| item.duration_ms as i64)
}

async fn listen_context_uris(
    store: Option<&Arc<Store>>,
    uri: &str,
) -> (Option<String>, Option<String>) {
    let Some(store) = store else {
        return (None, None);
    };
    store.listen_context_uris(uri).await.unwrap_or((None, None))
}

fn new_session_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use spotuify_core::ResourceUri;

    fn started(uri: &str) -> PlayerEvent {
        PlayerEvent::PlaybackStarted {
            uri: ResourceUri::parse(uri).expect("test URI must be valid"),
            position_ms: 0,
        }
    }

    #[tokio::test]
    async fn fresh_tracker_is_idle() {
        let t = Arc::new(SessionTracker::new());
        assert_eq!(t.current_state().await, "idle");
    }

    #[tokio::test]
    async fn playback_started_transitions_to_playing() {
        let t = Arc::new(SessionTracker::new());
        t.observe(&started("spotify:track:1")).await;
        assert_eq!(t.current_state().await, "playing");
    }

    #[tokio::test]
    async fn pause_then_resume_round_trips_through_paused() {
        let t = Arc::new(SessionTracker::new());
        t.observe(&started("spotify:track:1")).await;
        t.observe(&PlayerEvent::PlaybackPaused).await;
        assert_eq!(t.current_state().await, "paused");
        t.observe(&PlayerEvent::PlaybackResumed).await;
        assert_eq!(t.current_state().await, "playing");
    }

    #[tokio::test]
    async fn end_of_track_drops_back_to_idle() {
        let t = Arc::new(SessionTracker::new());
        t.observe(&started("spotify:track:1")).await;
        t.observe(&PlayerEvent::EndOfTrack {
            uri: ResourceUri::parse("spotify:track:1").unwrap(),
        })
        .await;
        assert_eq!(t.current_state().await, "idle");
    }

    #[tokio::test]
    async fn session_disconnected_drops_to_idle() {
        let t = Arc::new(SessionTracker::new());
        t.observe(&started("spotify:track:1")).await;
        t.observe(&PlayerEvent::SessionDisconnected {
            reason: "AirPods unpaired".to_string(),
        })
        .await;
        assert_eq!(t.current_state().await, "idle");
    }

    #[tokio::test]
    async fn finalize_uses_injected_audio_counter_over_wall_clock() {
        let store = Arc::new(Store::in_memory().await.expect("store"));
        let (tx, _rx) = broadcast::channel(8);
        let counter = AudioCounterHandle::new();
        counter.set_format(1_000, 2);
        counter.reset();
        counter.add_samples(10_000);
        let t = SessionTracker::with_store_and_audio_counter(store.clone(), tx, Some(counter));
        let snapshot = SessionState::Playing {
            session_id: "audio-counter-test".to_string(),
            uri: "spotify:track:counter".to_string(),
            started_at_ms: spotuify_core::now_ms().saturating_sub(60_000),
            last_position_ms: 60_000,
            accumulated_paused_ms: 0,
            audible_baseline_ms: 0,
            context_uri: None,
            source: PlaybackSource::Unknown,
            backend: BackendLabel::Embedded,
            private_session: false,
        };

        t.finalize(snapshot, SkipReason::UserNext).await;

        let audible_ms: i64 = sqlx::query_scalar(
            "SELECT audible_ms FROM listen_facts WHERE session_id = 'audio-counter-test'",
        )
        .fetch_one(store.reader())
        .await
        .expect("listen fact");
        assert_eq!(audible_ms, 5_000);
    }
}
