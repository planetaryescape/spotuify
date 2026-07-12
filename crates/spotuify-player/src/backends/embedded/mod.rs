//! EmbeddedBackend — Phase 9.2+ librespot host.
//!
//! Hosts an in-process librespot Session + Player + Spirc so a single
//! `spotuify` binary registers as a Spotify Connect device. The sink
//! chain is attachable through `sink_builder()` and is handed to
//! `Player::new` during device registration. Playback controls are
//! forwarded through Spirc and librespot player events are translated
//! into the daemon's `PlayerEvent` stream.
//!
//! When the `embedded-playback` feature is off the whole module is
//! `#[cfg]`'d out and the daemon's player factory falls back to
//! spotifyd or connect (Phase 9.1 behaviour).

// Phase 9.5 guard: `embedded-playback` without a concrete audio
// backend is misconfigured — librespot would compile but have no
// way to emit audio. Force a build break with a useful message.
#[cfg(not(any(
    feature = "alsa-backend",
    feature = "pipewire-backend",
    feature = "rodio-backend",
    feature = "portaudio-backend",
)))]
compile_error!(
    "feature `embedded-playback` requires exactly one audio backend feature: \
     `alsa-backend`, `pipewire-backend`, `rodio-backend`, or `portaudio-backend`. \
     See docs/implementation/12-phase-9-librespot-embed.md (audio backend matrix) \
     for the recommended per-platform default."
);

#[cfg(any(
    all(feature = "alsa-backend", feature = "pipewire-backend"),
    all(feature = "alsa-backend", feature = "rodio-backend"),
    all(feature = "alsa-backend", feature = "portaudio-backend"),
    all(feature = "pipewire-backend", feature = "rodio-backend"),
    all(feature = "pipewire-backend", feature = "portaudio-backend"),
    all(feature = "rodio-backend", feature = "portaudio-backend"),
))]
compile_error!(
    "feature `embedded-playback` requires exactly one audio backend feature; \
     choose only one of `alsa-backend`, `pipewire-backend`, `rodio-backend`, \
     or `portaudio-backend`."
);

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use librespot_connect::{ConnectConfig, LoadRequest, LoadRequestOptions, PlayingTrack, Spirc};
use librespot_core::authentication::Credentials;
use librespot_core::cache::Cache;
use librespot_core::config::SessionConfig;
use librespot_core::session::Session;
use librespot_core::Error as LibrespotError;
use librespot_core::SpotifyUri;
use librespot_playback::audio_backend::Sink as LibrespotSink;
use librespot_playback::config::{PlayerConfig, VolumeCtrl};
use librespot_playback::mixer::{self, MixerConfig};
use librespot_playback::player::{Player, PlayerEvent as LibrespotPlayerEvent};
use parking_lot::Mutex;
use spotuify_audio::SharedAnalyzer;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::backends::audio_counter_tap::AudioCounterHandle;
use crate::backends::librespot_sink_chain::default_librespot_sink_factory;
use crate::backends::token_bridge::TokenProvider;
use crate::{
    BackendKind, DeviceId, PlayContextRequest, PlayerBackend, PlayerError, PlayerEvent,
    PlayerResult, RepeatMode,
};

const SESSION_CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
const SPIRC_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const MERCURY_GET_TIMEOUT: Duration = Duration::from_secs(10);

/// Cache layout under `~/.cache/spotuify/librespot/`. The three
/// subdirs match librespot's `Cache::new(creds, volume, audio, size)`
/// argument layout.
pub struct EmbeddedCachePaths {
    pub creds: PathBuf,
    pub volume: PathBuf,
    pub audio: Option<PathBuf>,
    pub audio_size_mib: Option<u64>,
}

impl EmbeddedCachePaths {
    pub fn under(base: PathBuf, audio_cache_mib: u32) -> Self {
        let root = base.join("librespot");
        Self {
            creds: root.join("creds"),
            volume: root.join("volume"),
            audio: (audio_cache_mib > 0).then(|| root.join("audio")),
            audio_size_mib: (audio_cache_mib > 0).then_some(audio_cache_mib as u64),
        }
    }
}

/// EmbeddedBackend — Phase 9.2+ host.
///
/// Holds the librespot cache plus the Session/Player state. Device
/// registration creates a real librespot `Player` with the tap-enabled
/// sink chain and stores Spirc for direct playback controls.
pub struct EmbeddedBackend {
    cache: Cache,
    token: Arc<dyn TokenProvider>,
    events_tx: mpsc::UnboundedSender<PlayerEvent>,
    viz_analyzer: Option<SharedAnalyzer>,
    audio_counter: Arc<AudioCounterHandle>,
    /// Local audio output device name to render to. `None` = follow system
    /// default. Applied when the sink chain is built in `ensure_spirc`.
    audio_output_device: Option<String>,
    /// librespot 0.8 ignores Load/Play/Pause/SetVolume until the Spirc device
    /// is activated. We activate lazily on the first transport command and
    /// latch it to avoid re-sending `Activate` (which librespot warns about).
    /// Held outside `State` so the player-event task can clear it the moment
    /// librespot deactivates us (another Connect device took over) — otherwise
    /// the latch goes stale, `ensure_active` skips re-activation, and every
    /// subsequent `Load` is silently dropped ("ignored while Not Active").
    /// Reset to `false` on (re)build and on deactivation; set `true` on
    /// successful activate.
    spirc_activated: Arc<AtomicBool>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    device_name: Option<String>,
    session: Option<Session>,
    player: Option<Arc<Player>>,
    spirc: Option<Spirc>,
    spirc_task: Option<tokio::task::JoinHandle<()>>,
    player_event_task: Option<tokio::task::JoinHandle<()>>,
}

