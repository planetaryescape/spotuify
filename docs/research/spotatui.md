# spotatui Deep Study

| | |
|---|---|
| Version sampled | v0.38.2 (May 2026) |
| Repo | https://github.com/LargeModGames/spotatui |
| Captured | 2026-05-13 |
| Provenance | Community revival of spotify-tui (abandoned 2021-11) |

## 1. Architecture

Single-binary, NOT a workspace, NOT a daemon. `Cargo.toml:124-128` declares one `[[bin]]` named `spotatui`. The repo also contains `worker-relay/` (Cloudflare Worker + Durable Object for listening-party WebSocket relay) but it's a separate JS subproject.

Module layout (`src/`):
- `core/` — app state, config, auth, layout (`app.rs` is **4308 lines**, the central state god-object)
- `infra/` — `network/` (rspotify wrappers + raw reqwest), `player/` (librespot wrapper + event pump), `audio/` (cpal/pipewire capture + FFT), `mpris.rs`, `macos_media.rs`, `discord_rpc.rs`, `redirect_uri.rs`, `media_metadata.rs`
- `tui/` — `event/`, `handlers/` (per-screen key handlers), `ui/` (per-screen draws), `runner.rs`, `cover_art.rs`, `banner.rs`
- `cli/` — `clap.rs`, `cli_app.rs`, `handle.rs`, `update.rs`, `util.rs`
- `runtime.rs` (1264 lines) — startup, OAuth, librespot init, channel wiring, panic hook
- `main.rs` — 12-line `#[tokio::main]` shim

**Evolution, not rewrite.** spotify-tui's original architecture is fully visible: `App` mega-struct + `IoEvent` enum dispatched via `std::sync::mpsc::Sender<IoEvent>` (`runtime.rs:582`) to a `Network` actor that runs in a `tokio::spawn`. Modules were extracted into `core/infra/tui` only in 0.37.0 (CHANGELOG L172). The recent v0.38.2 work (CHANGELOG L17-22) just finished pulling auth/runtime/TUI-runner out of `main.rs`. UI thread + IO thread + TUI input thread.

Event loop = `std::sync::mpsc` between UI and IO + `tokio::sync::mpsc` for streaming recovery + crossbeam-style polling (`runtime.rs:1007-1020`):

```rust
async fn start_tokio(io_rx: std::sync::mpsc::Receiver<IoEvent>, network: &mut Network) {
  loop {
    match io_rx.try_recv() {
      Ok(io_event) => network.handle_network_event(io_event).await,
      Err(std::sync::mpsc::TryRecvError::Empty) =>
        tokio::time::sleep(std::time::Duration::from_millis(5)).await,
      Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
    }
    network.process_party_messages().await;
  }
}
```

5ms try_recv busy-poll. Not ideal — a tokio mpsc + `select!` would let them sleep until needed.

## 2. PKCE auth migration

PKCE migration landed in 0.36.0 (CHANGELOG L228). All auth lives in `src/core/auth.rs` and `src/infra/redirect_uri.rs`.

