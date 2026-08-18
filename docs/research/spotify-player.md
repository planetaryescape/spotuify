# spotify-player Deep Study

| | |
|---|---|
| Version sampled | v0.23.0 |
| Repo | https://github.com/aome510/spotify-player |
| Captured | 2026-05-13 |
| Recent | "Add confirmation popups on destructive actions (#966)" |

> Update 2026-08-12: v0.24.1 is still the latest release. This document
> preserves the v0.23.0 source study. Current source still uses the chunked
> localhost UDP command socket, but authentication now uses two independent
> OAuth flows and album art now uses `ratatui-image`. Use
> [Why not just use spotify-player?](../../site/src/content/docs/guides/why-not-spotify-player.md)
> for the current product-level comparison.

## 1. Architecture

**Workspace layout.** Two crates: `spotify_player` (bin) and `lyric_finder` (lib). Root `Cargo.toml` declares `members = ["spotify_player", "lyric_finder"]`, `resolver = "2"`, and workspace-level lints (deny `pedantic`, deny `unsafe_code`). `lyric_finder` is a standalone Genius-scraping lib but is **no longer wired into the binary** — current lyrics come from librespot's mercury bus. They keep it published as a side crate.

**Module boundaries** under `spotify_player/src/`:
- `main.rs` — thread spawning, panic hook, daemonize, rustls install, tokio runtime entry
- `auth.rs` — librespot OAuth wrapper, hardcoded client IDs (Spotify web + ncspot fallback)
- `client/` — `AppClient` over rspotify, request enum, custom `Spotify` impl, request handler loop, player-event watcher
- `cli/` — clap definitions, **UDP socket** server/client, JSON request/response
- `config/` — TOML configs (`app.toml`, `keymap.toml`, `theme.toml`), `OnceLock<Configs>`
- `state/` — `SharedState = Arc<State>`, with `Mutex<UIState>`, `RwLock<PlayerState>`, `RwLock<AppData>` (all `parking_lot`)
- `event/` — terminal event handler (crossterm), `page.rs`/`popup.rs`/`window.rs` dispatchers
- `ui/` — ratatui render loop, page renderers, FFT visualizer
- `streaming.rs` — librespot Spirc/Player wiring
- `media_control.rs` — souvlaki MPRIS/SMTC integration

**"Daemon" is just fork.** The biggest surprise: there is **no separate daemon binary, no IPC client/server split**. The "daemon" feature in `main.rs:307-322` is just `daemonize::Daemonize::new().start()?` — a fork-then-detach pattern on Linux. The same process either runs TUI + background threads, or just the background threads. The "IPC" for CLI subcommands is a **UDP socket on 127.0.0.1:8080** that the live process always listens on (`main.rs:154-160` calls `cli::start_socket`). The CLI subcommand handler in `cli/handlers.rs:146-181` (`try_connect_to_client`) pings UDP; if `ConnectionRefused`, it **spawns its own short-lived runtime inside the CLI invocation** with `tokio::runtime::Runtime::new()` and starts the same socket server thread.

**Mode dispatch.** `main.rs:287` — `match args.subcommand()`: `None` = run app (TUI or daemonized), `Some(cmd)` = CLI subcommand path that pipes JSON over UDP and prints the response.

## 2. Auth / config

**No PKCE redirect listener in this codebase.** `auth.rs:100-109` uses `librespot_oauth::OAuthClientBuilder` — librespot owns the redirect listener at `http://127.0.0.1:8989/login` (configurable via `login_redirect_uri`). Two clients exist:

- librespot OAuth client → uses Spotify's official web client ID `65b708073fc0480ea92a077233ca87bd` (`auth.rs:6`) — this returns librespot credentials cached to `~/.cache/spotify-player/credentials.json` via librespot's `Cache::new()`
- rspotify `AuthCodePkceSpotify` (only if user configured a custom `client_id`) — uses standard PKCE, cached to `user_client_token.json` in cache folder (`client/mod.rs:80-100`)

The default `client_id` in the TOML config (`config/mod.rs:297`) is **ncspot's** `d420a117a32841c2b3474932e49fb54b`, with comments noting it predates the Nov 2024 web-API quota changes and avoids extended-quota mode.

**Token refresh.** `client/spotify.rs:86-102` implements `BaseClient::refetch_token` which calls `token::get_token_rspotify(session)`. `token.rs:8-46` uses librespot's `session.login5().auth_token()` with a 5-second timeout (`TIMEOUT`); on timeout, it calls `session.shutdown()` to force re-init. rspotify's `Config::token_refreshing = true` (in `client/spotify.rs:43`) drives automatic refresh — every `http_get` calls `self.token()` which calls `self.auto_reauth()` (`client/mod.rs:114-125`). No proactive scheduled refresh; refresh happens on demand before each request.