impl EmbeddedBackend {
    /// Construct from the configured cache root. Returns the backend
    /// plus the receiving end of its event stream so the daemon can
    /// drain it through the same translator as ConnectOnly/Spotifyd.
    pub fn new(
        paths: EmbeddedCachePaths,
        token: Arc<dyn TokenProvider>,
    ) -> PlayerResult<(Arc<Self>, UnboundedReceiverStream<PlayerEvent>)> {
        Self::new_with_analyzer(paths, token, None, None)
    }

    pub fn new_with_analyzer(
        paths: EmbeddedCachePaths,
        token: Arc<dyn TokenProvider>,
        viz_analyzer: Option<SharedAnalyzer>,
        audio_output_device: Option<String>,
    ) -> PlayerResult<(Arc<Self>, UnboundedReceiverStream<PlayerEvent>)> {
        // Phase 9.5 — make spotuify show up nicely in pavucontrol on
        // Linux. The env vars are inherited by librespot's audio
        // backend when it spawns the PulseAudio stream.
        #[cfg(target_os = "linux")]
        {
            // SAFETY: this runs once during backend construction at
            // daemon boot; no other thread reads these vars at that
            // point.
            std::env::set_var("PULSE_PROP_application.name", "spotuify");
            std::env::set_var("PULSE_PROP_application.icon_name", "spotuify");
            std::env::set_var("PULSE_PROP_stream.description", "Spotify (spotuify)");
        }

        let cache = Cache::new(
            Some(paths.creds.as_path()),
            Some(paths.volume.as_path()),
            paths.audio.as_deref(),
            paths.audio_size_mib,
        )
        .map_err(|err| PlayerError::Other(anyhow::anyhow!("librespot cache init: {err}")))?;
        let (tx, rx) = mpsc::unbounded_channel();
        let backend = Arc::new(Self {
            cache,
            token,
            events_tx: tx,
            viz_analyzer,
            audio_counter: AudioCounterHandle::new(),
            audio_output_device,
            spirc_activated: Arc::new(AtomicBool::new(false)),
            state: Mutex::new(State::default()),
        });
        Ok((backend, UnboundedReceiverStream::new(rx)))
    }

    fn emit(&self, event: PlayerEvent) {
        let _ = self.events_tx.send(event);
    }

    /// Reference to the librespot cache, for tests + the OAuth flow
    /// that lives in librespot_oauth.
    pub fn cache(&self) -> &Cache {
        &self.cache
    }

    pub fn audio_counter(&self) -> Arc<AudioCounterHandle> {
        self.audio_counter.clone()
    }

