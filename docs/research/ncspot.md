# ncspot Deep Study

| | |
|---|---|
| Version sampled | v1.3.3 (CHANGELOG 2026-02-06) |
| Repo | https://github.com/hrkfdn/ncspot |
| Captured | 2026-05-13 |
| Source LoC | ~13,400 across 48 files in `src/` |

> Update 2026-08-12: v1.3.4 is still the latest release. It was a maintenance
> release, so this v1.3.3 architecture study remains useful. Use
> [Why not just use ncspot?](../../site/src/content/docs/guides/why-not-ncspot.md)
> for the current product-level comparison.

Single-binary, single-crate (plus `xtask`), Cursive-based, librespot-embedded TUI. Built for premium Spotify accounts since 2018. ~7 years of accumulated scar tissue.

## 1. Architecture

**Workspace layout:** flat. `Cargo.toml:16-20` declares a 2-member workspace: root crate + `xtask`. No daemon/lib/cli split. Everything in one binary. `xtask` is for dev tooling only (man-page/completion generation).

**Module map** (`src/main.rs:1-40`):
- `application` — event loop, wiring (`src/application.rs`)
- `spotify` — public facade over librespot worker (`src/spotify.rs`)
- `spotify_worker` — the actual librespot thread (`src/spotify_worker.rs`)
- `spotify_api` — rspotify wrapper (`src/spotify_api.rs`)
- `authentication` — OAuth2 flow (`src/authentication.rs`)
- `library` — cache + sync (`src/library.rs`)
- `queue` — playback order (`src/queue.rs`)
- `command` — enum + parser (`src/command.rs`, 793 lines)
- `commands` — dispatch + key bindings (`src/commands.rs`)
- `events` — cross-thread message bus (`src/events.rs`)
- `config` — TOML + CBOR persistence (`src/config.rs`)
- `model/` — Track, Album, Artist, Playlist, Show, Episode, Playable enum
- `ui/` — Cursive views
- `mpris.rs` — D-Bus via zbus, feature-gated
- `ipc.rs` — Unix socket, `#[cfg(unix)]` only
- `panic.rs` — crash-to-file hook because stdout is owned by curses

**Cursive↔tokio bridge** is the cleverest piece. `src/application.rs:60` declares `pub static ASYNC_RUNTIME: OnceLock<tokio::runtime::Runtime>` — a single global multi-thread runtime initialized at startup (`application.rs:87-94`). Synchronous Cursive code calls librespot/rspotify via `ASYNC_RUNTIME.get().unwrap().block_on(...)` or `spawn(...)` (e.g. `spotify.rs:88`, `:96`, `:178`). The main loop in `application.rs:246-298` calls `self.cursive.step()` (which drains its callback queue once non-blockingly) then drains the crossbeam channel from `event_manager`. Worker threads push events via `event_manager.send()`, which does both `tx.send()` AND `cursive_sink.send(Box::new(Cursive::noop))` (`events.rs:49-57`) — the no-op forces Cursive to wake from `step()` immediately so cross-thread events are processed.

**Thread layout:**
- UI thread (main, blocking on `cursive.step()`)
- One tokio multi-thread runtime hosting: librespot worker (`spotify_worker.rs:66-183`), MPRIS server (`mpris.rs:509-517`), IPC accept loop (`ipc.rs:55-59`), per-connection IPC stream handlers, token-refresh blocking task (`spotify_api.rs:98-112`).
- `std::thread::spawn` for: library cache load/fetch (`library.rs:215-296` — 5 sibling threads joined), pagination prefetch (`pagination.rs:142-152`), cover-art HTTP download in notifications.

Hybrid model — OS threads where work is synchronous and CPU/IO heavy (library fetch, cache reads), tokio for everything network/protocol related.

## 2. Auth / credentials

`src/authentication.rs:12-13` hardcodes two client IDs:
- `SPOTIFY_CLIENT_ID = "65b708073fc0480ea92a077233ca87bd"` (librespot session, scopes at `:15-39`)
- `NCSPOT_CLIENT_ID = "d420a117a32841c2b3474932e49fb54b"` (rspotify Web API, scopes at `:41-57`)

**Two separate OAuth2 token caches** — librespot's own cache (`librespot/credentials.json` in user cache dir) plus a `rspotify_token.json` for the Web API. Both via `librespot_oauth::OAuthClientBuilder` (`authentication.rs:110-115`, `:139-156`). They use a random free port for the redirect URI (`authentication.rs:59-65`).

