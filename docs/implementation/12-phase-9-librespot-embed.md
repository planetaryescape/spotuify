# Phase 9 - Embedded Librespot

## Decision

**Embed librespot in the daemon.** All three active Rust Spotify TUIs (ncspot, spotify-player, spotatui) embed librespot 0.8.x. Sibling-process spotifyd is supported as a `--backend spotifyd` fallback for users who want crash-isolation. Embedded becomes the default once mac+linux audio backends prove stable.

Decision log entry: D010 (write during Phase 13).

## Goal

Replace the supervised spotifyd sibling process with an in-process librespot Player + Spirc so a single `spotuify` binary registers itself as a Spotify Connect device. Use librespot's event stream as the daemon's primary playback truth (per Phase 6). Use librespot's mercury bus for endpoints Spotify killed in Nov 2024 (lyrics, radio, recommendations).

## Reference implementations

| Pattern | Reference | File:lines |
|---|---|---|
| librespot embed structure | spotify-player | `streaming.rs:142-281` |
| Worker with `tokio::select!` over command + event + interval | ncspot | `spotify_worker.rs:66-183` |
| RecoveringSink panic wrapper | spotatui | `streaming.rs:33-138` |
| Spirc dual-timeout (inner 30s + outer abort) | spotatui | `streaming.rs:434-466` + `runtime.rs:653-684` |
| Premium gate before init | spotatui | `runtime.rs:131-179` |
| Sink-factory closure (taps) | spotify-player | `streaming.rs:200-213` |
| `login5().auth_token()` → Web API token | spotify-player | `client/spotify.rs:86-102` + `token.rs:8-46` |
| Mercury bus: lyrics | spotify-player | `client/mod.rs:642-661` |
| Mercury bus: radio (autoplay) | spotify-player | `client/mod.rs:949-1019` |
| TimeToPreloadNextTrack → preload (gapless) | ncspot | `spotify_worker.rs:151-154`, `queue.rs:461-471` |
| Pulse env vars for pavucontrol | spotify-player | `main.rs:114-139` |
| Audio backend per-platform | spotatui | `Cargo.toml:72-99` |
| Position-as-SystemTime offset | ncspot | `spotify.rs:307-313` |
| Session-died → restart | ncspot | `application.rs:275-284` |
| Session-disconnect recovery (cache-only) | spotatui | `events.rs:55-120` |

## Pinned versions

| Crate | Version | Reason |
|---|---|---|
| `librespot-core` | 0.8 | All three competitors on 0.8.x |
| `librespot-connect` | 0.8 | Same |
| `librespot-oauth` | 0.8 | Provides redirect listener |
| `librespot-metadata` | 0.8 | Mercury access for lyrics |
| `librespot-playback` | 0.8 | `default-features = false, features = ["native-tls"]` to avoid rustls conflict |
| `librespot-protocol` | 0.8 | `default-features = false` |
| `vergen` | `=9.0.6` | Pinned trio required by librespot-core 0.8's build.rs |
| `vergen-lib` | `=9.1.0` | Same |
| `vergen-gitcl` | `=1.0.8` | Same |

## Audio backend matrix

| Platform | Default backend | Cargo feature | Notes |
|---|---|---|---|
| Linux x86_64 GNU | alsa | `alsa-backend` | PipeWire optional via `pipewire-backend` |
| Linux musl | rodio | `rodio-backend` | alsa-sys vendoring is fragile under musl |
| macOS aarch64 | portaudio | `portaudio-backend` | **Critical**: rodio + CoreAudio SIGSEGVs on AirPods disconnect (spotatui bug history) |
| macOS x86_64 | portaudio | `portaudio-backend` | Same |
| Windows x86_64 | rodio | `rodio-backend` | **Critical**: librespot's `pipe` sink writes PCM to stdout and corrupts the TUI; never select it |