    pub fn sink_builder(
        &self,
    ) -> PlayerResult<impl FnOnce() -> Box<dyn LibrespotSink> + Send + 'static> {
        let output_device = self.effective_audio_output_device();
        tracing::debug!(
            configured = ?self.audio_output_device,
            selected = ?output_device,
            "building librespot sink"
        );
        default_librespot_sink_factory(
            output_device,
            self.viz_analyzer.clone(),
            self.audio_counter.clone(),
        )
        .ok_or_else(|| PlayerError::Playback("no librespot audio backend available".into()))
    }

    fn effective_audio_output_device(&self) -> Option<String> {
        resolve_output_device(
            self.audio_output_device.clone(),
            &crate::list_audio_outputs(),
            current_default_output_override(),
        )
    }

    fn credentials(&self) -> PlayerResult<Credentials> {
        if let Some(credentials) = self.cache.credentials() {
            return Ok(credentials);
        }
        self.token
            .current_token()
            .map(Credentials::with_access_token)
            .ok_or_else(|| {
                PlayerError::Auth(
                    "embedded backend needs cached librespot credentials or a Spotify access token"
                        .to_string(),
                )
            })
    }

    /// Build a `SessionConfig` with a deterministic `device_id` derived
    /// from the configured device name (see `derive_device_id`).
    /// Falls back to librespot's UUIDv4 default when we don't yet
    /// know the name (e.g. preload firing before `register_device`).
    fn session_config(&self) -> SessionConfig {
        let mut config = SessionConfig::default();
        if let Some(name) = self.state.lock().device_name.as_deref() {
            config.device_id = derive_device_id(name);
        }
        config
    }

    async fn session(&self) -> PlayerResult<Session> {
        if let Some(session) = self
            .state
            .lock()
            .session
            .as_ref()
            .filter(|session| !session.is_invalid())
            .cloned()
        {
            return Ok(session);
        }

        let credentials = self.credentials()?;
        let session = Session::new(self.session_config(), Some(self.cache.clone()));
        tokio::time::timeout(SESSION_CONNECT_TIMEOUT, session.connect(credentials, true))
            .await
            .map_err(|_| PlayerError::Timeout(SESSION_CONNECT_TIMEOUT))?
            .map_err(|err| PlayerError::Network(format!("librespot session connect: {err}")))?;
        self.state.lock().session = Some(session.clone());
        Ok(session)
    }

    fn session_for_spirc(&self) -> PlayerResult<(Session, Credentials)> {
        let credentials = self.credentials()?;
        if let Some(session) = self
            .state
            .lock()
            .session
            .as_ref()
            .filter(|session| !session.is_invalid())
            .cloned()
        {
            return Ok((session, credentials));
        }

        let session = Session::new(self.session_config(), Some(self.cache.clone()));
        self.state.lock().session = Some(session.clone());
        Ok((session, credentials))
    }

    async fn ensure_spirc(&self, name: &str) -> PlayerResult<()> {
        if self.state.lock().spirc.is_some() {
            return Ok(());
        }

        let (session, credentials) = self.session_for_spirc()?;
        let sink_builder = self.sink_builder()?;
        let mixer_builder = mixer::find(None)
            .ok_or_else(|| PlayerError::Playback("no librespot mixer available".to_string()))?;
        let mixer = mixer_builder(mixer_config())
            .map_err(|err| PlayerError::Playback(format!("librespot mixer init: {err}")))?;
        let player = Player::new(
            player_config(),
            session.clone(),
            mixer.get_soft_volume(),
            sink_builder,
        );
        let mut player_events = player.get_player_event_channel();
        let player_events_tx = self.events_tx.clone();
        let activated_for_events = self.spirc_activated.clone();
        let player_event_task = tokio::spawn(async move {
            while let Some(event) = player_events.recv().await {
                // When another Connect device takes over, librespot deactivates
                // us and emits SessionDisconnected (spirc `became_inactive`).
                // Clear the activation latch so the next transport re-activates
                // instead of sending Loads librespot drops as "Not Active".
                if matches!(event, LibrespotPlayerEvent::SessionDisconnected { .. }) {
                    activated_for_events.store(false, Ordering::SeqCst);
                }
                if let Some(event) = translate_librespot_player_event(event) {
                    if player_events_tx.send(event).is_err() {
                        break;
                    }
                }
            }
        });
        let config = ConnectConfig {
            name: name.to_string(),
            initial_volume: self.initial_volume(),
            ..ConnectConfig::default()
        };
        let (spirc, task) = tokio::time::timeout(
            SPIRC_CONNECT_TIMEOUT,
            Spirc::new(config, session, credentials, player.clone(), mixer),
        )
        .await
        .map_err(|_| PlayerError::Timeout(SPIRC_CONNECT_TIMEOUT))?
        .map_err(|err| PlayerError::Playback(format!("librespot spirc init: {err}")))?;
        // librespot's Spirc task ends when the underlying session/dealer
        // closes — most importantly on a silent AP keepalive drop
        // ("Connection to server closed"), which librespot does NOT surface as
        // a player event. Wrap the task so its natural completion emits a
        // SessionDisconnected, giving the daemon a reliable reconnect trigger.
        // On an intentional teardown (`shutdown`) we abort this handle BEFORE
        // the spirc loop ends, so the emit cannot fire spuriously.
        let session_lost_tx = self.events_tx.clone();
        let task = tokio::spawn(async move {
            task.await;
            let _ = session_lost_tx.send(PlayerEvent::SessionDisconnected {
                reason: "librespot session closed".to_string(),
            });
        });

        let mut state = self.state.lock();
        state.player = Some(player);
        state.spirc = Some(spirc);
        state.spirc_task = Some(task);
        state.player_event_task = Some(player_event_task);
        // Fresh Spirc starts inactive — force re-activation on next play.
        self.spirc_activated.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Activate the Spirc device so librespot honours transport commands.
    /// librespot 0.8 silently drops `Load`/`Play`/`SetVolume` while the
    /// connect device is "Not Active" (see librespot-connect spirc.rs:
    /// `_ if !is_active() => warn!("…ignored while Not Active")`). The
    /// embedded backend is the playback device, so we activate it the
    /// first time we drive playback. Idempotent via the `spirc_activated`
    /// latch to avoid librespot's "ignored while already active" warning.
    /// The latch is cleared when librespot deactivates us (see the player-
    /// event task in `ensure_spirc`), so a device hand-off doesn't leave it
    /// stale and silently drop every later `Load`.
    fn ensure_active(&self) -> PlayerResult<()> {
        if self.spirc_activated.load(Ordering::SeqCst) {
            return Ok(());
        }
        let state = self.state.lock();
        let spirc = state.spirc.as_ref().ok_or(PlayerError::NotInitialised)?;
        spirc
            .activate()
            .map_err(|err| PlayerError::Playback(format!("librespot spirc activate: {err}")))?;
        self.spirc_activated.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn send_spirc(
        &self,
        action: impl FnOnce(&Spirc) -> Result<(), LibrespotError>,
    ) -> PlayerResult<()> {
        let state = self.state.lock();
        let spirc = state.spirc.as_ref().ok_or(PlayerError::NotInitialised)?;
        action(spirc)
            .map_err(|err| PlayerError::Playback(format!("librespot spirc command: {err}")))
    }

    fn initial_volume(&self) -> u16 {
        self.cache.volume().unwrap_or(u16::MAX / 2)
    }

    fn set_cached_volume(&self, volume: u16) -> PlayerResult<()> {
        let active = {
            let state = self.state.lock();
            if state.spirc.is_none() {
                return Err(PlayerError::NotInitialised);
            }
            self.spirc_activated.load(Ordering::SeqCst)
        };

        if active {
            self.send_spirc(|spirc| spirc.set_volume(volume))?;
        }
        self.cache.save_volume(volume);
        Ok(())
    }

    fn apply_current_volume(&self) -> PlayerResult<()> {
        let volume = self.initial_volume();
        self.send_spirc(|spirc| spirc.set_volume(volume))
    }
}

#[async_trait]
impl PlayerBackend for EmbeddedBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Embedded
    }

    fn audio_counter(&self) -> Option<Arc<AudioCounterHandle>> {
        Some(self.audio_counter.clone())
    }

    fn set_audio_output_device(&mut self, device: Option<String>) {
        // Stored only; `sink_builder` reads it the next time the Spirc
        // is (re)built, so callers pair this with a reconnect.
        self.audio_output_device = device;
    }

    async fn register_device(&mut self, name: &str) -> PlayerResult<DeviceId> {
        // Stash the name BEFORE creating the session so `session_config`
        // can derive the stable device_id (see `derive_device_id`).
        // Order matters here — `ensure_spirc` constructs the librespot
        // Session via `session_for_spirc`, which reads `device_name`.
        self.state.lock().device_name = Some(name.to_string());
        self.ensure_spirc(name).await?;
        let id = DeviceId::new(format!("embedded-pending-{name}"));
        self.emit(PlayerEvent::Ready {
            device_id: id.clone(),
            name: name.to_string(),
        });
        Ok(id)
    }

    async fn play_uri(&mut self, uri: &str, position_ms: u32) -> PlayerResult<()> {
        let request = load_request_for_uri(uri, position_ms)?;
        // librespot 0.8 ignores Load until the device is active. Activate
        // before loading; both commands queue on the same Spirc channel
        // and are processed in order, so the load lands post-activation.
        self.ensure_active()?;
        self.apply_current_volume()?;
        self.send_spirc(|spirc| spirc.load(request))
    }

    async fn play_context(&mut self, request: PlayContextRequest) -> PlayerResult<()> {
        let load = load_request_for_context(&request)?;
        // Same activate-before-load ordering as `play_uri`.
        self.ensure_active()?;
        self.apply_current_volume()?;
        self.send_spirc(|spirc| spirc.load(load))
    }

    async fn pause(&mut self) -> PlayerResult<()> {
        self.send_spirc(Spirc::pause)
    }

    async fn resume(&mut self) -> PlayerResult<()> {
        // Resume can be the first command after a daemon restart (new,
        // inactive Spirc), so activate before playing — same Not-Active
        // gate as `play_uri`.
        self.ensure_active()?;
        self.apply_current_volume()?;
        self.send_spirc(Spirc::play)
    }

    async fn next(&mut self) -> PlayerResult<()> {
        self.send_spirc(Spirc::next)
    }

    async fn previous(&mut self) -> PlayerResult<()> {
        self.send_spirc(Spirc::prev)
    }

    async fn seek(&mut self, position_ms: u32) -> PlayerResult<()> {
        self.send_spirc(|spirc| spirc.set_position_ms(position_ms))
    }

    async fn volume(&mut self, percent: u8) -> PlayerResult<()> {
        self.set_cached_volume(volume_percent_to_librespot(percent))
    }

    async fn shuffle(&mut self, on: bool) -> PlayerResult<()> {
        self.send_spirc(|spirc| spirc.shuffle(on))
    }

    async fn repeat(&mut self, mode: RepeatMode) -> PlayerResult<()> {
        match mode {
            RepeatMode::Off => {
                self.send_spirc(|spirc| spirc.repeat(false))?;
                self.send_spirc(|spirc| spirc.repeat_track(false))
            }
            RepeatMode::Context => {
                self.send_spirc(|spirc| spirc.repeat_track(false))?;
                self.send_spirc(|spirc| spirc.repeat(true))
            }
            RepeatMode::Track => self.send_spirc(|spirc| spirc.repeat_track(true)),
        }
    }

    async fn preload_uri(&mut self, uri: &str) -> PlayerResult<()> {
        let parsed = preloadable_uri(uri)?;
        let player = self
            .state
            .lock()
            .player
            .as_ref()
            .cloned()
            .ok_or(PlayerError::NotInitialised)?;
        player.preload(parsed);
        Ok(())
    }

    async fn queue_add(&mut self, _uri: &str) -> PlayerResult<()> {
        // librespot 0.8.0 (crates.io) does NOT expose Spirc::add_to_queue
        // as a public method — the dealer can RECEIVE AddToQueue
        // commands from Spotify's network but Spirc has no way to
        // ORIGINATE one. The dev/git branch adds it; expect to ship
        // when a release lands. Until then, return Unsupported so the
        // handler falls back to the Web API POST /me/player/queue path.
        Err(PlayerError::Unsupported(
            "Spirc::add_to_queue not public in librespot 0.8.0; using Web API fallback".to_string(),
        ))
    }

    async fn is_connected(&self) -> bool {
        let state = self.state.lock();
        // The session can be resurrected as a bare login5 session (for Web API
        // token minting via `session()`) independently of the Spirc playback
        // device. Require the Spirc task to still be running so a dead playback
        // device can't be masked by a live bare session — otherwise the health
        // loop reads "connected" and never reconnects the silent drop.
        let session_ok = state
            .session
            .as_ref()
            .is_some_and(|session| !session.is_invalid());
        let spirc_ok = state
            .spirc_task
            .as_ref()
            .is_some_and(|task| !task.is_finished());
        session_ok && spirc_ok
    }

    /// Mint a full-scope Web API bearer from the live librespot session
    /// via `login5`. This is the opt-in first-party token source: the
    /// keymaster session is never in Development Mode, so this bearer
    /// can write playlists where a dev-app token gets a 403. It is not
    /// the default Web API path while sustained keymaster polling is
    /// more rate-limit-prone than per-user dev-app traffic.
    ///
    /// `login5`'s `Login5Manager` caches the token internally and only
    /// re-mints when within seconds of expiry, so calling this on every
    /// daemon auth-health tick is cheap. Returns `None` (rather than an
    /// error) when no session can be established yet — the daemon treats
    /// that as "not authenticated".
    async fn web_api_token(&self) -> Option<String> {
        let session = match self.session().await {
            Ok(session) => session,
            Err(err) => {
                tracing::debug!(
                    error = %err,
                    "web_api_token: no librespot session for login5 mint yet"
                );
                return None;
            }
        };
        match crate::backends::first_party_auth::mint_via_login5(&session).await {
            Ok(token) => Some(token.access_token),
            Err(err) => {
                tracing::warn!(error = %err, "web_api_token: login5 mint failed");
                None
            }
        }
    }

    async fn mercury_get(&self, uri: &str) -> PlayerResult<Bytes> {
        let session = self.session().await?;
        let future = session
            .mercury()
            .get(uri)
            .map_err(|err| PlayerError::Network(format!("mercury get start: {err}")))?;
        let response = tokio::time::timeout(MERCURY_GET_TIMEOUT, future)
            .await
            .map_err(|_| PlayerError::Timeout(MERCURY_GET_TIMEOUT))?
            .map_err(|err| PlayerError::Network(format!("mercury get: {err}")))?;
        if !(200..300).contains(&response.status_code) {
            return Err(PlayerError::Playback(format!(
                "mercury get {uri} failed with status {}",
                response.status_code
            )));
        }
        let bytes = response.payload.into_iter().flatten().collect::<Vec<u8>>();
        Ok(Bytes::from(bytes))
    }

    async fn shutdown(&mut self) -> PlayerResult<()> {
        let mut state = self.state.lock();
        // Abort the Spirc monitor task FIRST. It emits SessionDisconnected when
        // the spirc loop ends, and an intentional shutdown ends that loop;
        // aborting before `spirc.shutdown()` keeps the teardown from scheduling
        // a spurious reconnect.
        if let Some(task) = state.spirc_task.take() {
            task.abort();
        }
        if let Some(spirc) = state.spirc.take() {
            if let Err(err) = spirc.shutdown() {
                tracing::debug!(error = %err, "librespot spirc shutdown failed during cleanup");
            }
        }
        if let Some(task) = state.player_event_task.take() {
            task.abort();
        }
        if let Some(session) = state.session.take() {
            session.shutdown();
        }
        state.player.take();
        state.device_name = None;
        // No spirc → not active. `ensure_spirc` also resets this on rebuild,
        // but clear it here so a torn-down backend never reads as activated.
        self.spirc_activated.store(false, Ordering::SeqCst);
        Ok(())
    }
}