Username/password login was removed in v1.2.2 (CHANGELOG) — Spotify killed it. Only OAuth2 PKCE remains.

`get_credentials()` (`authentication.rs:75-97`) loops `Spotify::test_credentials` (real librespot connect) and re-prompts if creds fail. Token refresh tracked via `WebApi.token_expiration` (`spotify_api.rs:39, 82-112`); refresh fires when <5 min remaining or on 401.

No multi-account / profile support — one set of cached creds per user-cache directory. Multiple instances differentiated only by `process::id()` in the MPRIS bus name (`mpris.rs:571`).

## 3. Playback path

**librespot 0.8.0** (`Cargo.toml:55-58`, confirmed via `Cargo.lock:2128`). Explicitly disable default features on `librespot-playback` and only enable `native-tls` — backends are wired explicitly per-feature.

**Backend selection** (`Cargo.toml:97-111`): each backend (`alsa_backend`, `pulseaudio_backend`, `portaudio_backend`, `rodio_backend`) is a Cargo feature that gates a corresponding `librespot-playback/X-backend` feature. The platform default is `pulseaudio_backend` (Linux). macOS/Windows CI builds use `rodio_backend` (`.github/workflows/ci.yml:31,35`). This keeps build times sane — **only one backend compiles per build**, and the audio libs (alsa-sys, pulse-sys, portaudio-rs) are leaf deps that don't trigger.

Runtime selection at `spotify.rs:209-236` (`init_backend`) — iterates `audio_backend::BACKENDS` (a librespot static), matches by name from config, defaults to first available. Sets Pulse-specific env vars at `:226-233` (`PULSE_PROP_application.name=ncspot` etc.).

**Glue** (`spotify.rs:240-298` `Spotify::worker`):

1. Build `PlayerConfig { gapless, bitrate, normalisation, normalisation_pregain_db }` (`:256-262`).
2. `create_session` (`:184-206`): create `librespot_core::cache::Cache` with creds + volume + optional audio cache (size in MiB), open `Session::new(...)`, `session.connect(creds, true).await`.
3. Create a `SoftMixer` (`:269-273`) — softvol only; no hardware mixer.
4. `Player::new(player_config, session, mixer.get_soft_volume(), backend_factory)` (`:276-281`).
5. Get `player.get_player_event_channel()` (`:282`) — librespot's event stream.
6. Spawn `Worker::run_loop()` (`spotify_worker.rs:66`).

**Worker run_loop** (`spotify_worker.rs:66-183`): single `tokio::select!` over command channel, librespot player events, and a 400ms `time::interval` tick. The interval triggers `events.trigger()` (UI redraw) only while playing (`:176-180`) — clever: no UI thrash when paused/stopped.

**Spirc is NOT used** — ncspot is a player only, not a Connect target/source. They glue `Session` + `Player` directly and manage queueing in their own `Queue` type.

**PlayerEvent translation** (`spotify_worker.rs:122-174`): maps librespot's `Playing/Paused/Stopped/EndOfTrack/TimeToPreloadNextTrack/Seeked` to ncspot's simpler `PlayerEvent::{Playing(SystemTime), Paused(Duration), Stopped, FinishedTrack}` (`spotify.rs:39-45`). `Playing` carries the `playback_start` SystemTime, not elapsed — current position computed from `SystemTime::now() - playback_start`. This avoids the need to tick the position counter; it's derived. `Spotify::get_current_progress` (`spotify.rs:307-313`) sums `elapsed` (last paused position) + time since last play.

**`TimeToPreloadNextTrack` → `QueueEvent::PreloadTrackRequest`** (`spotify_worker.rs:151-154`). Queue's handler (`queue.rs:461-471`) calls `spotify.preload(next_track)` — this is how gapless works through the librespot API.

**Session recovery:** worker checks `session.is_invalid()` at the top of every loop iteration (`spotify_worker.rs:70-74`). When invalid OR player event channel closes (`:170-173`), the loop breaks. On exit, `Spotify::worker` (`spotify.rs:295-297`) clears the worker channel and sends `Event::SessionDied`. Main loop (`application.rs:275-284`) calls `start_worker(None)` to restart; if restart fails, quits.

