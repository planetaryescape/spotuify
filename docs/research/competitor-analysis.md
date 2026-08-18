# Competitor Analysis (Synthesis)

> Cross-cutting comparison of the three active Rust Spotify TUIs in 2026, mapped to spotuify's design decisions.

Captured 2026-05-13. Re-validate before treating as current.

Update 2026-08-12: release facts and the public comparison pages were checked
again against upstream source. ncspot remains v1.3.4 and spotify-player remains
v0.24.1. The landscape and architecture tables below now describe those
releases and spotuify v0.1.84. The deeper numbered findings still preserve the
May source study unless a later note explicitly supersedes them.

spotify-player v0.24.1 still uses its chunked localhost UDP command socket. It
now documents two independent OAuth flows, one for the Web API and one for the
librespot session, and uses `ratatui-image` for album art. The earlier v0.23
single-token bridge remains useful historical lineage, not current upstream
behavior.

Update 2026-05-28: the first-party/keymaster auth idea was built far enough
to keep as an opt-in experiment, but D016 superseded it as the default.
Current code defaults to user dev-app PKCE (`client_id` in config or
`SPOTUIFY_CLIENT_ID`) because sustained Web API polling through keymaster is
rate-limited harder. `login5().auth_token()` remains a useful pattern for
first-party mode and future native-session reads, not the default Web API path.

Update 2026-06-03: spotuify removed macOS Keychain/keyring storage. Auth now
lives in private config-dir files (`<config_dir>/auth/*.json`, `0700` auth dir
and `0600` files on Unix), matching the power-user CLI trade-off used by the
other terminal clients while keeping an auto-generated config-dir `.gitignore`.

## The landscape

| Project | Maintained | Stack | Playback | CLI surface | Local cache | Search | Differentiation today |
|---|---|---|---|---|---|---|---|
| **spotify-tui** | abandoned 2021-11 | Ratatui | Web API control only | minimal | none | remote | Historical canonical TUI; broken by Nov-2024 API changes |
| **ncspot** | active (v1.3.4 May 2026) | Cursive | librespot 0.8 embedded | none beyond TUI | JSON-per-collection | remote only | Mature, long-lived, MPRIS-rich |
| **spotify-player** | active (v0.24.1 July 2026) | Ratatui | librespot 0.8 embedded + optional daemon | substantial (`get`, `playback`, `search`, etc.) | JSON-blob files + TTL memory cache | remote + local fuzzy filter | Closest in-domain analog to spotuify's daemon model |
| **spotatui** | active (v0.38.2 May 2026) | Ratatui | librespot 0.8 embedded | minimal | none | remote only | Newest entry, audio viz, listening party |
| **spotuify** | active (v0.1.84 August 2026) | Ratatui | librespot embedded in daemon | CLI + MCP + TUI + macOS client | **SQLite + Tantivy** | **local hybrid** | Daemon-owned state, local analytics, operation log, and provider capability contract |

## Architecture comparison