/// Derive a stable Spotify Connect device ID from a device name.
///
/// Spotify's `/v1/me/player/devices` retains every distinct device_id
/// it has ever seen for hours-to-days, even after the librespot
/// session shuts down — there's no public deregister API. Librespot's
/// `SessionConfig::default_for_os()` defaults `device_id` to a fresh
/// `uuid::Uuid::new_v4()` per process, so every daemon restart
/// registers a NEW Connect device and the list grows monotonically.
///
/// The fix is industry-standard (this is exactly what spotifyd does at
/// `Spotifyd/spotifyd/src/config.rs:616`): derive the ID
/// deterministically from the device name via SHA-1. Same name → same
/// ID across restarts → no accumulation.
///
/// `device_id` is opaque to Spotify; the format just needs to be stable
/// for a given install. 40-char lowercase hex matches spotifyd so
/// users running both end up with the same registration.
fn derive_device_id(name: &str) -> String {
    use sha1::{Digest, Sha1};
    let digest = Sha1::digest(name.as_bytes());
    let mut out = String::with_capacity(40);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn player_config() -> PlayerConfig {
    PlayerConfig {
        // Phase 8 — periodic position heartbeat. The daemon's PlaybackClock
        // extrapolates progress locally, so we only need librespot's report
        // often enough to catch sink underrun and clock drift. 5s gives the
        // clock a fresh anchor every cycle while keeping CPU off the audio
        // worker. The internal worker tick (400ms) is unrelated — it drives
        // the spawn_worker loop, not librespot's emit cadence.
        position_update_interval: Some(std::time::Duration::from_secs(5)),
        ..PlayerConfig::default()
    }
}

fn mixer_config() -> MixerConfig {
    MixerConfig {
        volume_ctrl: VolumeCtrl::Linear,
        ..MixerConfig::default()
    }
}

fn volume_percent_to_librespot(percent: u8) -> u16 {
    ((percent.min(100) as u32 * u16::MAX as u32) / 100) as u16
}

#[cfg(all(
    target_os = "macos",
    feature = "portaudio-backend",
    feature = "audio-device-enumeration"
))]
fn current_default_output_override() -> Option<String> {
    crate::current_default_output_name()
}