**Crash isolation:** none. Panic in worker → panic hook writes backtrace to `$CACHE/backtrace.log` (`panic.rs`), terminal recovers somewhat thanks to Cursive's drop, then exit. No worker-as-subprocess.

**Conditional compilation** (`Cargo.toml:97-111`, sprinkled `#[cfg(feature = "...")]`):
- `mpris` (default on, off for Windows), `notify` (default on), `share_clipboard` (default on)
- `cover` (off; ueberzug-based, Linux-only), `share_selection` (Linux/BSD)
- `#[cfg(unix)]` for IPC (`main.rs:36-37`, `ipc.rs`)
- `crossterm_backend` (default), `pancurses_backend` (Windows fallback), `ncurses_backend`, `termion_backend`

**Album art:** `model/track.rs:36` has `cover_url`. Cover screen (`ui/cover.rs`) uses TIOCGWINSZ ioctl + spawns ueberzug subprocess and writes JSON over its stdin. Notification cover (`queue.rs:487-516`): downloads via blocking reqwest (`utils.rs:52-60`), caches under `$CACHE/covers/<basename>`, passes path to notify-rust.

**Crossfade:** not implemented. Only gapless preload.

**ReplayGain:** `normalisation` and `normalisation_pregain_db` exposed via `PlayerConfig` (`spotify.rs:259-260`). librespot handles the actual gain math.

## 4. Web API client

**rspotify 0.15.0** with `client-ureq` + `ureq-native-tls` features (`Cargo.toml:81-84`). They turned off rspotify's `token_refreshing` (`spotify_api.rs:44-46`) and refresh manually because rspotify's auto-refresh uses a different OAuth flow than they want.

**Retry-After handling** (`spotify_api.rs:115-152` `api_with_retry`): single retry on 429, parses `Retry-After` header as u64 seconds, `thread::sleep`s, retries once. On 401, calls `update_token()` and retries once. Other errors: log and return `None`.

**Notable smell:** `thread::sleep` on a 429 in the UI thread (when commands are dispatched synchronously) freezes the TUI for the retry duration. spotuify must NOT do this — async wait + cancel-safe retry needed.

**snapshot_id** plumbed through for playlist mutations. `model/playlist.rs:25` stores it. `WebApi::delete_tracks` (`spotify_api.rs:175-205`) passes `snapshot_id` to `playlist_remove_specific_occurrences_of_items`. Library sync (`library.rs:140-148` `needs_download`) compares local vs remote snapshot_id to decide whether to refetch tracks for a playlist — saves an enormous amount of API traffic on startup since most playlists don't change.