- **Strictly `http://127.0.0.1`** — no `localhost` fallback. `src/core/config.rs:75-77`: `format!("http://127.0.0.1:{}/callback", self.get_port())`. `src/infra/redirect_uri.rs:7` binds `TcpListener::bind(format!("127.0.0.1:{}", port))`. (Spotify's Nov-2025 changes forbid `localhost`; spotatui complies.)
- **Code verifier**: fully delegated to rspotify 0.16 via `Credentials::new_pkce(client_id)` (`auth.rs:107`). No grep hits for `code_verifier`/`code_challenge`.
- **Two-client fallback (0.36.0)**: ships ncspot's shared client_id `d420a117a32841c2b3474932e49fb54b` (`config.rs:16`). First-run wizard lets user add a personal `fallback_client_id`. ncspot uses port `8989`, path `/login`; user app uses `8888`, path `/callback` (`auth.rs:85-91`, `config.rs:197,206`).
- **Token persistence**: manual, plaintext JSON at `~/.config/spotatui/spotify_token_cache_<first-8-of-client-id>.json` (`auth.rs:75-83`). **No keyring**. They explicitly note "rspotify's built-in caching isn't working" (`auth.rs:41`). A `.gitignore` is auto-generated in the config dir (`config.rs:99-115`) to hedge against dotfile sync.
- **Refresh strategy**: `auth.rs:42-57` includes a critical safety: if a refreshed token comes back without `refresh_token`, they merge the old refresh_token from disk before writing. That fix (PR #217) is in v0.38.1. Without it, every reboot forces re-login.
- **Re-auth detection**: `auth.rs:140-163` — `me()` probe at startup; if response text matches `401|unauthorized|status code 400|invalid_grant|access token expired`, delete cache and re-prompt. String-matching is fragile.
- **Streaming OAuth is a SEPARATE flow** — see §4.

## 3. Rate-limit resilience

Two layers, inconsistent:

**Layer A — `src/infra/network/requests.rs:30-127`**: hand-rolled raw `reqwest` request path called `spotify_get_typed_compat_for` (used for endpoints where rspotify 0.16's models can't deserialize Spotify's Feb 2026 payloads — see §7). The **good** layer:

- Global pacing: `OnceLock<Mutex<Option<Instant>>>` enforcing **≥250ms** between API calls (`requests.rs:13-28`). One mutex serializes all writes — coarse but simple.
- On 429: read `retry-after` header (`requests.rs:111-117`), default 1s, then `backoff_secs = retry_after.max(1) + attempt`, up to 4 attempts. Linear additive backoff, no jitter.
- On 401: refresh token once, replay request (`requests.rs:92-108`).
- On network error: 1-second linear backoff per attempt (`requests.rs:73-78`).

**Layer B — rspotify-direct calls** (most of `playback.rs`, `library.rs`, `user.rs`): NO retry middleware. They detect 429 by string-matching `err.to_string().contains("429")` (`playback.rs:374-385`, `user.rs:29`) and show a status message but DON'T retry. The function `is_rate_limited_error` (`requests.rs:242`) is just `text.contains("429") || ...`.

**Key finding**: the two layers are inconsistent. New compat-path endpoints get proper 429 + Retry-After; older rspotify calls just show toast and move on. No jitter anywhere. The fix is to make ALL requests go through a single middleware-style client.

## 4. Native streaming via librespot

Verified real. `Cargo.toml:61-83`:

```toml
librespot-core = { version = "0.8", optional = true }
librespot-connect = { version = "0.8", optional = true }
librespot-oauth = { version = "0.8", optional = true }
librespot-metadata = { version = "0.8", optional = true }
librespot-protocol = { version = "0.8", optional = true, default-features = false }
```

Per-platform backend selection (worth copying):
- Linux GNU: `alsa-backend` default; PipeWire optional
- Linux musl: `rodio-backend` (alsa-sys vendoring is painful)
- Windows: `rodio-backend` — explicit comment (`Cargo.toml:81`): without this, librespot's `pipe` sink falls back to writing PCM bytes to stdout and corrupts the TUI
- macOS: `portaudio-backend` (because rodio + CoreAudio SIGSEGVs on AirPods, see Cargo.toml:91-94)

**Pinned vergen trio** (`Cargo.toml:58-60`): `vergen = "=9.0.6"`, `vergen-lib = "=9.1.0"`, `vergen-gitcl = "=1.0.8"` to match what librespot-core 0.8's `build.rs` expects.

**StreamingPlayer** at `src/infra/player/streaming.rs:272-491`:

- Uses **spotify-player's client_id** `65b708073fc0480ea92a077233ca87bd` (`streaming.rs:153`) — NOT the user's app's id — because librespot needs a client_id with the `streaming` scope that newly-created developer apps no longer get. Redirect URI is hardcoded to `http://127.0.0.1:8989/login` (`streaming.rs:156`).
- Streaming OAuth uses `librespot-oauth`'s `OAuthClientBuilder` (`streaming.rs:161-176`), opens browser, gets access token, builds `Credentials::with_access_token(...)`. Separate from the Web API PKCE flow above.
- Credentials cached at `~/.config/spotatui/streaming_cache/credentials.json` via librespot's `Cache::new(...)` (`streaming.rs:339`). Audio cache separate, opt-in.
- **RecoveringSink wrapper** (`streaming.rs:33-138`) — wraps the backend `Sink` and `catch_unwind`s every `write/start/stop` so a panicking PortAudio (AirPods disconnect) doesn't crash the process. The sink is dropped and rebuilt on next write. **Adopt verbatim.**
- Spirc init has both an internal 30s timeout (`streaming.rs:434-466`) AND an outer `tokio::time::timeout` with abort (`runtime.rs:653-684`). On Spirc auth failure with cached creds, they clear creds and retry once (`streaming.rs:436-452`). On Spirc timeout, they explicitly DO NOT clear creds because it could be transient network.
- `is_connected()` (`streaming.rs:499-503`) checks `spirc_alive` AtomicBool + `!session.is_invalid() && !player.is_invalid()` — librespot 0.8 exposes invalidity flags.
- `set_repeat` cycles Off → Context (`repeat(true)+repeat_track(false)`) → Track (`repeat_track(true)`) → Off.
- Volume conversion: `(v/100 * 65535).round() as u16` (`streaming.rs:371,624`).
- Bitrate: 96/160/320, defaults 320 (`streaming.rs:355-364`).
- `play_uri` uses `player.load(SpotifyUri, true, 0)` directly; `load(LoadRequest)` uses Spirc for context-aware loads (`streaming.rs:506-529`).

**PlayerEvent consumption** at `src/infra/player/events.rs:177-461`: matches `Playing/Paused/Seeked/TrackChanged/Stopped/EndOfTrack/VolumeChanged/PositionChanged/SessionDisconnected`. Direct mutation of `App` state via `Arc<Mutex<App>>`. On `SessionDisconnected`, dispatches a `StreamingRecoveryRequest` to a separate recovery handler (`events.rs:55-120`) that tries `new_cache_only` (no browser) once.

**Premium gate**: `runtime.rs:131-179` — calls `spotify.me()` and refuses to initialize librespot for Free/unknown plans, surfacing a status banner. Prevents librespot's process-exit-on-free-account behavior. Free accounts can still browse.

## 5. Synced lyrics

Source: **LRCLIB only**. `src/infra/network/utils.rs:89-92`:

```rust
client.get("https://lrclib.net/api/get")
  .query(&[("track_name",..),("artist_name",..),("duration",..)])
```

- Hand-rolled LRC parser (`utils.rs:106-141`): regex-less, splits on `[mm:ss.xx]`. Supports 2 or 3 digit ms. Returns `Vec<(u128_ms, String)>`.
- No caching layer beyond in-memory `app.lyrics`. Re-fetched every track change.
- Falls back to `plainLyrics` if no `syncedLyrics` (`utils.rs:99-101`).
- Rendering at `src/tui/ui/player.rs:401-498`: scrolls so active line is at vertical center; binary-search-style scan for active index.
- Alignment uses `app.song_progress_ms` which is fed by librespot's `PositionChanged` (1s interval) or Web API polling. No offset tuning.

Implementation is minimal but works. No retries, no rate limit etiquette (LRCLIB asks for it).

## 6. Audio visualization

Verified — implementation is genuine and **does NOT use Spotify's deprecated Audio Analysis API**.

Source: **system audio loopback**, NOT a librespot tap.

- Linux: `cpal` host enumerated; picks devices whose name contains `"monitor"` (PipeWire/PulseAudio monitors), sorted bluez/bluetooth → speaker/analog → default → hdmi (`audio/capture.rs:80-119`). Falls back to default input. Also has a pipewire-native capture at `audio/pipewire_capture.rs` behind feature `audio-viz`.
- Windows: WASAPI loopback via `cpal::default_output_device()` (`capture.rs:73-76`).
- macOS: NO native loopback — requires BlackHole or Loopback virtual device; uses default input device (`capture.rs:131-137`, README L247). Doc-honest.

**FFT**: `realfft` crate, 2048-point real-to-complex (`audio/analyzer.rs:14`). Hann windowed. Maps 1025 magnitude bins to 12 logarithmic bands (`analyzer.rs:96-110`) with per-band gains compensating for high-frequency low-energy (`analyzer.rs:131-144`). EMA smoothing (factor 0.5), noise gate at 0.005, sqrt scaling for dB-like response. Rendered via `tui-equalizer` and `tui-bar-graph` (`Cargo.toml:36-37`).

Clean implementation. spotuify can adopt `analyzer.rs` near-verbatim.

## 7. Spotify Web API

**rspotify 0.16** as primary (`Cargo.toml:24`), `cli`+`env-file`+`client-reqwest`+`reqwest-rustls-tls`, default features off. Migration from 0.14 → 0.16 happened in 0.38.0 (CHANGELOG L82).

**Feb 2026 compat layer** (CHANGELOG L222): they hit deserialization failures because Spotify silently dropped `tracks`, `track`, `followers`, `external_ids`, `available_markets`, `linked_from`, `popularity`, etc. from various response shapes. Solution at `src/infra/network/requests.rs:129-240` — `normalize_spotify_payload()` walks the JSON before deserializing and reinserts missing keys with safe defaults so existing rspotify model structs still parse:

```rust
if map.contains_key("album") && map.contains_key("artists")
   && map.contains_key("track_number") && map.contains_key("duration_ms") {
  map.entry("available_markets").or_insert_with(|| json!([]));
  map.entry("external_ids").or_insert_with(|| json!({}));
  map.entry("linked_from").or_insert(Value::Null);
  map.entry("popularity").or_insert_with(|| json!(0));
}
```

Pragmatic but **brittle** — pattern-matches on the presence of certain keys to guess "this is a track". Files using compat path: `metadata.rs` (album_tracks, show_episodes, show, current_show_episodes), `search.rs` (artist search), library/playback partially.

**Deprecated endpoints STILL CALLED** (will break for new dev apps):

- `spotify.artist_related_artists(...)` — `metadata.rs:54`, `#[allow(deprecated)]`. README L250 acknowledges.
- `spotify.recommendations(...)` — `recommend.rs:39-48`, no `#[allow(deprecated)]` here but the endpoint is Nov-2024-deprecated.
- `spotify.artist_top_tracks(...)` — `metadata.rs:51-52`, marked deprecated.
- `audio-features` / `audio-analysis`: completely removed. Local FFT replaced it.

## 8. TUI patterns

- **ratatui 0.30** with `crossterm` + `layout-cache` features (`Cargo.toml:25`). Default features off.
- **Event loop**: 3 threads — UI/render (main, `tui/runner.rs`), crossterm input poll (`tui/event/events.rs:56`), Network IO (tokio task). Ticks at configurable rate (default ~250ms, lowered for visualization).
- **State management**: one giant `App` struct in `core/app.rs` (4308 lines, 220+ fields). Route stack is `Vec<(RouteId, ActiveBlock)>` pushed/popped by handlers. Settings and theme live inside `UserConfig` which is also inside `App`.
- **Key bindings**: YAML config at `~/.config/spotatui/config.yml` under `keybindings:`, parsed by `core/user_config.rs:459-540` (string-to-Key parser supporting `ctrl-<x>`, `alt-<x>`, `space`, `enter`, F-keys, etc.).
- **No command palette / fuzzy global picker**. Search is per-screen.
- **No multi-select on tracks** — operations are single-row. Real product gap.
- Mouse support added 0.37.0 with click/scroll on playbar controls.

## 9. CLI surface

`spt`/`spotatui` non-interactive subcommands defined in `src/cli/clap.rs`: `playback` (alias `pb`), `play` (`p`), `list` (`l`), `search` (`s`). Format-string output via `%t %a %b %p %h %f %s %v %d %r %u` placeholders. **No `--json` flag anywhere in the codebase** — only formatted strings.

The CLI reuses the same `Network` actor and rspotify client; CLI mode skips streaming init (`runtime.rs:597-604`).

## 10. Local cache / persistence

**No SQLite, no sled, no Tantivy, no embedded search index.** All app state — playlists, tracks, search results, paginated pages — is fetched fresh on demand and held in `App` in memory until exit.

Files:
- `client.yml` — client IDs, port, streaming knobs (YAML, `serde_yaml`)
- `config.yml` — keybindings, theme, behavior flags (YAML)
- `spotify_token_cache_<8>.json` — OAuth token (plaintext JSON, no keyring)
- `streaming_cache/credentials.json` — librespot's reusable token
- `streaming_cache/audio/` — librespot audio cache (opt-in, `streaming_audio_cache: false` default)
- `update_pending.json` — auto-update state (`cli/update.rs:63`)
- `/tmp/spotatui_logs/spotatuilog<PID>` — session log via `fern`

They added a `PreFetchSavedTracksPage` / `PreFetchPlaylistTracksPage` event for smoother paging but it's still in-memory only.

**Biggest architectural gap vs spotuify.** They lose all state on restart and pay the full API cost every session — which means rate limits matter much more.

## 11. Configuration

YAML, two files. `client.yml` is auth-y (`core/config.rs`) with `setup_version` migration field. `config.yml` is user/UX (`core/user_config.rs`, 1533 lines, has its own `UserConfigString` shadow struct for tolerant deserialization). Defaults are coded as Rust functions per field with `#[serde(default = "...")]` — removing a field from YAML just resorts to default rather than erroring. `BehaviorConfig` (`user_config.rs:677-715`) has ~30 fields including `relay_server_url`, `keepawake_enabled`, `tick_rate_milliseconds`, `sidebar_width_percent`, `playbar_height_rows`, `visualizer_style`. Theme has preset enum + per-color override.

## 12. Testing / CI

179 `#[test]`/`#[tokio::test]` annotations across `src/`. Concentrated in `auth.rs` (token-preserve logic), `redirect_uri.rs` (HTTP parsing), `media_metadata.rs`, `playback.rs` (offset trimming, seek timing), `network/library.rs`. No mocking of Spotify API — they test pure functions (payload normalizers, seek throttling, retry decisions). No integration tests against live API. No `mockito` / `wiremock` in `Cargo.toml`.

CI (`.github/workflows/ci.yml`): single ubuntu-latest job running `cargo check --locked`, `cargo test --locked`, `cargo fmt --check`, `cargo clippy -- -D warnings`. No cross-platform matrix in CI (only in CD). System deps include `libasound2-dev`, `libxcb*`, `pkg-config`, `libssl-dev`. CD (`cd.yml:17-43`): 4-target matrix — x86_64-linux-gnu, x86_64-apple-darwin (Intel runner), aarch64-apple-darwin, x86_64-pc-windows-msvc. Per-target audio feature sets baked into the matrix. `cargo-deb` for Debian package. AUR + Homebrew publishing scripts in `cd.yml`. Nix flake checked in (`flake.nix`).

## 13. Error handling

`anyhow::Result` everywhere, no custom error enum. Every network operation match-arms `Err(e) => self.handle_error(anyhow!(e)).await` which pushes the user to a fullscreen "Error" route (`app.rs:1805-1809`):

```rust
pub fn handle_error(&mut self, e: anyhow::Error) {
  info!("error occurred: {}", e);
  self.push_navigation_stack(RouteId::Error, ActiveBlock::Error);
  self.api_error = e.to_string();
}
```

Recoverable errors (429, 5xx, network) are intercepted EARLIER per-callsite by string-matching the error message (`playback.rs:374-419`) and showing status-bar toasts instead of the error screen. This works but every endpoint needs to remember which patterns to catch — not robust.

## 14. Notable choices vs original spotify-tui

Backported / Added in the revival (CHANGELOG mining):

- **PKCE auth + ncspot fallback client** (0.36.0)
- **Native librespot streaming** (intro mid-0.34/0.35)
- **Feb 2026 API compat normalizer** (0.36.0)
- **MPRIS Linux + macOS Now Playing + Discord RPC** (0.35.x-0.37.x)
- **Cover art rendering via `ratatui-image`** (0.37.0, feature-gated)
- **Audio visualization via local FFT** (replaces deprecated Audio Analysis)
- **Listening party** via Cloudflare Worker + Durable Object (0.37.1)
- **In-app settings UI** + theme presets (0.37.x-0.38.x)
- **Self-update silent on launch** (0.37.2)
- **Resizable layout** (0.37.1)
- **Stop-after-current-track**, **shuffle-on-startup**, **keepawake** (0.38.x)

Removed / rewritten:

- Audio Analysis API consumption — replaced with local FFT
- Interactive update prompt — replaced with silent auto-update
- Original spotify-tui's `BasicView` renamed to `LyricsView`, art split into `CoverArtView`
- Direct rspotify calls for affected endpoints replaced with raw-reqwest compat path
- `setup_version` migration prompts existing users to redo client setup

## 15. Things to watch

- **TODO/FIXME**: only 7 in the whole tree. Codebase is clean.
- **Deprecated endpoint still active**: `artist_related_artists` (`metadata.rs:54`), `recommendations` (`recommend.rs:41`), `artist_top_tracks` (`metadata.rs:52`). All `#[allow(deprecated)]`. Will silently break for newly-created dev apps.
- **String-matching error classification** is ubiquitous. Brittle if Spotify changes error wording or rspotify wraps errors differently.
- **Single global 250ms pacing mutex** (`requests.rs:13-28`) serializes ALL Spotify API calls through the compat layer. Direct rspotify calls bypass it.
- **No persistent state** — every session re-fetches everything.
- **Mega `App` struct** — 4308 lines, hard to refactor without breakage.
- **Recent direction**: 0.38.x focused on QoL (theme editor, create-playlist UX, keepawake), minor module splits. No fundamental architecture changes pending.

## Adopted by spotuify

1. RecoveringSink panic wrapper around librespot's audio backend (`streaming.rs:33-138`). Verbatim.
2. Per-platform librespot audio backend selection matrix (`Cargo.toml:72-99`).
3. Dual-client + ncspot fallback strategy.
4. Compat payload normalizer pattern (`requests.rs:129-240`).
5. Spirc init with both internal AND outer timeout + abort handle (`streaming.rs:434-466` + `runtime.rs:653-684`).
6. Premium gate before initializing librespot (`runtime.rs:131-179`).
7. Streaming credentials cached as `credentials.json` + audio cache opt-in via librespot's own `Cache::new`.
8. Auto-generated `.gitignore` in user config dir.
9. Hand-rolled redirect HTTP server that filters non-callback requests (`redirect_uri.rs:32-79`).
10. Account-policy-gated streaming + Web API fallback as a deliberate degradation.
11. Token refresh `refresh_token` merge (PR #217).
12. realfft 2048-point FFT pipeline for audio viz.
13. Discord RPC opt-in via feature flag.

## Rejected

1. The 5ms `try_recv` busy-poll on the IO task (`runtime.rs:1007-1020`).
2. One giant `App` struct holding everything (`core/app.rs`, 4308 lines).
3. String-matching error classification (`text.contains("429")`).
4. No persistent cache layer.
5. Two retry layers with different policies.
6. YAML configuration (TOML is more forgiving).
7. Self-update silent on launch.