#[cfg(not(all(
    target_os = "macos",
    feature = "portaudio-backend",
    feature = "audio-device-enumeration"
)))]
fn current_default_output_override() -> Option<String> {
    None
}

/// Decide which output-device name to hand to the librespot sink.
///
/// A configured device that is no longer present — e.g. AirPods that have
/// disconnected — makes the PortAudio sink panic with "could not find device"
/// and degrades the backend. That degradation not only kills local playback,
/// it also reads to the audio-flow watchdog as "clock playing but sink not
/// advancing", which is the one condition that arms it. So when we can
/// enumerate outputs and the configured device is confirmed absent, fall back
/// to the system default instead. When enumeration is unavailable (`available`
/// empty) the configured name is trusted as before.
fn resolve_output_device(
    configured: Option<String>,
    available: &[String],
    system_default: Option<String>,
) -> Option<String> {
    let Some(name) = configured else {
        return system_default;
    };
    if available.is_empty() || available.contains(&name) {
        return Some(name);
    }
    tracing::warn!(
        configured = %name,
        fallback = ?system_default,
        "configured audio output device is not available; falling back to system default"
    );
    system_default
}

/// Inverse of [`volume_percent_to_librespot`]: map librespot's u16 volume
/// onto 0..=100, rounding to nearest so a 50%/55% set round-trips back to
/// the same percent rather than drifting down by one.
fn librespot_volume_to_percent(volume: u16) -> u8 {
    let max = u16::MAX as u32;
    ((volume as u32 * 100 + max / 2) / max) as u8
}

fn load_request_for_uri(uri: &str, position_ms: u32) -> PlayerResult<LoadRequest> {
    let options = LoadRequestOptions {
        start_playing: true,
        seek_to: position_ms,
        ..LoadRequestOptions::default()
    };
    if uri.starts_with("spotify:track:") || uri.starts_with("spotify:episode:") {
        return Ok(LoadRequest::from_tracks(vec![uri.to_string()], options));
    }
    if uri.starts_with("spotify:") {
        return Ok(LoadRequest::from_context_uri(uri.to_string(), options));
    }
    Err(PlayerError::InvalidArg(format!(
        "expected Spotify URI, got `{uri}`"
    )))
}