| Dimension | ncspot | spotify-player | spotatui | spotuify |
|---|---|---|---|---|
| Workspace | root + xtask | bin + unused lyric_finder lib | single binary | **18 workspace packages**: root binary + 17 crates |
| Daemon | none — TUI process holds IPC socket | "daemon" = `daemonize::start()` fork; same binary holds UDP port | none | unified binary; detached daemon process over UDS or Windows named pipe |
| IPC | line-delimited JSON over UDS (Linux+macOS) | **UDP localhost** (4KB max), chunked responses with `\\n` literal escapes | none | length-prefixed JSON over UDS + Windows named pipe |
| Multi-client | broadcast `tokio::sync::watch` | best-effort UDP | n/a | request/response correlation + event broadcast |
| Auth | two OAuth flows (librespot + rspotify) | two independent OAuth flows (Web API + librespot session) | two flows | dev-app PKCE default; first-party/login5 opt-in |
| Credential storage | librespot Cache + JSON | librespot Cache + JSON | JSON file + `.gitignore` autogen | private config auth files + `.gitignore` autogen |
| Rate-limit handling | blocking `thread::sleep` (freezes UI) | none | two-tier inconsistent | unified middleware + jittered backoff (Phase 6) |
| snapshot_id | used as refetch gate during sync | stored, only used for reorder | not visible | refetch gate + operation-rollback token (Phase 6/12) |
| ETag / If-None-Match | not used | not used | not used | per-row freshness w/ ETag (Phase 6) |
| Local search index | none | local fuzzy filter only | none | **Tantivy** with hybrid local+remote (existing Phase 3) |
| MCP server | no | no | no | **yes** (Phase 8) |
| Operation log + undo | no | no | no | **yes** (Phase 12) |
| Analytics derivations | no | no | no | **yes** (Phase 10) |
| MPRIS / SMTC / NowPlaying | zbus directly (Linux) | souvlaki (cross-platform) | souvlaki + macos_media | souvlaki + per-OS fallbacks (Phase 14) |
| Lyrics | none | mercury bus (Spotify-internal) | LRCLIB | **mercury + LRCLIB + cache** (Phase 16) |
| Cover art rendering | basic (ueberzug subprocess) | `ratatui-image` | `ratatui-image` | `ratatui-image` (Phase 15) |
| Audio visualization | no | FFT via sink-wrapper | FFT via system loopback | **hybrid** sink-tap + loopback (Phase 17) |
| Discord RPC | no | no | yes (opt-in) | optional via feature flag (Phase 14) |
| Shell hooks | no | yes (`player_event_hook_command`) | no | yes (Phase 14) |
| Tests | thin (queue only) | thin (queue + few model tests) | thin (~179 tests, pure functions) | conformance + fake-Spotify CLI tests + integration |

## Critical patterns identified across all three

### Adopted by spotuify