**Config files** at `~/.config/spotify-player/` — `app.toml`, `keymap.toml`, `theme.toml`. Parsed via `config_parser2` crate (their own derive) on top of `toml`. There's a `-o KEY=VALUE` CLI override mechanism (`config/mod.rs:526-553`) that round-trips through TOML's `Value` tree for dot-path overrides. Very nice.

**Credentials.** No keyring. Plain JSON in cache folder.

## 3. Playback path

**librespot 0.8.0** (workspace pinned, from `Cargo.lock`). `librespot-playback` set to `default-features = false, features = ["native-tls"]` to avoid rustls conflict.

**Audio backend** selected at compile time via Cargo features (`Cargo.toml:80-91`): `rodio-backend` (default), `alsa`, `pulseaudio`, `portaudio`, `jackaudio`, `rodiojack`, `sdl`, `gstreamer`. `compile_error!` if `streaming` is on but no backend is selected (`streaming.rs:19-39`).

**Session/Spirc/Player wiring** in `streaming.rs:142-281`:

- Convert config volume 0-100 to librespot's 0-65535 (`streaming.rs:155`)
- Build `ConnectConfig { name, device_type, initial_volume, is_group: false, disable_volume: false, volume_steps: 64 }`
- `SoftMixer::open(MixerConfig::default())` — hardcoded soft mixer; no hardware mixer support
- `audio_backend::find(None)` — picks the compile-time-linked backend
- `PlayerConfig { bitrate, normalisation, ..Default::default() }` — bitrate from config (default 320), normalisation from `device.normalization`
- `player::Player::new(player_config, session, mixer.get_soft_volume(), || -> Box<dyn Sink> { ... })` — sink factory closure, where they inject a FFT-tapping wrapper sink (`VisualizationSink`) when audio visualization is enabled

**PlayerEvent consumption** at `streaming.rs:217-263`: tokio task pulls from `player.get_player_event_channel()`, maps librespot's `player::PlayerEvent` to a smaller internal enum (Changed/Playing/Paused/EndOfTrack), updates `state.player.buffered_playback.is_playing`, then optionally executes a shell hook from config (`player_event_hook_command`). The shell-hook pattern is great for extensibility.

**Spirc reconnect.** `streaming.rs:267-276` — `Spirc::new` returns `(spirc, spirc_task)`. They `tokio::select!` on the spirc_task OR the player_event_task — if either ends, the task ends. **No automatic reconnect** beyond librespot internals. When session goes invalid, `client/handlers.rs:28` checks `client.check_valid_session(state).await` before each client request, which calls `new_session` (which rebuilds spirc).

**Playback control routing.** All playback commands go through `client/mod.rs:251-358` (`handle_player_request`) and call rspotify's **Web API** methods like `self.start_context_playback`, `self.next_track` — **not direct librespot calls**. So even with embedded librespot, commands flow through Spotify's HTTP API, then the local Spirc receives the command from Spotify cloud and acts on it. **Critical:** they use librespot purely as a Connect device endpoint.

**Crash isolation.** A panic in `streaming.rs`'s tokio tasks aborts that task only because of the `panic` hook in `main.rs:88-93` which writes a backtrace file but doesn't abort. librespot panics in audio callbacks can kill the audio backend thread silently.

## 4. Spotify Web API client

**rspotify 0.15.3** with `cli` feature. They use `AuthCodePkceSpotify` for user-configured client IDs only; for the default ncspot path they implement their **own** `Spotify` struct (`client/spotify.rs`) that implements `BaseClient` and `OAuthClient` traits from rspotify. The custom impl avoids rspotify owning the OAuth — they get tokens from librespot's session via `login5().auth_token()` (avoids needing a second OAuth flow). `OAuthClient::get_oauth` and `request_token` impls `panic!("should never be called!")`.

**Rate-limiting handling: NONE.** Grep for `retry`, `Retry-After`, `429`, `TooManyRequests` returns nothing in the source. They rely entirely on rspotify's built-in behavior. `client/mod.rs:1476-1512` (`http_get`) just bails on non-200. **Clear gap.**

**snapshot_id handling.** Stored on `Playlist` (`state/model.rs:176`). Forwarded as `Option<String>` only to `reorder_playlist_items` (`client/mod.rs:1158-1195`). Not used for optimistic concurrency on add/remove. Used as a tiebreaker for playlist sort order.