/// Build a Spirc load for "play this collection, starting at this track".
///
/// - Explicit `tracks` (Liked Songs) → `from_tracks(full_list)`, so the
///   whole collection becomes the queue.
/// - `context_uri` (album/playlist/…) → `from_context_uri`, so Spotify
///   owns natural progression.
/// - `playing_track: Some(Uri(start_uri))` starts at the tapped track;
///   librespot resolves the index by URI inside the loaded context and
///   falls back to the first track when the URI isn't present.
///
/// `context_options` is deliberately left `None`. In the pinned fork
/// `LoadContextOptions::Autoplay` on the context-URI path makes librespot
/// load the *autoplay/radio* variant of the context instead of the album
/// itself; the non-shuffle load path already calls
/// `add_autoplay_resolving_when_required()`, so radio continuation after
/// the collection is preserved without that hazard.
fn load_request_for_context(request: &PlayContextRequest) -> PlayerResult<LoadRequest> {
    let options = LoadRequestOptions {
        start_playing: true,
        seek_to: request.position_ms,
        playing_track: Some(PlayingTrack::Uri(request.start_uri.clone())),
        ..LoadRequestOptions::default()
    };
    if let Some(tracks) = request.tracks.as_ref().filter(|t| !t.is_empty()) {
        return Ok(LoadRequest::from_tracks(tracks.clone(), options));
    }
    if let Some(context_uri) = request.context_uri.as_deref() {
        return Ok(LoadRequest::from_context_uri(
            context_uri.to_string(),
            options,
        ));
    }
    // No usable context — fall back to the lone track.
    load_request_for_uri(&request.start_uri, request.position_ms)
}

fn preloadable_uri(uri: &str) -> PlayerResult<SpotifyUri> {
    let parsed = SpotifyUri::from_uri(uri)
        .map_err(|err| PlayerError::InvalidArg(format!("invalid Spotify URI `{uri}`: {err}")))?;
    if !parsed.is_playable() {
        return Err(PlayerError::InvalidArg(format!(
            "expected playable Spotify URI, got `{uri}`"
        )));
    }
    Ok(parsed)
}

fn spotify_uri_string(uri: &SpotifyUri) -> String {
    // librespot's `SpotifyUri::to_uri` is infallible in the pinned fork
    // (returns String, not Result) — see docs/maintenance/librespot-fork.md.
    uri.to_uri()
}