Backend selection happens at compile time per target (Cargo features), with
`compile_error!` if `embedded-playback` is enabled without exactly one audio
backend. The root `spotuify` package forwards these features to the daemon and
player crates, so `cargo build --features 'embedded-playback,rodio-backend'`
uses the embedded sink-chain path end to end.

## Architecture

```text
crates/spotuify-player/
├── src/
│   ├── lib.rs                  // PlayerBackend trait
│   ├── backends/
│   │   ├── embedded/
│   │   │   ├── mod.rs          // EmbeddedBackend: Session + Player + Spirc
│   │   │   ├── recovering_sink.rs  // catch_unwind wrapper around audio backend
│   │   │   ├── premium_gate.rs
│   │   │   ├── token_bridge.rs // login5().auth_token() -> rspotify token
│   │   │   ├── mercury.rs      // lyrics, radio, recommendations
│   │   │   └── worker.rs       // tokio::select! command loop
│   │   ├── spotifyd/
│   │   │   └── mod.rs          // existing spotifyd-subprocess code
│   │   └── connect_only/
│   │       └── mod.rs          // Web API transfer only, no local device
│   ├── events.rs               // domain PlayerEvent (smaller than librespot's)
│   └── config.rs               // PlayerConfig, BackendKind enum
```

`PlayerBackend` trait:

```rust
#[async_trait]
pub trait PlayerBackend: Send + Sync {
    async fn register_device(&mut self, name: &str) -> Result<DeviceId>;
    async fn play_uri(&mut self, uri: &SpotifyUri, position_ms: u32) -> Result<()>;
    async fn pause(&mut self) -> Result<()>;
    async fn resume(&mut self) -> Result<()>;
    async fn next(&mut self) -> Result<()>;
    async fn previous(&mut self) -> Result<()>;
    async fn seek(&mut self, position_ms: u32) -> Result<()>;
    async fn volume(&mut self, percent: u8) -> Result<()>;
    async fn shuffle(&mut self, on: bool) -> Result<()>;
    async fn repeat(&mut self, mode: RepeatMode) -> Result<()>;
    async fn events(&mut self) -> BoxStream<'_, PlayerEvent>;
    async fn web_api_token(&self) -> Option<String>;
    async fn mercury_get(&self, uri: &str) -> Result<Bytes>;
    async fn is_connected(&self) -> bool;
    async fn shutdown(self) -> Result<()>;
}
```

## Implementation details

### Two-token strategy
- librespot owns the **streaming token** via its own OAuth (client_id with `streaming` scope).
- Use `session.login5().auth_token()` to get a **Web API token** out of the same session.
- This eliminates the second browser-prompt OAuth flow that ncspot, spotatui all do.
- Reference: spotify-player `token.rs:8-46`. Note their 5s timeout that forces `session.shutdown()` to trigger reconnect.
- Persist librespot creds via `librespot_core::cache::Cache::new(creds_path, volume_path, audio_cache_path, audio_cache_size_mib)`.

### Client IDs
- Streaming OAuth client_id: `65b708073fc0480ea92a077233ca87bd` (Spotify web client, has `streaming` scope; both spotify-player and spotatui use this).
- Web API client_id: user-provided via `config set spotify.client_id`, OR fall back to ncspot's public client `d420a117a32841c2b3474932e49fb54b`.
- Document the rotation playbook: if Spotify revokes either id, we ship a new release with a different id and a clear migration message.

### Spirc dual-timeout
```text
Spirc::new(...)
├── inner: 30s timeout (librespot's own)
└── outer: tokio::time::timeout + abort_handle (ours)
    ├── On inner failure (Spirc auth error) with cached creds → clear creds, retry once
    ├── On outer timeout → DO NOT clear creds (could be transient network), surface as `DaemonEvent::PlayerDegraded`
    └── On success → emit `DaemonEvent::PlayerReady { device_id, name }`
```

### RecoveringSink
Wrap the audio backend `Sink` in a struct that:
- Calls `catch_unwind` around `start()`, `stop()`, `write()`.
- On panic, drops the inner sink and lazily reconstructs it on next `write()`.
- Logs the panic via `tracing::error!` with sink type and platform.