**No ETag / If-None-Match / 304 usage** anywhere in the codebase. snapshot_id is the only freshness primitive used. Also a clever "list-unchanged" check for saved tracks at `library.rs:499-514`: if page 0 has same length and IDs as local store and `total == local.len()`, skip refetch. (Bug-prone — total can be stable while order changes, but they don't care about order for the saved-tracks store.)

**Deprecated endpoints still called:** `artist_related_artists` (`spotify_api.rs:678-682`, `#[allow(deprecated)]`). `recommendations` (`spotify_api.rs:324-353`) — also officially gone for new apps Nov 2024 but still called from "similar tracks" feature.

**Manual pagination wrapping** (`ui/pagination.rs`): `ApiResult` + `ApiPage` types + `FetchPageFn` boxed closure form a generic over rspotify's `*_manual` pagination methods, allowing lazy infinite scroll in list views with a separate `std::thread::spawn`-based prefetcher (`pagination.rs:137-153`).

## 5. Local cache / state persistence

Caches live in user cache dir (`platform-dirs` crate, `config.rs:298-311`).

Files:
- `librespot/credentials.json` (librespot's own credential cache)
- `librespot/files/` audio cache (if enabled, size-bounded via config `audio_cache_size` MiB)
- `librespot/volume` (librespot persistent volume)
- `rspotify_token.json` (Web API token)
- `tracks.db`, `albums.db`, `artists.db`, `playlists.db` — JSON-serialized via serde_json (`library.rs:24-34`, `:96-136`). Read via `std::fs::read_to_string` + `serde_json::from_str` (`library.rs:107-108`), explicit comment that string-based is faster than serde_json reader.
- `covers/<filename>` (cached cover JPEGs)
- `backtrace.log` (panic dumps)

User state in config dir:
- `config.toml` (TOML, user-edited; `serialization.rs:39-69`)
- `userstate.cbor` (CBOR via `serde_cbor 0.11.2`; `lib.rs:7`, `config.rs:213-217`). Stores volume, shuffle, repeat, queue state (with track progress!), playlist sort orders, cache_version.

The `cache_version` field (`config.rs:17 CACHE_VERSION: u16 = 1`, checked in `library.rs:97-103`) lets them invalidate caches on breaking schema changes — when the constant bumps, all caches are ignored on next start.

**Queue persistence across restarts** is real (`config.rs:138-144 QueueState`, `application.rs:144-163`): on quit (`commands.rs:113-126`), they snapshot the queue, current index, random order, and current progress, write to CBOR. On startup, they load the snapshot back into Queue::new (`queue.rs:52-63`), then re-`load` the current track into the player with `position_ms = saved_progress` (`application.rs:148-153`).

Per-playlist sort order also stored: `UserState.playlist_orders: HashMap<String, SortingOrder>` (`config.rs:153`).

JSON-file-per-collection is clean but doesn't scale to large libraries.

## 6. Search

100% remote, via Web API (`spotify_api.rs:357-375` `WebApi::search`). No local search index. Submitted via the search EditView on Enter (`ui/search.rs:42-52`) — creates a `SearchResultsView` which fires the search synchronously through the rspotify call (blocking; runs on the UI thread). **No debouncing.** User has to type then hit Enter.

Vim-like in-list search (`/foo`) is a substring match over loaded items (`ui/listview.rs:46`, `search_indexes`) — purely client-side.

## 7. Commands and key bindings

**Command enum** at `command.rs:117-160` — 38 variants. `Display` impl (`:162-228`) round-trips to the textual command form (used for `help` view + key-binding equivalence).

**Parser** at `command.rs:362-793` is a hand-written `match` on the head token. Argument parsing inline. Multi-command via `;` separator with `;;` escape (`:362-386`). Comparable in complexity to a small Roff parser — lots of `BadEnumArg/InsufficientArgs/ArgParseError` error variants.

`parse_duration 2.1.1` (`Cargo.toml:62`) gives them human durations on `seek +1m30s` (`command.rs:454`).

**Bindings** stored in `CommandManager.bindings: RefCell<HashMap<String, Vec<Command>>>` (`commands.rs:40`). Defaults at `commands.rs:409-...`. User overrides via `[keybindings]` in `config.toml` — string key like `"Shift+i"` parsed by `parse_keybinding`. Multiple commands per key supported (`commands.rs:78-86`, value is `;`-separated). To unbind: bind to `noop`.

**Reload**: `Command::ReloadConfig` (`commands.rs:213-235`) re-reads config, rebuilds theme, `unregister_keybindings` + `register_keybindings`. Users can hot-reload without restart.

**Modes:** no explicit modal state machine. The `:` and `/` prefixes are handled by `Layout::enable_cmdline(prefix)` (`ui/layout.rs:108-113`); the `EditView`'s `set_on_submit` callback (`layout.rs:54-90`) strips the prefix, parses, and dispatches. Escape clears. Insert/search modes inside lists are encapsulated per-view (e.g. `ui/listview.rs` has its own search state).

**Dispatch:** every key event goes through `CommandManager::handle` (`commands.rs:362-370`) → `handle_callbacks` (`:331-360`) which tries the active context-menu / modal first, then `s.on_layout(|l| l.on_command(...))` (which walks current view stack), then `handle_default_commands`. Result is a `CommandResult` (`commands.rs:31-36`) — `Consumed/View/Modal/Ignored`.

## 8. UI patterns

`ui/layout.rs:24-37` — central `Layout` view holding `screens: HashMap<String, Box<dyn ViewExt>>`, a per-screen view stack (`stack`), statusbar, cmdline EditView. Pushing/popping detail views = pushing onto the per-screen stack.

`ui/listview.rs:40-53` is the workhorse list. It owns `content: Arc<RwLock<Vec<I>>>` (shared with the source-of-truth e.g. `Library`/`Queue`), per-list `selected`, `search_query`, `scroller`, and a `Pagination<I>` that fires on scroll-near-end. `try_paginate()` is called in `layout()` to prefetch.

Pagination loads next batch on background thread (`pagination.rs:142-152`) — busy flag prevents reentry.

Error / status messages are written into `Layout.result: Result<Option<String>, String>` (`layout.rs:31-32`) and `result_time: Option<SystemTime>` — shown in cmdline area for a few seconds then fade.

Help (`ui/help.rs`) is built from `bindings.borrow().clone()` (`commands.rs:208-212`) — generated listing of every active binding-to-command mapping.

Theming: `theme.rs:39-90` parses optional hex/name colors out of `ConfigTheme`, falls back to defaults.

## 9. MPRIS / IPC / external control

**MPRIS** (`src/mpris.rs`, 575 lines):
- `zbus 5.14.0` with default-features = false + `tokio` (`Cargo.toml:50`)
- Bus name: `format!("org.mpris.MediaPlayer2.ncspot.instance{}", process::id())` (`:570-574`) — multi-instance safe.
- `MprisRoot` (`:33-70`) and `MprisPlayer` (`:72-469`) interfaces.
- Property getters delegate to `Spotify`/`Queue`/`Library`; signals (`PropertiesChanged`) explicitly emitted from a worker thread driven by `MprisCommand` enum (`:474-483`).
- `MprisManager::new` spawns a tokio task (`:509-517`) — owns the zbus connection, loops over `MprisCommand`s, calls `player_iface.playback_status_changed(ctx)` etc.
- `Spotify::update_status` calls `send_mpris(EmitPlaybackStatus)` (`spotify.rs:378-380`). Volume changes notify only when `notify=true` (HACK comment at `:482-487` — prevents loops when MPRIS itself set the volume).
- `OpenUri` (`mpris.rs:379-468`) handles full Spotify URI playback via Queue manipulation.
- `Seeked` signal emitted via `MprisCommand::EmitSeekedStatus(micros)` (`:467-473`).

Media keys: MPRIS-mediated — works on Linux DEs that wire media-keys to MPRIS, doesn't on macOS/Windows (no native MediaSession integration).

**Unix domain socket IPC** (`src/ipc.rs`, 138 lines, `#[cfg(unix)]`):
- Path: `$RUNTIME_DIR/ncspot.sock` (`application.rs:166-178`, `utils.rs:67-117` for runtime-dir lookup). Falls back from `$XDG_RUNTIME_DIR` → `/run/user/$uid/` → `/tmp/ncspot-$uid/`.
- Multi-instance: if existing socket is responsive, new instance creates `ncspot.<pid>.sock` (`ipc.rs:35-44`). Stale sockets are deleted.
- Wire format: newline-delimited via `tokio_util::codec::LinesCodec` (`:98-99`). Bi-directional: incoming lines are commands (sent via `Event::IpcInput` to main loop, parsed and run); outgoing is JSON `Status { mode: PlayerEvent, playable: Option<Playable> }` published on every player state change.
- Uses `tokio::sync::watch` channel (`:53`) so multiple IPC clients all see latest status without queueing.
- Per-connection task (`:81-87`) spawned on each `listener.accept()`.

Simpler than a daemon model — ncspot is one process, the socket is just a remote-control surface. The line-delimited JSON pattern and the runtime-dir-not-cache-dir lesson are both worth copying.

## 10. Notifications

`notify-rust 4` with `z` feature (zbus, not dbus) (`Cargo.toml:90-95`), feature-gated `notify` (default on). Sent from `queue::send_notification` (`queue.rs:486-516`):
- title/body from `[notification_format]` config (`config.rs:60-73`), default `title=%title body=%artists`.
- cover URL downloaded sync via blocking reqwest, cached, path passed to `n.icon()`.
- XDG-specific (`:503-506`): urgency=Low, transient hint, desktop entry hint — so notifications don't accumulate and group nicely.
- Behavior identical whether ncspot is focused or not — `notify-rust` doesn't know about focus. Linux + macOS supported; macOS doesn't get the XDG hints.

## 11. Configuration

TOML config + CBOR runtime state (separate files). `config.rs:76-104 ConfigValues` lists every key — flat schema. All fields are `Option<T>` (no defaults at deserialization; defaults applied lazily where read). No serde validation — invalid TOML aborts with error message; invalid enum values silently fall through (e.g. `theme.rs` warns and falls back).

Runtime reload via `:reload` command, also rebuilds theme + rebinds keys.

`reload` doesn't restart the worker — changing `backend` or `bitrate` doesn't take effect until restart.

## 12. Lyrics

Not implemented. Zero hits for `lyrics` in source.

## 13. Testing

Tests are sparse — only `src/queue.rs:519-829` (~310 lines, 30 tests) for queue mutation logic, plus a couple of `from_str` smoke tests in `spotify.rs:551-573` and `spotify_url.rs:82+`. Total: ~34 `#[test]` items.

Test infra is minimal — `Config::new_for_test`, `EventManager::new_for_test`, `Spotify::new_for_test`, `Library::new_for_test` (`queue.rs:518-573`) — all just construct default state, skipping network entirely. No mocks; no integration tests; no async tests; no UI tests.

CI matrix (`.github/workflows/ci.yml:14-69`): 4 build targets (linux-x86_64, linux-arm64, macos-aarch64, windows-x86_64). Each runs `cargo build && cargo test`. Plus `cargo fmt --check` and `cargo clippy --all-targets --all-features -- -D warnings`.

**Test discipline is weak** for code that's been around since 2018. They've leaned on user bug reports + Cursive's lack of headless harness.

## 14. Error handling

No `thiserror`/`anyhow` — they use `Box<dyn Error>` and stringified errors liberally (e.g. `Result<(), String>` returned from `main`/many APIs). `CommandParseError` is a hand-rolled enum (`command.rs:313-360`) with `fmt::Display` — clean and informative.

Web API errors are mostly swallowed and logged (`spotify_api.rs:147-149`). API calls return `Result<T, ()>` — caller has no info about why something failed. From a UX perspective, the error is silently dropped to the log file.

`panic.rs` registers a custom backtrace hook because stdout is owned by Cursive — backtrace goes to `$CACHE/backtrace.log` and `panic_info` is appended.

## 15. Notable lessons from CHANGELOG (7+ years)

- **v1.3.1, v1.3.2, v1.3.3** all contain "Spotify API change" bug fixes — Spotify has broken playback/metadata fetch multiple times in 2024-2025. They survive by depending on `librespot` for protocol and letting that crate's maintainers handle the drift.
- **v1.2.2**: removed username/password support; OAuth2 only.
- **v1.2.0**: librespot 0.5 upgrade fixed file CDN download, AP connection handling, frequent disconnects.
- **v1.1.0**: "Complete freeze when ncspot was running for a long time" — probably the 401-retry loop without proper token refresh.
- **v1.0.0**: moved IPC socket from cache dir → runtime dir.
- **v0.13.x**: introduced `reconnect` command after network-change problems.
- **v0.12.0**: IPC socket added; crash dump moved to file.

The pattern: most fixes are operational (auth, connection, persistence, recovery), not new features. **Long-lived TUIs spend their maintenance budget on session/auth/connection management, not UI.**

## Adopted by spotuify

1. Worker pattern: `tokio::select!` over command channel + librespot event channel + interval tick that only fires when playing.
2. Per-resource OS thread for library cache load/fetch fan-out.
3. snapshot_id-aware playlist sync as refetch gate.
4. Saved-tracks unchanged shortcut.
5. `cache_version` constant for schema invalidation.
6. CBOR for runtime state, TOML for user config.
7. Multi-instance MPRIS bus naming (`instance$pid`).
8. Line-delimited JSON over Unix socket in runtime dir (not cache dir).
9. `parse_duration` for human seek values.
10. `reload` + `reconnect` commands.
11. Backtrace dump to file on panic.
12. notify-rust with XDG hints (urgency=Low, transient, desktop-entry).

## Rejected

1. Blocking `thread::sleep` on Retry-After.
2. `Result<T, ()>` for Web API calls.
3. JSON-per-collection cache files.
4. Hand-rolled 793-line command parser.
5. Hardcoded client IDs in source.
6. Two separate OAuth2 flows.
7. No integration / API tests.

## Quick facts

| Key | Value |
|---|---|
| librespot | 0.8.0 |
| rspotify | 0.15.0, ureq-native-tls |
| Cursive | 0.21.1 |
| Tokio | 1.x, `rt-multi-thread`, `sync`, `time`, `net` |
| Rust edition | 2024 |
| zbus | 5.14 with tokio backend |
| crossbeam-channel | 0.5 |
| Release profile | `lto=true, codegen-units=1`; separate `optimized` profile keeps incremental fast |
| Source LoC | ~13,400 across 48 files |
| Workspace | 2 members (root + xtask) |
| CI | ubuntu-latest, ubuntu-24.04-arm, macos-14, windows-latest |