1. **`librespot 0.8` for embedded playback.** All three use it. Pin to specific version; track upstream.
2. **`vergen` trio pinning** (`=9.0.6` + `=9.1.0` + `=1.0.8`) required by librespot-core 0.8's build.rs.
3. **`login5().auth_token()` to bridge librespot session → Web API token** — one OAuth flow instead of two. Learned from the v0.23 spotify-player snapshot; v0.24.1 moved to two independent flows. Later revised by D016: keep this as opt-in/future until spotuify can route reads through native session channels instead of sustained Web API polling.
4. **Two-client_id strategy** — hardcode an official streaming-scoped client_id (use `65b708073fc0480ea92a077233ca87bd` per spotify-player), allow user override for Web API.
5. **Per-platform audio backend matrix** (alsa Linux GNU, rodio Linux musl + Windows, portaudio macOS) — Windows MUST NOT use `pipe` backend (corrupts TUI); macOS rodio SIGSEGVs on AirPods disconnect.
6. **RecoveringSink panic wrapper** with `catch_unwind` around audio backend `start/stop/write` — adopted verbatim from spotatui. Essential for AirPods / PipeWire / WASAPI resilience.
7. **Spirc dual-timeout (inner 30s + outer abort)** — distinguishes auth failure (clear creds, retry once) from transient timeout (don't clear).
8. **Premium gate before librespot init** — `GET /me` before Session::new; librespot panics process on Free accounts.
9. **Sink-factory closure for taps** — chain wrappers for FFT visualization, analytics, scrobble counting. Don't fork librespot.
10. **`TimeToPreloadNextTrack` → `player.preload(next)`** for gapless playback. From ncspot.
11. **Worker `tokio::select!` over command + PlayerEvent + interval-only-when-playing** — ncspot's pattern, saves CPU when paused.
12. **Position-from-SystemTime offset** — `current = playback_start_systemtime.elapsed()` instead of ticked counter. Avoids off-by-one bugs.
13. **`PlayerEvent` stream as primary truth** — Web API polling becomes fallback only.
14. **Mercury bus for endpoints Spotify killed in Nov 2024** — `hm://lyrics/...`, `hm://autoplay-enabled/query`, `hm://radio-apollo/v3/stations/`, `hm://similarity-bff/v1/related-artists/`. spotify-player precedent.
15. **Pulse env vars** (`PULSE_PROP_application.name=spotuify` etc.) for nice pavucontrol display on Linux.
16. **snapshot_id as refetch gate** during sync — saves 95%+ of `/playlists/{id}/tracks` requests when nothing changed. ncspot pattern.
17. **Saved-tracks unchanged shortcut** — compare page-0 ids+total. ncspot pattern.
18. **Compat normalizer for Spotify payload drift** — walk JSON, backfill missing keys (`available_markets`, `external_ids`, `linked_from`, `popularity`), then deserialize. From spotatui's Feb-2026 fix.
19. **Token-refresh `refresh_token` merge** — if Spotify omits `refresh_token` from refresh response, merge from cache. spotatui PR #217.
20. **Auto-generated `.gitignore` in config dir** to hedge dotfile-sync token leaks.
21. **`cache_version` constant** with startup gate, bumped on schema break. ncspot pattern.
22. **`User-Agent` header** on every outbound HTTP call.
23. **Backtrace dump on panic** to file because stdout is owned by TUI.
24. **Confirmation popups on destructive actions** — TUI parity with MCP `confirm: true`. spotify-player commit #966.
25. **`reload` command for hot config reload.** ncspot.
26. **`reconnect` command** to manually rebuild session after network change. ncspot.
27. **`-o key.path=value` global CLI flag** for one-shot TOML override. spotify-player.
28. **Multi-instance MPRIS bus naming** (`spotuify.instance{pid}`). ncspot.
29. **Action registry + multi-key sequences + count prefixes** — vim-style `5j`, `g space`, etc. spotify-player's full default keymap as code.
30. **Shell-hook for player events** for Last.fm/ListenBrainz/tmux/Hammerspoon. spotify-player precedent.
31. **Cover-art file cache** with file:// paths to notify-rust and souvlaki. ncspot+spotify-player pattern.
32. **LRC parser** with binary-search alignment, RTL via `unicode-bidi`. spotify-player.
33. **CD matrix**: x86_64-linux-gnu, aarch64-linux-gnu, x86_64-linux-musl, x86_64-apple-darwin, aarch64-apple-darwin, x86_64-pc-windows-msvc. cargo-deb + AUR + Homebrew + Scoop + Nix flake.
34. **macOS codesign + notarize.** spotatui.

### Rejected by spotuify (with reason)

1. **UDP for IPC** (spotify-player). Loses ordering on chunked responses; `\\n` literal-escape workaround is a tell. Unix socket + length-prefixed framing is correct.
2. **Blocking `thread::sleep` on Retry-After in request path** (ncspot). Freezes UI. Use async wait + cancel-safe retry + token-bucket budget.
3. **`Result<T, ()>` for Spotify API calls** (ncspot). Loses error context — UI cannot discriminate rate-limit / auth / not-found / network. Use typed `SpotifyError` enum.
4. **String-matching error classification** (`err.to_string().contains("429")`, spotatui throughout). Brittle. Pattern-match on typed error.
5. **JSON-blob-per-collection caches** (spotify-player, ncspot). Atomic-rewrite-the-world; no indexing; no transactions; no point-in-time. SQLite + Tantivy is strictly better.
6. **Two separate OAuth flows for librespot vs Web API** (ncspot, spotatui). Double browser prompt; confuses users. Bridge via `login5().auth_token()`.
7. **One 4308-line `App` god struct** (spotatui). Refactor blocker. Workspace split + per-feature state slices is the correct shape.
8. **Hand-rolled 793-line command parser** (ncspot). Use a parser combinator or generate from a schema. Adding commands compounds linearly.
9. **5x retrieve-after-action polling** (spotify-player). Use `PlayerEvent` stream as truth (Phase 6); fall back to polling on disagreement only.
10. **Two-tier inconsistent rate-limit handling** (spotatui — raw-reqwest path respects Retry-After but rspotify-direct path doesn't). Route every call through one middleware tier.
11. **Loose plaintext token storage in config dir** (all three). Accept the file-backed CLI trade-off, but make it explicit: private auth directory, mode `0600` files on Unix, atomic writes, lock file, and `.gitignore` autogen.
12. **5ms `try_recv` busy-poll on IO task** (spotatui `runtime.rs:1007-1020`). Use proper `tokio::select!`.
13. **Hardcoded retry counts** (spotify-player's "5 retries every time" pattern). Adaptive retry with rate-limit awareness.
14. **YAML config** (spotatui). TOML is better — more forgiving, less indentation-sensitive, supported by `config_parser2` for nested overrides.
15. **Self-update silent on launch** (spotatui). Surprising behavior in a CLI tool. Make explicit.

## Key product gaps in all three

Three observations cut across the field. Each is a spotuify differentiation opportunity:

### 1. No agent-friendly interface

None of the three exposes anything like MCP. spotatui's CLI emits only format-string output (`%t %a` placeholders), no JSON. spotify-player has a `get` subcommand with JSON output but no schema, no tool-call discovery, no live resource subscriptions. ncspot has only a status-broadcast IPC socket — no request/response.

**Implication:** spotuify with Phase 8 (MCP server) is uniquely positioned. The Rust ecosystem has no Spotify MCP; the Python ones are Web-API-only, no local cache, no librespot, no analytics.

### 2. No correctness-first sync layer

All three play fast-and-loose with rate limits, payload drift, and schema invalidation. ncspot blocks the UI on Retry-After; spotify-player has no handling at all; spotatui's two-tier system is internally inconsistent. None has true freshness signals on cached rows; none has a per-operation `snapshot_id` rollback path.

**Implication:** Phase 6 (sync hardening) plus Phase 12 (operation log with snapshot_id-per-op) build a layer none of the competitors offer — long-running daemons that don't degrade, mutations that can be rolled back, and clean reactions to Spotify's silent schema changes.

### 3. No first-class local analytics

All three log raw playback events at best. None have derived listen facts, top-N queries, habit rollups, or external scrobbling integration beyond best-effort shell hooks.

**Implication:** Phase 10 (analytics derivations) gives users Wrapped-style data continuously, without depending on Spotify's once-a-year compilation. Combined with sink-tap audible-time measurement (Phase 9) and the shell-hook bridge (Phase 14), spotuify becomes the only Spotify TUI/CLI where this is first-class.

## Stack alignment

spotuify's existing stack already matches the 2026 state of the art:

| Dep | spotuify | competitors | Notes |
|---|---|---|---|
| ratatui | 0.30 | spotify-player 0.30, spotatui 0.30, ncspot uses Cursive 0.21 instead | Aligned |
| crossterm | 0.29 | matching | Aligned |
| tokio | 1 full | matching | Aligned |
| reqwest | 0.12 rustls | matching | Aligned |
| sqlx | 0.8 sqlite | none use SQL | **spotuify ahead** |
| tantivy | 0.22 | none use Tantivy | **spotuify ahead** |
| ratatui-image | (Phase 15) | spotatui uses it; spotify-player uses `viuer` instead | Right choice |
| souvlaki | (Phase 14) | spotify-player uses it | Right choice |
| notify-rust | (Phase 14) | ncspot + spotify-player both | Right choice |
| librespot | (Phase 9) | all three pin 0.8.x | Align on 0.8 |

## Distribution stack target

Match competitor coverage:

- Homebrew tap (matches spotatui, spotify-player)
- AUR (matches ncspot, spotify-player)
- Scoop manifest (matches spotify-player)
- cargo-deb (matches spotatui)
- Nix flake (matches spotatui)
- GH Releases with checksummed artifacts (all three)
- Cross-platform binaries: 6 targets

## Decisions captured

- **D008** (existing): copy mxr's daemon/IPC/store/search patterns; we extended this to copy spotify-player/ncspot/spotatui's librespot patterns.
- **D010** (Phase 13): embed librespot. Confirmed by all three competitors.
- **D011** (Phase 13): MCP server. No competitor does this — clear differentiator.
- **D012** (Phase 13): operation log + undo. No competitor does this.
- **D013** (Phase 13): HealthClass cardinality.
- **D014** (Phase 13): competitor study; record this analysis as background for future decisions.

## Re-validation plan

These competitor reports go stale fast. Schedule a re-study every 6 months or when a major change is signaled:

- New Spotify API deprecation announcement (recurring risk; Nov 2024 was the last major one).
- librespot major version bump (0.8 → 0.9, etc.).
- MCP spec update.
- Any of the three competitors becoming inactive or new entrants appearing.

When re-running: clone fresh to `/tmp/spotuify-research/`, repeat the deep-study agent runs against the same brief (see commit history for the prompt templates), diff the findings against this document, update the per-repo files and this synthesis.