**ETag / If-None-Match: NONE.**

**Pagination.** `client/mod.rs:1514-1569` (`all_paging_items`) implements offset-based pagination with **parallel fan-out**: up to `MAX_PARALLEL: usize = 8` concurrent page fetches via `futures::future::try_join_all`, with `PAGE_LIMIT: usize = 50`. The parallelism is great for initial library load but blows the rate budget on a cold start.

**Deprecated endpoints.** They DO NOT call `audio-features` or `recommendations` (grep confirms). For "radio", they use librespot's **mercury bus** (`hm://autoplay-enabled/query?uri=…` then `hm://radio-apollo/v3/stations/…`) at `client/mod.rs:949-1019`. This bypasses the Web API entirely. Smart workaround — the recommendations endpoint was killed in Nov 2024 and they pivoted to the Spotify-internal mercury endpoints that the official client uses.

For `browse/categories/{id}/playlists`, they bypass rspotify and use raw `http_get` due to a known rspotify bug.

## 5. Local cache / state

**File caches.** `state/data.rs:200-232` — `store_data_into_file_cache` / `load_data_from_file_cache`. Each cache key (`FileCacheKey` enum: Playlists, PlaylistFolders, FollowedArtists, SavedShows, SavedAlbums, SavedTracks) writes one JSON file like `Playlists_cache.json` in `~/.cache/spotify-player/`. `AppData::new` (`state/data.rs:78-85`) loads them on startup.

**TTL in-memory cache.** `MemoryCaches` in `state/data.rs:49-75` uses `ttl_cache::TtlCache` (3rd-party crate) with capacity 64 and `TTL_CACHE_DURATION = 1 hour`. Caches contexts (playlists/albums/artists), search results, lyrics, genres, and rendered images. Liked tracks (`USER_LIKED_TRACKS_URI`) bypass the context cache and always refresh (`client/mod.rs:493-496`) to keep `saved_tracks` synchronized.

**No SQLite.** Everything is JSON on disk or HashMap/TtlCache in RAM. The image cache is on-disk under `~/.cache/spotify-player/image/` keyed by `"{album}-{artist}-cover-{id_prefix}.jpg"`.

**Playlist imports** (`cli/client.rs:721-866`) maintain a per-(from,to) diff cache as hashed sets of TrackData in JSON files under `~/.cache/spotify-player/imports/{to_id}/{from_id}`. Diff-based one-way sync — only NEW tracks are added on each `sync`; with `--delete`, tracks gone from source since last import are also removed from target.

## 6. Search

**Remote only.** `client/mod.rs:1022-1090` (`search`) calls `tokio::try_join!` of `search_specific_type` for each of Track/Artist/Album/Playlist/Show/Episode (6 parallel requests). Results cached in `MemoryCaches::search` (TTL 1 hour). No local Tantivy or any index.