Critical on macOS (AirPods disconnect panics PortAudio) and Linux (PipeWire restart). Adopt spotatui's implementation verbatim.

### Sink-factory closure for taps
`Player::new` takes a `Fn() -> Box<dyn Sink>` closure. We chain wrappers:

```text
sink_factory() -> LibrespotSinkChain(backend_sink)
                  ├── Phase 10 listen-qualified sample counter
                  ├── Phase 17 FFT visualization tap
                  └── RecoveringSink-style panic guard/rebuild
```

Implemented in `crates/spotuify-player/src/backends/librespot_sink_chain.rs`.
The chain taps decoded PCM before delegating the original `AudioPacket`
to the selected librespot physical backend, so the sink path is now
attachable from `EmbeddedBackend::sink_builder()` and constructed when
the embedded backend registers its device. `EmbeddedBackend` now also
stores Spirc after registration, forwards transport commands through it,
and translates librespot player events into `PlayerEvent`s. Live Spotify
account smoke is still separate from the local unit/clippy coverage.

Sink wrappers add: PCM tap for FFT and sample counter for accurate
"listen qualified" duration. RMS and current-track scrobble tagging are
future extensions.

### Premium gate
Before initializing Player/Spirc, call Web API `GET /me`. If `product != "premium"`:
- emit `DaemonEvent::PremiumRequired`
- skip librespot init entirely
- show TUI banner: "Streaming unavailable — Spotify Premium required. Browse and remote-control still work."
- `spotuify play` etc. with no other device active returns a clear error code.

Without this gate, librespot exits the process on a Free account.

### Mercury bus access
Expose:
- `mercury_get("hm://lyrics/v1/track/{spotify_id}")` for synced lyrics (Phase 16).
- `mercury_get("hm://autoplay-enabled/query?uri={uri}")` to check autoplay availability.
- `mercury_get("hm://radio-apollo/v3/stations/{uri}")` for radio/recommendations (the replacement for the dead `/recommendations` Web API endpoint).
- `mercury_get("hm://similarity-bff/v1/related-artists/{artist_id}")` for related-artists replacement.
- Cache mercury responses in SQLite with a TTL (60min default).

### Worker loop
```rust
async fn worker_run(mut state: WorkerState) -> Result<()> {
    let mut tick = tokio::time::interval(Duration::from_millis(400));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            cmd = state.command_rx.recv() => match cmd { ... }
            event = state.player_events.next() => match event { ... }
            _ = tick.tick(), if state.is_playing() => {
                state.events_tx.send(DaemonEvent::PositionTick { ms: state.derived_position_ms() })?;
            }
            _ = state.shutdown.cancelled() => break,
        }
    }
    Ok(())
}
```