fn translate_librespot_player_event(event: LibrespotPlayerEvent) -> Option<PlayerEvent> {
    match event {
        LibrespotPlayerEvent::Playing {
            track_id,
            position_ms,
            ..
        } => Some(PlayerEvent::PlaybackStarted {
            uri: spotify_uri_string(&track_id),
            position_ms,
        }),
        LibrespotPlayerEvent::Paused { .. } => Some(PlayerEvent::PlaybackPaused),
        LibrespotPlayerEvent::TrackChanged { audio_item } => Some(PlayerEvent::TrackChanged {
            uri: audio_item.uri,
            position_ms: 0,
        }),
        LibrespotPlayerEvent::PositionChanged { position_ms, .. }
        | LibrespotPlayerEvent::PositionCorrection { position_ms, .. }
        | LibrespotPlayerEvent::Seeked { position_ms, .. } => {
            Some(PlayerEvent::PositionTick { position_ms })
        }
        LibrespotPlayerEvent::EndOfTrack { track_id, .. } => Some(PlayerEvent::EndOfTrack {
            uri: spotify_uri_string(&track_id),
        }),
        LibrespotPlayerEvent::Stopped { .. } => None,
        LibrespotPlayerEvent::TimeToPreloadNextTrack { track_id, .. } => {
            Some(PlayerEvent::PreloadNext {
                uri: spotify_uri_string(&track_id),
            })
        }
        LibrespotPlayerEvent::Preloading { .. } => None,
        LibrespotPlayerEvent::SessionDisconnected { .. } => {
            Some(PlayerEvent::SessionDisconnected {
                reason: "Spotify session disconnected".to_string(),
            })
        }
        LibrespotPlayerEvent::Unavailable { track_id, .. } => Some(PlayerEvent::Degraded {
            reason: format!("track unavailable: {}", spotify_uri_string(&track_id)),
        }),
        LibrespotPlayerEvent::VolumeChanged { volume } => Some(PlayerEvent::VolumeChanged {
            percent: librespot_volume_to_percent(volume),
        }),
        LibrespotPlayerEvent::PlayRequestIdChanged { .. }
        | LibrespotPlayerEvent::Loading { .. }
        | LibrespotPlayerEvent::SessionConnected { .. }
        | LibrespotPlayerEvent::SessionClientChanged { .. }
        | LibrespotPlayerEvent::ShuffleChanged { .. }
        | LibrespotPlayerEvent::RepeatChanged { .. }
        | LibrespotPlayerEvent::AutoPlayChanged { .. }
        // SetQueue is new in the pinned librespot fork (upstream #1677): a
        // Connect-state queue/context notification. spotuify's daemon owns the
        // queue, so we ignore it like the other Connect-state events.
        | LibrespotPlayerEvent::SetQueue { .. }
        | LibrespotPlayerEvent::FilterExplicitContentChanged { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        derive_device_id, librespot_volume_to_percent, load_request_for_context,
        load_request_for_uri, mixer_config, preloadable_uri, resolve_output_device,
        translate_librespot_player_event, volume_percent_to_librespot, EmbeddedBackend,
        EmbeddedCachePaths,
    };
    use crate::backends::token_bridge::StaticTokenProvider;
    use crate::{PlayContextRequest, PlayerBackend, PlayerError, PlayerEvent};
    use librespot_connect::PlayingTrack;
    use librespot_core::SpotifyUri;
    use librespot_playback::config::VolumeCtrl;
    use librespot_playback::player::PlayerEvent as LibrespotPlayerEvent;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn owned(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn resolve_output_prefers_configured_when_available() {
        assert_eq!(
            resolve_output_device(
                Some("AirPods".into()),
                &owned(&["AirPods", "MacBook Pro Speakers"]),
                Some("MacBook Pro Speakers".into()),
            ),
            Some("AirPods".to_string())
        );
    }

    #[test]
    fn resolve_output_falls_back_when_configured_absent() {
        // AirPods disconnected: not in the live list -> use the system default
        // rather than handing PortAudio a device that would panic.
        assert_eq!(
            resolve_output_device(
                Some("Bhekani's AirPods Pro".into()),
                &owned(&["MacBook Pro Speakers", "DELL U4025QW"]),
                Some("MacBook Pro Speakers".into()),
            ),
            Some("MacBook Pro Speakers".to_string())
        );
    }

    #[test]
    fn resolve_output_trusts_configured_when_enumeration_unavailable() {
        // Empty list == backend can't enumerate; don't second-guess the config.
        assert_eq!(
            resolve_output_device(Some("AirPods".into()), &[], Some("Speakers".into())),
            Some("AirPods".to_string())
        );
    }

    #[test]
    fn resolve_output_uses_system_default_when_unconfigured() {
        assert_eq!(
            resolve_output_device(None, &owned(&["Speakers"]), Some("Speakers".into())),
            Some("Speakers".to_string())
        );
    }

    #[test]
    fn cache_paths_under_disabled_audio_returns_none() {
        // Adversarial: audio_cache_mib=0 must NOT create an audio
        // cache dir — librespot writes scratch frames there and a
        // user who explicitly opted out shouldn't see surprise GiBs
        // on disk.
        let paths = EmbeddedCachePaths::under(PathBuf::from("/tmp/test"), 0);
        assert!(paths.audio.is_none());
        assert!(paths.audio_size_mib.is_none());
    }

    #[test]
    fn cache_paths_under_enabled_audio_returns_path_and_size() {
        let paths = EmbeddedCachePaths::under(PathBuf::from("/tmp/test"), 256);
        assert_eq!(
            paths.audio,
            Some(PathBuf::from("/tmp/test/librespot/audio"))
        );
        assert_eq!(paths.audio_size_mib, Some(256));
    }

    #[test]
    fn cache_paths_layout_matches_phase_9_doc() {
        let paths = EmbeddedCachePaths::under(PathBuf::from("/u/.cache/spotuify"), 128);
        assert_eq!(
            paths.creds,
            PathBuf::from("/u/.cache/spotuify/librespot/creds")
        );
        assert_eq!(
            paths.volume,
            PathBuf::from("/u/.cache/spotuify/librespot/volume")
        );
    }

    #[tokio::test]
    async fn mercury_without_credentials_or_token_returns_auth_error() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let paths = EmbeddedCachePaths::under(temp.path().to_path_buf(), 0);
        let (backend, _stream) =
            EmbeddedBackend::new(paths, Arc::new(StaticTokenProvider::missing()))
                .expect("embedded backend");
        let backend = Arc::try_unwrap(backend).ok().expect("single owner");

        let err = backend
            .mercury_get("hm://lyrics/v1/track/abc")
            .await
            .expect_err("missing credentials should fail");

        assert!(
            matches!(err, crate::PlayerError::Auth(message) if message.contains("credentials"))
        );
    }

    #[tokio::test]
    async fn transport_commands_before_register_return_not_initialised() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let paths = EmbeddedCachePaths::under(temp.path().to_path_buf(), 0);
        let (backend, _stream) =
            EmbeddedBackend::new(paths, Arc::new(StaticTokenProvider::missing()))
                .expect("embedded backend");
        let mut backend = Arc::try_unwrap(backend).ok().expect("single owner");

        let err = backend
            .pause()
            .await
            .expect_err("pause before register should fail");

        assert!(matches!(err, PlayerError::NotInitialised));
    }

    #[test]
    fn volume_percent_maps_to_librespot_u16_range() {
        assert_eq!(volume_percent_to_librespot(0), 0);
        assert_eq!(volume_percent_to_librespot(100), u16::MAX);
        assert_eq!(volume_percent_to_librespot(200), u16::MAX);
        assert!(volume_percent_to_librespot(50) > 32_000);
        assert!(volume_percent_to_librespot(50) < 33_000);
    }

    #[test]
    fn mixer_uses_linear_volume_scale() {
        assert!(matches!(mixer_config().volume_ctrl, VolumeCtrl::Linear));
    }

    #[test]
    fn initial_volume_uses_saved_librespot_cache() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let paths = EmbeddedCachePaths::under(temp.path().to_path_buf(), 0);
        let (backend, _stream) =
            EmbeddedBackend::new(paths, Arc::new(StaticTokenProvider::missing()))
                .expect("embedded backend");

        assert_eq!(backend.initial_volume(), u16::MAX / 2);
        backend.cache().save_volume(volume_percent_to_librespot(80));
        assert_eq!(backend.initial_volume(), volume_percent_to_librespot(80));
    }

    #[test]
    fn librespot_volume_round_trips_through_percent() {
        assert_eq!(librespot_volume_to_percent(0), 0);
        assert_eq!(librespot_volume_to_percent(u16::MAX), 100);
        // Every percent we send must come back as the same percent.
        for percent in 0..=100u8 {
            assert_eq!(
                librespot_volume_to_percent(volume_percent_to_librespot(percent)),
                percent,
                "percent {percent} did not round-trip",
            );
        }
    }

    #[test]
    fn volume_changed_event_translates_to_percent() {
        let event = translate_librespot_player_event(LibrespotPlayerEvent::VolumeChanged {
            volume: volume_percent_to_librespot(55),
        });
        assert!(matches!(
            event,
            Some(PlayerEvent::VolumeChanged { percent: 55 })
        ));
    }

    #[test]
    fn play_uri_load_request_starts_at_requested_position() {
        let request = load_request_for_uri("spotify:track:abc", 42_000)
            .expect("track URI should build load request");

        assert!(request.start_playing);
        assert_eq!(request.seek_to, 42_000);
    }

    #[test]
    fn context_load_starts_at_requested_track_by_uri() {
        // Explicit track list (Liked Songs): the whole list loads and
        // playback starts at the tapped track, addressed by URI.
        let request = load_request_for_context(&PlayContextRequest {
            context_uri: None,
            tracks: Some(vec![
                "spotify:track:a".to_string(),
                "spotify:track:b".to_string(),
                "spotify:track:c".to_string(),
            ]),
            start_uri: "spotify:track:b".to_string(),
            position_ms: 0,
        })
        .expect("track-list context should build a load request");
        assert!(request.start_playing);
        assert!(matches!(
            request.playing_track,
            Some(PlayingTrack::Uri(ref uri)) if uri == "spotify:track:b"
        ));

        // Album/playlist context URI path also carries the start URI.
        let ctx = load_request_for_context(&PlayContextRequest {
            context_uri: Some("spotify:album:xyz".to_string()),
            tracks: None,
            start_uri: "spotify:track:b".to_string(),
            position_ms: 0,
        })
        .expect("context-uri path should build a load request");
        assert!(matches!(
            ctx.playing_track,
            Some(PlayingTrack::Uri(ref uri)) if uri == "spotify:track:b"
        ));

        // Empty track list with no context URI falls back to the lone track.
        let fallback = load_request_for_context(&PlayContextRequest {
            context_uri: None,
            tracks: Some(vec![]),
            start_uri: "spotify:track:b".to_string(),
            position_ms: 5_000,
        })
        .expect("empty context should fall back to single track");
        assert_eq!(fallback.seek_to, 5_000);
    }

    /// Locks the stable-device-id format so a careless refactor (e.g.
    /// switching hashers) can't accidentally orphan every previously-
    /// registered Spotify Connect device. The vector below is
    /// `echo -n 'spotuify' | shasum -a 1` and matches the spotifyd
    /// convention exactly.
    #[test]
    fn derive_device_id_is_lowercase_sha1_hex_of_name() {
        let id = derive_device_id("spotuify");
        assert_eq!(id, "c77941ae06acef3ef6b17f577668e6100c0089ef");
        assert_eq!(id.len(), 40);
        assert!(id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // Same input → same output across calls (the whole point).
        assert_eq!(derive_device_id("spotuify"), id);
        // Different names produce different IDs (no collisions in
        // practice with SHA-1 over short device names).
        assert_ne!(derive_device_id("spotuify-hume"), id);
    }

    #[test]
    fn play_uri_rejects_non_spotify_uri() {
        let err = load_request_for_uri("https://example.com/not-spotify", 0)
            .expect_err("non-Spotify URI should fail");

        assert!(matches!(err, PlayerError::InvalidArg(message) if message.contains("Spotify URI")));
    }

    #[test]
    fn preloadable_uri_rejects_context_uri() {
        let err = preloadable_uri("spotify:album:3n3Ppam7vgaVa1iaRUc9Lp")
            .expect_err("context URI should not be preloaded as audio");

        assert!(matches!(err, PlayerError::InvalidArg(message) if message.contains("playable")));
    }

    #[test]
    fn preloadable_uri_rejects_non_spotify_uri() {
        let err = preloadable_uri("https://example.com/not-spotify")
            .expect_err("non-spotify URI should fail");

        assert!(matches!(err, PlayerError::InvalidArg(message) if message.contains("invalid")));
    }

    #[test]
    fn librespot_playing_event_translates_to_domain_event() {
        let track_id =
            SpotifyUri::from_uri("spotify:track:3n3Ppam7vgaVa1iaRUc9Lp").expect("valid track URI");
        let event = translate_librespot_player_event(LibrespotPlayerEvent::Playing {
            play_request_id: 7,
            track_id,
            position_ms: 12_345,
        })
        .expect("playing event should translate");

        assert!(matches!(
            event,
            PlayerEvent::PlaybackStarted { ref uri, position_ms }
                if uri == "spotify:track:3n3Ppam7vgaVa1iaRUc9Lp" && position_ms == 12_345
        ));
    }

    #[test]
    fn librespot_position_event_translates_to_position_tick() {
        let track_id =
            SpotifyUri::from_uri("spotify:track:3n3Ppam7vgaVa1iaRUc9Lp").expect("valid track URI");
        let event = translate_librespot_player_event(LibrespotPlayerEvent::PositionChanged {
            play_request_id: 7,
            track_id,
            position_ms: 40_000,
        })
        .expect("position event should translate");

        assert!(matches!(
            event,
            PlayerEvent::PositionTick {
                position_ms: 40_000
            }
        ));
    }

    #[test]
    fn librespot_stopped_event_does_not_mark_playback_ended() {
        let track_id =
            SpotifyUri::from_uri("spotify:track:3n3Ppam7vgaVa1iaRUc9Lp").expect("valid track URI");
        let event = translate_librespot_player_event(LibrespotPlayerEvent::Stopped {
            play_request_id: 7,
            track_id,
        });

        assert!(
            event.is_none(),
            "librespot emits Stopped during track transitions; treating it as EndOfTrack pauses the daemon clock after next/previous"
        );
    }

    #[test]
    fn time_to_preload_translates_current_uri_signal() {
        let track_id =
            SpotifyUri::from_uri("spotify:track:3n3Ppam7vgaVa1iaRUc9Lp").expect("valid track URI");
        let event =
            translate_librespot_player_event(LibrespotPlayerEvent::TimeToPreloadNextTrack {
                play_request_id: 7,
                track_id,
            })
            .expect("preload window should translate");

        assert!(matches!(
            event,
            PlayerEvent::PreloadNext { ref uri }
                if uri == "spotify:track:3n3Ppam7vgaVa1iaRUc9Lp"
        ));
    }

    #[test]
    fn preloading_event_does_not_request_another_preload() {
        let track_id =
            SpotifyUri::from_uri("spotify:track:3n3Ppam7vgaVa1iaRUc9Lp").expect("valid track URI");
        let event = translate_librespot_player_event(LibrespotPlayerEvent::Preloading { track_id });

        assert!(event.is_none());
    }
}