**Local fuzzy filtering** for library/menus only, via `fuzzy-matcher` crate (skim's algorithm) at `utils.rs:53-70`. Feature-gated as `fzf`. Used for filtering already-loaded lists, not for searching the catalog.

**Debouncing.** None — search fires on Enter (`event/page.rs:217-222`). No search-as-you-type.

## 7. CLI surface

**Subcommands** (`cli/mod.rs:164-223` + `cli/commands.rs`):
- `get key {playback|devices|user-playlists|user-liked-tracks|user-saved-albums|user-followed-artists|user-top-tracks|queue}`
- `get item {playlist|album|artist|track} [--id ID | --name NAME]`
- `playback {start|play|pause|play-pause|next|previous|shuffle|repeat|volume|seek}` with subcommands
- `playback start {track|context|liked|radio}`
- `connect [--id ID | --name NAME]`
- `like [--unlike]`
- `playlist {new|delete|list|import|fork|sync|edit}`
- `search QUERY`
- `lyrics [--id|--name]`
- `authenticate`
- `generate {bash|zsh|fish|...}` for shell completions
- `features` prints compiled-in features

**Output format.** JSON for `get`, free-form text for playlist/lyrics. No table mode, no `--format json|yaml|table` flag. Responses go through a `Response::{Ok(Vec<u8>)|Err(Vec<u8>)}` envelope.

**IPC protocol.** UDP localhost, max 4096-byte requests (`cli/mod.rs:9` MAX_REQUEST_SIZE). Server-to-client responses chunked at 4096 bytes with an empty packet as terminator. Newlines are **literal `\\n`** in stdout, fixed by `replace("\\n", "\n")` at the receiver — a hack.

**UDP is wrong.** UDP is unreliable; localhost is reliable in practice but you lose ordering guarantees for chunked responses larger than ~64KB. Anything bigger than a small playlist will tear. They likely picked UDP for simplicity but a Unix socket / named pipe is correct. Their "any process bound to the port = the server, otherwise spawn one" via `try_connect_to_client` is a port-stealing race condition.

## 8. TUI patterns

**ratatui 0.30.0 + crossterm 0.29.0**, both very recent.

**Render loop** (`ui/mod.rs:36-76`): tight sleep loop in a dedicated `ui` thread (`main.rs:194-197`), wakes every `app_refresh_duration_in_ms` (default 32 ms ≈ 30 fps), grabs the `ui` mutex, clears + redraws the whole frame each tick. No dirty regions, no per-widget invalidation. Image rendering is the one exception — they track `last_cover_image_render_info` and reset on terminal resize.

**State management.** Three locks under `Arc<State>`: `Mutex<UIState>`, `RwLock<PlayerState>`, `RwLock<AppData>`. All `parking_lot`. The pattern is "many small locks held briefly during one tick". No redux-like reducers; mutations happen inline where they're triggered. Three primary threads: tokio runtime (client handler + socket), UI thread (render), event handler thread (crossterm input), plus a player-event-watcher OS thread that polls. Channel is `flume::unbounded::<ClientRequest>`.

**Key bindings: action registry in config.** `config/keymap.rs` defines `Keymap { key_sequence, command }` and `ActionMap { key_sequence, target, action }`. Default keymap is a long literal Vec in `KeymapConfig::default()` (`config/keymap.rs:33-470`). Users add/override via `~/.config/spotify-player/keymap.toml`. Supports multi-key sequences (`g space`, `u p`, `g t`). Count prefixes are supported — `5j` moves down 5 (`event/mod.rs:170-189`). Command palette popup launched with `?`.

**Image rendering** via `viuer = "=0.9.2"` (pinned because newer versions cause freezing — see comment at `Cargo.toml:46`). Supports kitty, iTerm2, sixel. Detection happens once on startup at `main.rs:101-108`. They have a `pixelate` mode that renders cover art as a low-res grid.

**Mouse support.** Click on progress bar to seek. Scroll wheel changes volume. No click-to-select rows.

**Confirmation popups.** Just added in commit #966 — destructive actions (delete playlist, etc.) route through `PopupState::ConfirmAction { message, action: ConfirmableAction, ... }`.

## 9. Lyrics

**Synced lyrics from Spotify's own backend** via `librespot-metadata`. `client/mod.rs:642-661` (`AppClient::lyrics`):
```rust
match librespot_metadata::Lyrics::get(&session, &id).await {
    Ok(lyrics) => Ok(Some(lyrics.into())),
    Err(err) => if err.to_string().to_lowercase().contains("not found") { Ok(None) } else { Err(err.into()) },
}
```
This hits a mercury endpoint (`hm://lyrics/...`). Lines convert to `(chrono::Duration, String)` via `state/model.rs:748-765`, sorted by start time. Bidirectional text support via `unicode-bidi` (RTL languages).

Alignment is line-level. UI renders current line by binary-search-by-progress (in `ui/page.rs:579+`).

`lyric_finder` (Genius scraper) published as separate crate but unused.

## 10. Notifications / MPRIS

**MPRIS / SMTC / mac NowPlaying** via `souvlaki = "0.8.3"` (cross-platform abstraction). `media_control.rs` registers MediaControls with `dbus_name: "spotify_player"` and listens for `Play/Pause/Toggle/SetPosition/Next/Previous/SetVolume` events. Updates pushed every 1 second (`media_control.rs:145` — explicit cap to avoid overloading souvlaki's Linux dbus handler).

**Windows/macOS quirk.** Souvlaki on those platforms needs a real window handle. They create a hidden winit window or a dummy Win32 message-only window (`media_control.rs:160-263`). On macOS/Windows daemon mode this feature is incompatible — they `eprintln!` and `std::process::exit(1)` at `main.rs:311-316`.

**Linux notifications** via `notify-rust` (`client/mod.rs:1821-1903`). Formatted from a user-defined `notify_format.summary` / `notify_format.body` template with `{track}`, `{artists}`, `{album}` substitutions via a hand-rolled regex tokenizer. Cover image attached via filesystem path.

**Media keys** are handled inside the MPRIS/SMTC path — no separate hotkey daemon.

## 11. Testing

**Minimal.** Two test modules:
- `client/mod.rs:1977-2010` — three tests on `move_seed_track_to_front`
- `state/queue.rs:400-660` — extensive (~20) unit tests for the CustomQueue advance/retreat/shuffle state machine

**No integration tests, no API mocks, no end-to-end playback tests.** CI just runs `cargo test`, `cargo fmt --check`, `cargo clippy -D warnings`, on macOS/Windows/Ubuntu × stable Rust. Plus `cargo-machete` for unused deps and `crate-ci/typos`.

## 12. Error handling

**`anyhow` everywhere.** No `thiserror`, no domain error types, no recoverable/fatal distinction. `anyhow::bail!` for "fail this operation", `?` propagates up to the request handler in `client/handlers.rs:36-44` which logs with `tracing::error!` and continues the loop. User-facing errors in the TUI just log to the log buffer / file; they're not shown in the UI directly except via the logs page.

CLI errors return as `Response::Err(Vec<u8>)` and print to stderr.

The panic hook at `main.rs:84-93` writes a backtrace file in cache folder.

## 13. Performance

**Async / tokio.** `tokio = { version = "1.50.0", features = ["rt", "rt-multi-thread", "macros", "time"] }`. Multi-threaded runtime. Blocking work runs on dedicated OS threads (UI render, event handler, player event watcher), not on tokio. `parking_lot` locks throughout.

**No backpressure.** `flume::unbounded` channel means a slow client handler can grow memory unboundedly.

**Wasteful retrieve-loop.** `update_playback` (`client/mod.rs:668-688`) and `initialize_playback` (`client/mod.rs:128-177`) both spawn a task that pings Spotify 5 times at 1-second intervals to deal with Spotify's eventual-consistency on playback state. That's 5 extra API calls per playback change.

**No persistent rate-limit budget tracker.**

**Image rendering on every redraw** is avoided by tracking `last_cover_image_render_info` and only re-rendering on terminal resize.

**Pagination concurrency** (8-wide) on cold start makes initial library load fast but can exhaust the rate budget; combined with no Retry-After handling this is a real risk.

## 14. Notable Cargo deps & gotchas

- `rustls = "0.23.37"` with `default-features=false, features=["ring"]` — manually installed in `main.rs:239-241` because librespot's hyper-rustls requires it. Comment notes "TODO: see if this can be fixed upstream".
- `librespot-playback` uses `native-tls` (not rustls) to avoid the conflict.
- `viuer = "=0.9.2"` is **pinned exactly** because newer versions freeze the TUI (linked to issue #899).
- `vergen = "=9.0.6"` pinned to avoid breakage in issue #914.
- `maybe-async = "0.2.10"` — for the rspotify trait impls (`client/spotify.rs:68`).
- `souvlaki = "0.8.3"` — cross-platform media controls.
- `flume = "0.12.0"` — chosen over `tokio::sync::mpsc` and `crossbeam-channel` for cross-thread+cross-runtime use.
- `config_parser2 = "0.1.7"` — their own derive crate for TOML deep-merge.
- `daemonize = "0.5.0"` only on `feature="daemon"`.
- `rustfft = "6"` only on `feature="streaming"` for the visualizer.
- `winit = "0.30.13"` only on Windows/macOS for the hidden window pattern.

## Adopted by spotuify

1. Sink-wrapper pattern for taps (`streaming.rs:200-213` passes a sink-factory closure to librespot's `Player::new`).
2. Action registry + multi-key sequences with count prefixes (`event/mod.rs:170-189` + `config/keymap.rs`). Vim-style `5j`, `g space`, etc.
3. Per-(from→to) playlist import diff cache (`cli/client.rs:721-866`).
4. Sink soft-mixer + per-device-name registration.
5. Shell-hook for player events (`config.player_event_hook_command`).
6. Pinned-version + comment-with-issue-link for tricky deps.
7. `-o KEY=VALUE` CLI config override.
8. `pulseaudio-backend` env-var injection (`main.rs:114-139`).
9. `login5().auth_token()` to bridge librespot session → Web API token (avoids second OAuth flow).
10. Mercury bus access (lyrics, autoplay, radio-apollo) for endpoints Spotify killed.
11. Buffered-playback mirror state pattern (`state/player.rs:34-58`).
12. Confirmation popups on destructive actions (#966).

## Rejected

1. UDP for IPC. Loses ordering on chunked responses; `\\n` literal-escape workaround is a tell.
2. No rate-limit handling at all.
3. JSON-blob file caches. Atomic-rewrite-the-world.
4. No tests for the HTTP layer.
5. 5x retrieve-current-playback after every action.
6. `viuer` (`=0.9.2` pin) — prefer `ratatui-image`.