The interval ticks only when playing (ncspot's pattern) — saves CPU when paused/stopped.

### Position computation
Don't tick a counter; derive position from `SystemTime::now() - playback_start_time` (ncspot pattern, `spotify.rs:307-313`). Position counter ticking is a class of off-by-one bugs we don't need.

### TimeToPreloadNextTrack → preload
On `PlayerEvent::TimeToPreloadNextTrack`, look up the next item in our queue (from sync.rs's `Queue` model) and call `player.preload(next_uri)`. This is how gapless playback works through librespot's API.

### Session-died handling
- `worker_loop` checks `session.is_invalid()` at the top of each iteration.
- If invalid, emit `DaemonEvent::SessionDisconnected { reason }`, then try `Session::new` with cached creds only (no browser).
- On second failure within 30s, surface `DaemonEvent::AuthError` and require user `spotuify reconnect` or `spotuify login`.
- `spotuify reconnect` CLI command + TUI keybind explicitly triggers session rebuild (ncspot's `:reconnect`).

### Pulse env vars (Linux only)
Set before Session::new on Linux:
```rust
std::env::set_var("PULSE_PROP_application.name", "spotuify");
std::env::set_var("PULSE_PROP_application.icon_name", "spotuify");
std::env::set_var("PULSE_PROP_stream.description", "Spotify (spotuify)");
```
Makes spotuify appear nicely in pavucontrol / mixer.

### Volume
- librespot uses u16 0-65535; user-facing 0-100.
- `librespot_value = (user_value as f32 / 100.0 * 65535.0).round() as u16`.
- SoftMixer only — no hardware mixer support (none of the competitors offer it; not worth it).

### Crash isolation
- Panic hook (already in `src/logging.rs`) writes backtrace to `~/.cache/spotuify/backtrace/<ts>.log`.
- RecoveringSink absorbs audio-backend panics.
- Worker task panics are caught by the supervising daemon and trigger restart after 1s, max 5 restarts in 60s before surfacing `DaemonEvent::PlayerFailed`.
- librespot is `JoinHandle` not separate process — protocol-drift maintenance comes in-house. Acceptable for one-binary install benefit.

## CLI / config

- `spotuify daemon --backend embedded|spotifyd|connect`
- `[player] backend = "embedded" | "spotifyd" | "connect"` in `config.toml`
- `[player] bitrate = 96 | 160 | 320` (default 320)
- `[player] device_name = "spotuify"` (default = hostname)
- `[player] normalization = false` (ReplayGain)
- `[player] audio_cache_mib = 0` (0 = disabled)
- `[player] pulse_props = true` (Linux only)
- `[analytics] hook_command = "..."` shell command run for playback/listen events; legacy `[player] event_hook` remains accepted as a fallback.
- `spotuify reconnect` — rebuild session (manual recovery).
- `spotuify doctor` reports: backend in use, audio backend selected, codec, bitrate, last `PlayerEvent` timestamp, Spirc state, premium status.

## Verification

- `spotuify daemon start --backend embedded` registers a Connect device named `spotuify` without spotifyd installed.
- macOS: connect AirPods, start playback, disconnect AirPods mid-track → daemon survives, RecoveringSink reports panic in log, switches to default device or pauses cleanly.
- Linux: PipeWire restart (`systemctl --user restart pipewire`) mid-playback → daemon survives, session reconnects within 5s.
- Windows: start daemon, start playback, sleep machine for 1 minute, wake → playback resumes from same position via librespot's reconnect.
- Free account: daemon refuses to init librespot, emits `PremiumRequired`, browse/control still works.
- Switch backends via `config set player.backend embedded` → `daemon restart` → previous backend cleanly shut down, new backend up; current playback queue persists across restart.
- Spirc auth failure with bad cached creds → daemon clears creds, retries once with browser flow.
- Spirc 30s timeout (simulate by blocking outbound traffic) → daemon emits `PlayerDegraded`, does NOT clear creds, surfaces to TUI.
- `mercury_get("hm://lyrics/v1/track/{id}")` returns synced lyrics for a known track.
- Playback control round-trip from CLI `spotuify pause` to TUI banner update measured <100ms on the embedded path (vs ~1-3s with Web API).
- Listen for `PlayerEvent::TimeToPreloadNextTrack`, confirm next track preloads, gap between tracks < 200ms.

## Migration

Existing spotifyd users:
- Keep `--backend spotifyd` as a supported choice indefinitely.
- `spotuify migrate-player` interactive prompt: detects spotifyd config, asks if user wants to switch to embedded, copies relevant settings, restarts daemon.
- README documents both paths.

## Definition of done

A fresh user runs `brew tap planetaryescape/spotuify && brew install spotuify && spotuify onboard && spotuify play "jazz"` and music plays locally with no other install steps. Existing spotifyd users see zero regression. Premium-gated, crash-isolated via RecoveringSink, dual-timeout Spirc init. Web API token comes from `login5().auth_token()`; no second OAuth flow. Mercury bus available for lyrics/radio. Phase 6's PlayerEvent-as-truth is fully wired. Decision recorded in `13-decision-log.md` as D010.
