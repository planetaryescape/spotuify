# spotuify - Decision Log

This document records settled decisions so future agents do not re-litigate them without new evidence.

## D001: Architecture - daemon-backed, not TUI-owned

Chosen: daemon-backed runtime with CLI/TUI clients.

Considered:

- monolithic TUI that calls Spotify directly
- CLI-only controller
- daemon-backed runtime

Why:

- music must continue after TUI exits
- CLI and agents need the same capabilities
- local cache/search requires background work
- TUI state should not be durable app state

## D002: CLI is canonical

Chosen: CLI-first product surface.

Why:

- every action becomes testable
- agents can use the app safely
- scripts and pipelines become first-class
- TUI-only features are hard to verify and easy to break

## D003: Playback device - use Spotify Connect, not Web API audio

Chosen: controller plus Spotify Connect device.

Why:

- Spotify Web API does not stream audio
- embedded librespot or official apps are the playback devices
- spotuify should control Spotify Connect devices; D010 later made embedded librespot the shipped local device

## D004: Search - local first, Spotify remote as provider

Chosen: SQLite/Tantivy local search plus live Spotify search.

Why:

- saved library and playlist data should be instant
- remote API is rate-limited and occasionally flaky
- agents need repeatable search results

## D005: Output formats are stable product contract

Chosen: table/json/jsonl/csv/ids on data commands.

Why:

- Unix composition
- agent integration
- testability
- less screen scraping

## D006: Lyrics are optional provider, not core Spotify feature

Chosen: no core lyrics promise until a legal/provider-backed source exists.

Why:

- Spotify Web API does not expose official lyrics
- unreliable scraping would make the player feel broken

## D007: TUI UX follows contextual action registry

Chosen: action registry drives hint bar, command palette, help, and command availability.

Why:

- prevents hidden keymap mystery
- keeps hints contextual
- makes CLI/TUI parity auditable
- supports future configurable keymaps

## D008: Implementation strategy - copy mxr before inventing

Chosen: copy/adapt mxr implementations for daemon, IPC, SQLite, Tantivy, CLI output, mutation discipline, and TUI async/action plumbing wherever the shape matches.

Considered:

- greenfield spotuify-specific architecture
- copying mxr first, then extracting shared crates after repetition is proven
- extracting shared crates before spotuify uses the patterns

Why copy first:

- mxr has already paid the design/debugging cost for local daemon architecture
- daemon/IPC/store/search mechanics are nearly identical across these terminal-native apps
- copy/paste/adapt is faster and safer than designing abstractions too early
- after two or three apps share the same shape, extraction targets become obvious

Future extraction candidates:

- local JSON IPC codec/client/server
- daemon lifecycle and socket management
- CLI output rendering formats
- mutation preview/confirmation/receipt helpers
- TUI action registry, keymaps, hint bar, command palette
- SQLite/Tantivy sync/index scaffolding

Do not abstract before the second real use case proves the seam.

## D009: TUI-only actions must stay client-scoped

Chosen: actions that touch Spotify, cache, search, playlist, queue, device, or daemon state need a CLI equivalent. TUI-only actions are allowed only for client-local navigation, discovery, input, selection, and layout state.

Current TUI-only actions:

- `Command Palette` - client discovery surface
- `Help` - client help overlay
- `Quit TUI` - closes the TUI client only
- `Move Down` - client navigation state
- `Move Up` - client navigation state
- `Page Down` - client navigation state
- `Page Up` - client navigation state
- `Jump Top` - client navigation state
- `Jump Bottom` - client navigation state
- `Back` - client navigation state
- `Filter Current List` - client-side visible-list filter
- `Cancel Input` - client text input state
- `Mark Item` - client multi-select state
- `Mark Range` - client multi-select state
- `Clear Marks` - client multi-select state
- `Toggle Player Size` - client layout preference
- `Expand Rail` - client layout preference
- `Devices` (quick-pick overlay) - client overlay shortcut

Why:

- these actions do not mutate reusable app state
- daemon IPC should not expose screen cursor, modal, hint, or layout state
- CLI parity remains mandatory for reusable music capabilities

## D010: Embedded librespot (Phase 9, decision gate)

Chosen: embed librespot in the daemon and ship it as the only supported playback backend. The old spotifyd and Connect-only backend choices are not supported runtime modes.

Why:

- All three active Rust Spotify TUIs (ncspot, spotify-player, spotatui) embed librespot 0.8.x; the install story improves from "install + configure spotifyd separately" to a single binary
- Sub-100ms playback control via direct `Spirc`/`Player` API instead of multi-second Web API roundtrips
- librespot's `PlayerEvent` stream replaces 60s polling for playback truth (per Phase 6)
- Mercury bus access unlocks lyrics + radio + related-artists endpoints Spotify killed in November 2024

Trade-offs accepted:

- Cargo tree grows ~30-40%, binary size from a few MB to ~25-40MB
- Audio-backend bugs come in-house (CoreAudio quirks on mac, PipeWire/PulseAudio selection on linux)
- librespot protocol drift maintenance now ours rather than spotifyd's release cycle
- Mitigated by spatatui's `RecoveringSink` pattern wrapping the backend Sink in `catch_unwind`
- Users who kept `[spotifyd] device_name` rely on a legacy config shim; no spotifyd process is started.

Implementation lands in Phase 9; not part of the current Phase 6/7/8 batch.

Implementation status (updated 2026-05-28):

- `BackendKind` accepts only `embedded`; `spotifyd` and `connect` parse as errors.
- `EmbeddedBackend` registers the local Spotify Connect device, wires the sink chain, forwards transport commands, and translates librespot player events.
- `MockPlayerBackend` remains test-only.
- Config still reads legacy `[spotifyd] device_name` as a fallback for existing installs.
- Foundations for Phase 9.3 — `RecoveringSink` (catch_unwind with
  rolling panic budget), `Clock` trait + position-as-SystemTime
  derivation (NTP-step safe), worker `tokio::select!` loop
  (interval ticks only when playing) — all unit-tested.
- Foundations for Phase 9.4 — `MercuryFetcher` trait + TTL cache,
  `TokenBridge` (5s timeout, graceful refresh fallback) — both
  unit-tested.
- Audio backend matrix: `alsa-backend`, `pipewire-backend`,
  `rodio-backend`, `portaudio-backend` Cargo features; `compile_error!`
  guard when `embedded-playback` is enabled without one selected.
  Linux pulse env vars set on `EmbeddedBackend::new`.
- vergen pin deviation: the planning doc called for
  `vergen=9.0.6 + vergen-lib=9.1.0 + vergen-gitcl=1.0.8`. In practice
  vergen 9.0.6 is the right pin because vergen-gitcl 1.0.x is
  internally on vergen-lib 0.1.x; mixing in 9.1.x of vergen-lib
  produces two coexisting versions and breaks `librespot-core`'s
  build script. Comment lives in the workspace `Cargo.toml`.

## D011: MCP server as a first-class spotuify surface (Phase 8)

Chosen: ship `spotuify-mcp` as a workspace crate and a separate binary, exposing the daemon's Request set as Model Context Protocol tools and resources over stdio (default) or HTTP.

Why:

- No prominent Rust-native Spotify MCP exists in 2026; the Python servers (varunneal, tylerpina, Carrieukie) are Web-API-only with no local cache, no librespot playback, no analytics
- The daemon already speaks length-delimited JSON over local IPC with typed Request/Response/Event; exposing the same types as MCP tools is incremental
- LLM clients (Claude Code, Cursor, Continue) can consume spotuify as a tool without shelling out
- Mercury-bus tools (lyrics/radio/related-artists, Phase 9 gated) and analytics tools (Phase 10 gated) give MCP clients capabilities the Python servers can't match

Discipline:

- Destructive tools (`playlist_create`, `playlist_add`, `library_save`, etc.) require explicit `confirm: true` in args. Without it the bridge returns a preview. Mirrors spotify-player commit #966 at the MCP layer.
- `undo_last` bypasses confirm -- it IS the safety net.
- Tools deferred to later phases surface a clear `LocalDeferred` marker rather than silently failing.

Pure-function core (tool catalogue, confirm gating, request bridge) tested with 31 unit tests; insta golden manifest snapshot locks the public tool surface so additions/renames are always a code-review event. The rmcp wire integration (stdio + HTTP transport) lands as a follow-up on top of the same core.

## D013: HealthClass has three variants (Phase 13)

Chosen: `HealthClass { Healthy, Degraded, Unhealthy }`.

Considered:

- two variants (Healthy/Degraded only)
- three variants (Healthy/Degraded/Unhealthy)
- four variants (mirroring mxr's `Healthy/Degraded/RestartRequired/RepairRequired`)

Why three:

- Two variants conflated "running with a soft failure" with "cannot reach Spotify at all". Operators and monitoring scripts need to act differently on those.
- Four variants over-fit the email-client domain (mxr); spotuify's recovery path is `daemon restart` or `login` re-auth in either case, so RestartRequired vs RepairRequired didn't pay rent.
- Doctor election is now: any `Error` finding → Unhealthy, any `Warning` → Degraded, else Healthy.

Implementation lands in `crates/spotuify-protocol/src/lib.rs` (enum) plus `crates/spotuify-daemon/src/diagnostics.rs:finalize_report` (election).

## D014: Competitor study citation (Phase 13)

Chosen: record the open-source Rust Spotify TUIs/MCP servers we studied and the patterns adopted from each. The blueprint cribbed liberally; this entry locks the provenance.

Sources studied (2025–2026):

- `ncspot` — cursive-based TUI; lifted: per-playlist `snapshot_id` as concurrency token (`model/playlist.rs:25`), MPRIS via direct zbus (`src/mpris.rs`), `panic.rs` terminal-restoration hook, `reload` and `reconnect` commands (`commands.rs:213-235`, `application.rs:275-284`).
- `spotify-player` — ratatui TUI + Connect API client; lifted: souvlaki media-controls + hidden-window pattern (`src/media_control.rs:160-263`), shell `player_event_hook_command` (`src/streaming.rs`), `-o key.path=value` config override (`config/mod.rs:526-553`), confirmation popups on destructive actions (commit #966 → Phase 13's TUI modal + Phase 8 MCP confirm gate).
- `spotatui` — Connect + analytics TUI; lifted: auto-`.gitignore` in config dir (`core/config.rs:99-115`), `RecoveringSink` (catch_unwind panic budget for librespot, Phase 9.3), Discord Rich Presence pattern (`infra/discord_rpc.rs`), macOS NowPlaying scaffolding (`infra/macos_media.rs`).
- `mxr` (planetaryescape) — email client; lifted: file-polling `logs tail --follow` loop (`crates/daemon/src/commands/logs.rs:48-142`), `bug-report` assembly + redaction (`crates/daemon/src/commands/bug_report.rs:57-216`), clap-built-in `generate completions` (`crates/daemon/src/commands/completions.rs`), JSON-to-file + text-to-stdout tracing layering pattern (`crates/daemon/src/lib.rs:965-1006`), undo-window snapshot/restore pattern (`crates/store/src/undo.rs`, adapted in spotuify-daemon/src/undo.rs).
- `jj` (mercurial-style VCS) — adopted `op log` + `op undo` model whole. The DAG-of-views richness was not adopted; spotuify uses a linear op log with `subject_op_id` linkage so the schema stays SQLite-friendly.

Date recorded: 2026-05-14.

## D012: Operation log + undo (Phase 12)

Chosen: every daemon mutation records an `operations` row with a reversal plan, surfaced via `spotuify ops log` / `spotuify ops undo` and the MCP `undo_last` tool.

Why:

- Phase 8 lets LLMs mutate state; without undo, a misfired tool call is unrecoverable without manual SQL or Spotify-app intervention
- jj's `op log` + `op undo` pattern is the established 2026 shape for "I let an agent do things and want a back button"
- Phase 6's two-stage receipts already capture mutation intent; the operations table extends it with persistent reversal plans plus snapshot_id concurrency tokens for safe rollback

Implementation lands in Phase 12; not part of the current Phase 6/7/8 batch.

## D015: First-party (keymaster) Web API auth (2026-05-24)

Status: superseded by D016.

Chosen: drop the per-user Spotify Developer app as the default. spotuify
logs in with librespot's first-party "keymaster" client id
(`65b708073fc0480ea92a077233ca87bd`) via `librespot-oauth`, and mints the
Web API bearer from the live librespot session with
`Session::login5().auth_token()`.

Why:

- Spotify put dev-mode apps behind a 5-user allow-list AND blocked
  playlist writes for them (Feb 2026). Verified 2026-05-24: a dev-app
  token gets `403 Forbidden` on `POST /users/{id}/playlists` and
  `POST /playlists/{id}/tracks`; the keymaster token gets `429`
  (authorized, only rate-limited). Allow-listing + re-login did not help.
- This is what every working terminal client does (spotify-player,
  ncspot). The keymaster client is never in Development Mode.
- It also deletes spotuify's worst onboarding step — there's no client_id
  to register/paste. One browser login and you're in.

How (as built):

- `login5().auth_token()` is the primary bearer source (full scope,
  re-mintable from the live session without a browser, survives
  keymaster-OAuth-endpoint outages). The raw `librespot-oauth` access
  token (refreshed via `refresh_token_async`) is the bootstrap +
  fallback — it's a valid full-scope bearer on its own (probe-proven).
- The bearer reaches the Web API client through a `WebApiBearerProvider`
  trait (`spotuify-spotify`), implemented in the daemon by minting via
  the player actor's `PlayerBackend::web_api_token()` (login5). The
  entire legacy dev-app PKCE path is left intact behind this seam.
- Persistence: only the librespot-oauth refresh token is stored
  (`FirstPartyCredentials` in `<config_dir>/auth/first-party.json` with
  mode 0600 on Unix). The bearer is never persisted; reusable native
  playback credentials live in librespot's own cache.
- Opt-out: set `SPOTUIFY_CLIENT_ID` (env) to use your own Spotify app
  (legacy dev-app flow). The opt-out is the **env var**, not a config
  client_id — the old onboarding wrote the user's dev-app id into the
  config, so keying off the config value would strand existing users on
  the broken flow. Env-only opt-out migrates everyone to the fix and
  lets the next launch send them through the browser login.
- Scope-drift banner is suppressed in first-party mode: login5 tokens
  always report empty scopes, so the check would fire a permanent false
  "run spotuify login".

Full staged plan: `docs/blueprint/auth-rework-plan.md`.

## D016: Dev-app PKCE remains the default auth path (2026-05-26)

Chosen: revert first-party/keymaster auth to opt-in and keep the per-user
Spotify Developer app PKCE flow as the default.

Why:

- Sustained Web API polling through keymaster gets policed harder than the
  per-user dev-app budget. It fixed the Development Mode write policy problem
  but introduced a worse rate-limit posture for normal daemon sync.
- The first-party path is still valuable once reads can move through
  librespot-native session channels instead of heavy `api.spotify.com`
  polling. Until then, it remains gated by `SPOTUIFY_USE_FIRST_PARTY=1`.
- Default dev-app auth has sharper operational edges, so the token store must
  be treated as shared mutable state: a private 0600 auth file, a
  cross-process lock, refresh-token replacement persistence, and `invalid_grant`
  purge/fail-fast behavior.

Current behavior:

- `Config::load()` requires `client_id` from config or `SPOTUIFY_CLIENT_ID`.
- `Config::is_first_party()` returns true only when
  `SPOTUIFY_USE_FIRST_PARTY=1`.
- Default credentials are `StoredToken` values in `<config_dir>/auth/token.json` with mode 0600 on Unix.
- First-party credentials are separate `FirstPartyCredentials` values in `<config_dir>/auth/first-party.json`.

## D017: Artist discography browsing with a daemon-tagged library filter (2026-06-05)

Chosen: surface an artist's full discography behind one request, grouped by
Spotify's `album_group`, with an "in library vs all" filter computed as a local
view over daemon-owned data rather than a separate query.

Why:

- Spotify buries an artist's catalog several screens deep and offers no
  "only what I have saved" filter. A flat command plus one toggle is the gap.
- There is no per-artist library endpoint. "In my library" can only be computed
  by intersecting an artist's album ids against the user's saved albums. The
  daemon already caches saved albums, so it tags each discography album with
  `in_library` once and clients filter that single payload with no refetch. This
  keeps the daemon as the state owner and the toggle as a pure client view.
- Fetching with `market=from_token` collapses the per-market duplicate rows the
  endpoint otherwise returns; remaining re-releases are de-duplicated by id.

Current behavior:

- New core requests: `ArtistAlbums { artist }` returns the full discography
  tagged with `album_group` and `in_library`; `FollowedArtists { limit }` is
  cache-backed and falls back to a live `/me/following` fetch when cold.
- New optional `MediaItem` fields `album_group` and `in_library` (skip-if-none,
  wire-compatible). They flow live from provider to client and are not persisted
  to the cache.
- Followed artists sync into `library_items` with `followed = 1` (a dedicated
  persist path, so they are not mismarked as saved albums).
- CLI: `spotuify artist albums <uri> [--library-only] [--group <g>]` and
  `spotuify artist followed`. TUI: the artist overlay groups releases into
  sections with `L` toggling the library filter. macOS: an Artists sidebar entry
  plus a grouped artist page with an All / In Library segmented control.
- IPC protocol version moved to 4 (this bundles the listening-reminders surface
  added in the same line of work). Older daemons fail the client version gate
  until rebuilt.

## D018: Cross-platform IPC keeps one protocol over platform transports (2026-06-09)

Chosen: keep the daemon wire protocol as length-delimited JSON, with
`spotuify-protocol::ipc_stream` hiding the platform transport. Unix builds use
Unix-domain sockets. Windows builds use Tokio named pipes.

Why:

- The daemon, CLI, TUI, MCP bridge, tests, and fake-provider smoke should share
  one codec and one Request/Response/Event contract.
- Windows should not force a TCP loopback fallback unless named pipes prove
  unusable. A local named pipe keeps the daemon off the network.
- Transport-specific behavior stays below the protocol. On Windows the listener
  creates the next pipe instance before handing the connected pipe to a task, so
  clients do not hit a gap between accepts.

Current behavior:

- `.github/workflows/ci.yml` checks, tests, builds, and fake-smokes
  `x86_64-pc-windows-msvc`.
- `.github/workflows/release.yml` publishes
  `spotuify-v{version}-windows-x86_64.zip`.
- Windows remains beta until real login, daemon startup, playback, and Task
  Scheduler install are verified on a Windows machine.

Out of scope for v1: fuzzy re-release matching (a deluxe or remastered edition
with a different album id can read as "not in library"); strict id matching is
used instead. A `/me/albums/contains` fallback for a cold saved-album cache is
deferred.

## D018: Update-awareness + cross-show episode feed (2026-06-07)

Decision: the daemon owns an update check and a podcast episode feed; clients are
views. Protocol bumped 5 to 6 (additive: `check-update` / `update-available` /
`update-status`, `episode-feed`, and a `date` search sort).

Rationale:

- Update check lives in the daemon so a single periodic GitHub call (startup, then
  every 6h, bounded 4s/8s timeouts) serves every client. It emits
  `UpdateAvailable` once per newer release and answers `CheckUpdate` from cache.
  The daemon derives the upgrade command from the running exe path
  (Homebrew / cargo / DMG / dev), so each client renders the right action.
- mxr deliberately avoids phone-home; we honor that ethos by contacting only the
  public, unauthenticated GitHub releases API, sending no identifying data, and
  making it opt-out via `SPOTUIFY_NO_UPDATE_CHECK`. Surfaced in CLI
  (`spotuify update`), the TUI banner, and a macOS banner + Settings toggle.
- The episode feed fans out `show-episodes` over the followed shows (bounded
  concurrency, first page each), merges, and caches the merged set for 15 min;
  sort + limit are applied per request. CLI: `spotuify episodes --sort …`.

Out of scope: sorting podcasts by "tags" or genres. Spotify's API exposes none on
shows or episodes (only release date, duration, title, show name, publisher,
played state), so the available-field sorts ship instead. User-applied local tags
would be a separate feature and were not built.

## D019: Audit-driven removals and won't-do markers (2026-06-10)

A full-codebase audit drove a backlog of fixes. The decisions below record
what was deliberately removed or declined so they don't get re-litigated.

Decision: **remove the `analytics export` / `analytics import` CLI + protocol
surfaces.** They only ever returned a "scrobble-bridge follow-up" error. An
in-tree provider bridge would mean storing third-party credentials and tracking
ListenBrainz/Last.fm API drift; the shell-hook recipes in `docs/recipes/` are the
supported live-scrobbling path. Removed `Request::AnalyticsExport`/`AnalyticsImport`,
`ExportTarget`, both CLI subcommands, the daemon bail arm, and the round-trip test.
MCP never exposed them, so no agent surface changed.

Won't-do (explicitly declined; revisit only on validated demand):

- **Row thumbnails** in search/playlist lists — visual noise + maintenance cost
  without a validated need (see Phase 15 cover-art notes).
- **Manual lyrics provider selection** — automatic mercury→LRCLIB fallback stands
  until there's a need to override it (Phase 16).
- **Native PipeWire visualizer capture** — cpal monitor capture already works over
  PipeWire/Pulse; a native dependency is not worth the marginal latency win.
- **AUR + Scoop packaging** — external-repo distribution, tracked outside this repo.
- **MCP resource push over HTTP** — the HTTP transport has no SSE by design; live
  push subscriptions ship stdio-only.

Accepted as-is (with code comments, no change):

- The IPC frame cap stays at 16 MiB (named `MAX_IPC_FRAME_BYTES`): album-art and
  large `ClientSeed` payloads are legitimate, and the socket is local-only 0600.
- Stale tantivy lock removal is not fsynced: the startup preflight re-runs every
  launch, so a resurrected lock is cleared on the next start.

## D020: Per-request IPC timeouts; dispatch split stays incremental (2026-06-10)

Decision: bound every IPC request at the daemon layer. `guard_ipc_response`
now wraps each handler in a category-aware `tokio::time::timeout`
(`DEFAULT_REQUEST_DEADLINE` 30s; `MAINTENANCE_REQUEST_DEADLINE` 600s for
reindex / sync / analytics-rebuild). A tripped deadline returns the new typed
`IpcErrorKind::Timeout` (retryable) instead of pinning the connection task
forever. Protocol bumped additively (new error kind; clients decode it as a
string with fallback, so no client break).

Update (2026-06-11): the `dispatch` god-function split was deferred here, then
done. `dispatch` is now a 33-line router: `handlers::categorize(&request)`
picks a category and delegates to one of 10 per-category modules under
`daemon/src/handlers/`. Arm bodies moved verbatim (no behaviour change); the
helpers + shared types/consts they reference are now `pub(crate)`. Verified by
the routing tests (`dispatch_routes_each_request_to_its_response_variant`), the
full daemon suite, clippy `-D warnings`, and smoke.sh. Original rationale for
deferring (below) no longer applies.

Decision (original): the `dispatch` god-function (~1750 lines, 70 arms) is NOT
split in this pass. The split was scheduled to unlock per-request timeouts,
per-request tests, and instrumentation — all three now exist without it (the
timeout wraps the whole handler; `handler::routing_tests` covers the whole
dispatch). The remaining work is a pure code-move, coupled to the shared
optimistic-mutation scaffolding, with no behavioral benefit and a large blast
radius.

## D021: spotuify-launcher crate extraction — deferred, then shipped (2026-06-10 → 06-11)

The audit flagged `spotuify-cli`'s dependency on `spotuify-daemon` (for
`ensure_daemon_running`) as a boundary violation (cli must not depend on daemon
internals). The clean fix is a leaf `spotuify-launcher` crate (protocol + tokio)
holding the client-side launcher logic.

Done (2026-06-11): `crates/spotuify-launcher` now holds `ensure_daemon_running`,
`start_daemon_background`, `restart`/`stop`/`daemon_status`, the socket-state
probes, and the build-id + compatibility checks. `run_daemon` stays in the
daemon; `start_daemon(foreground)` is a thin wrapper (foreground → `run_daemon`,
background → `launcher::start_daemon_background`). `server.rs` re-exports the
launcher fns so the binary/TUI/`state.rs` keep calling `server::…` unchanged
(build-id is now single-source). `spotuify-cli` dropped its `spotuify-daemon`
dependency entirely. Verified by the smoke test (daemon start/status/stop
through the new path), workspace compile, and clippy. The deferral rationale
(below) held until a smoke-gated pass made the move safe.

## D022: Mercury radio + related artists shipped (2026-06-10)

Reverses the Phase-8 deferral ("radio_start / related_artists deliberately
absent until typed daemon requests and verified mercury parsing exist").
Built end to end on the user's call to ship without a live spike:

- `spotuify-spotify/src/mercury.rs`: base62↔gid conversion + defensive
  parsers for the `hm://artist/v1/{gid}/desktop` and
  `hm://radio-apollo/v3/stations/{uri}` responses. A rotated/unknown shape
  degrades to empty results rather than erroring.
- `Request::RelatedArtists` / `RadioStart` (CoreMusic), daemon handlers via
  the in-session `mercury_get` with an 8s timeout, CLI (`artist related`,
  `radio start --dry-run`), MCP tools (`related_artists`, `radio_start`),
  and the macOS `DaemonRequest` cases (parity test forces them).

Caveat: the `hm://` endpoints are reverse-engineered and unversioned, and
the Web API equivalents were deprecated Nov 2024. This shipped WITHOUT a
live verification (mercury isn't curl-able; a spike needs a logged-in
session). The parsers are defensive and the daemon logs "endpoint may have
changed" when a response doesn't parse; if Spotify rotated the shape, the
fix is localized to `mercury.rs`. Verify against a live Premium session.

## D023: Windows SMTC + macOS CLI notarization both shipped (2026-06-10)

**Windows SMTC hidden window (9.C) — SHIPPED.** Originally deferred as
environment-blocked, then unblocked by standing up a cross-compile toolchain on
the macOS dev box: `xwin` for the MSVC CRT/SDK headers + `cargo-xwin` +
Homebrew LLVM (`clang-cl`/`lld-link`) so `ring`'s C build and the winit Windows
backend compile for `x86_64-pc-windows-msvc`. `crates/spotuify-system/src/media_controls_win.rs`
spawns a dedicated thread that creates a hidden message-only `winit` window,
hands its `HWND` to souvlaki, and runs the event loop forever to pump SMTC
button presses. The souvlaki controls are owned on that thread (SMTC must be
same-thread as its window); the main thread pushes owned metadata/playback over
an `EventLoopProxy`, and button presses flow back over the existing
`commands_tx`. `MediaControlsHandle` is cfg-split (Unix keeps in-process
souvlaki; Windows uses the thread). Verified: `cargo xwin check` + `cargo xwin
clippy -D warnings` for the Windows target are green, and the native build +
clippy stay green. Runtime caveat: there is no Windows CI runner, so live SMTC
button behaviour still needs manual QA on a real Windows box — but the failure
mode is bounded (init error → logged → daemon runs without SMTC, never bricks
playback). Remaining gap: `MediaControlsConfig.allow_hidden_window` (the
`--no-media-controls` opt-out) is honoured by the driver but not yet wired to a
CLI flag in `build_system_config`; that's a small follow-up, not a blocker.

Follow-up done (2026-06-11): `build_system_config` never set `system.media_controls`
at all, so the whole media-controls subsystem (MPRIS / Now Playing / SMTC) was
dead on every platform. It now defaults to enabled, with
`SPOTUIFY_NO_MEDIA_CONTROLS=1` disabling it entirely (sets both `enabled` and
`allow_hidden_window` to false, so the macOS Now Playing / Linux MPRIS
registration and the Windows hidden-window driver are all skipped). souvlaki
init failures still degrade gracefully (logged, no handle), so enabling it
can't break playback. Verified: daemon compile + clippy + 140 tests + smoke.sh
green with the subsystem on; the macOS Now Playing widget itself needs visual
confirmation (an OS-widget check, not a CLI surface). Also cleaned up: the
`#![allow(unused_imports)]` in the split handler modules was replaced with
precise per-module imports via `cargo fix`.

**macOS CLI signing/notarization in CI (9.N) — wired, guarded.** The release
DMG was already Developer-ID-signed + notarized locally via
`clients/macos/scripts/build-dmg.sh`; the gap was the macOS *CLI binary*
tarball, which shipped unsigned and so triggered Gatekeeper for curl/brew
users. `release.yml`'s `build-binaries` job now has a `Codesign + notarize CLI
binary (macOS)` step (between build and packaging) that imports a Developer ID
cert into a throwaway keychain, signs the binary with hardened runtime + a
secure timestamp, and notarizes it with `notarytool submit --wait` (a bare CLI
binary can't be stapled, so Gatekeeper verifies the ticket online). It is fully
guarded: with no signing secrets the binary ships unsigned exactly as before,
so forks and unconfigured runs still release. Required repo secrets to enable
it: `MACOS_SIGN_CERTIFICATE_BASE64`, `MACOS_SIGN_CERTIFICATE_PASSWORD`,
`MACOS_SIGN_IDENTITY`, `MACOS_NOTARY_KEY_BASE64`, `MACOS_NOTARY_KEY_ID`,
`MACOS_NOTARY_ISSUER_ID`. Verification limit: the YAML is syntax-validated but
the signing path itself can only be exercised in a real tagged release with the
secrets present — it follows the standard Apple/GitHub-Actions notarization
pattern and the no-secrets branch is a safe no-op.

## D024: Queue adds are not reversible; queue dedup is skip-only (2026-06-11)

**Queue adds no longer pretend to be undoable.** `OperationKind::QueueAdd`
moved to the non-reversible set. Neither the Spotify Web API nor librespot
0.8 exposes queue-remove, so the previous design (a `queue_remove` reversal
plan whose executor logged a warning, returned `Ok`, and marked the op
undone) reported success while removing nothing. New `queue_add` rows record
`reversible = 0` with a `NotReversible` plan stating the reason; store
migration v18 flips legacy rows so `ops undo` stops selecting them; executing
a legacy `queue_remove` plan now fails with a clear error instead of lying.
Revisit if librespot ever grows queue manipulation.

**Queue set semantics are enforced as skip, not move.** The product rule is
"a track appears at most once in the queue". Spotify has no queue-move, so
the implementable half is: at add time, fetch the LIVE queue (never the
persisted snapshot, which may describe a dead session; fetch failure degrades
to no dedup), drop URIs already queued plus intra-batch duplicates, and say
so in the receipt (`skipped N already queued`). The "move the existing entry
up" half of the rule is blocked upstream.

**`ops undo --dry-run` now previews.** `OperationUndoResult` gained a
wire-optional `preview` field carrying one "would undo …" line per inspected
op; the CLI prints those instead of the old bare `0 succeeded, 0 skipped,
0 error(s)` counts that read like a failure.

## D025: Pin a forked/patched librespot for session recovery (2026-06-15)

Chosen: depend on a forked librespot via `[patch.crates-io]` instead of the
crates.io `0.8.0` release, to get automatic session/dealer reconnect.

Considered:

- stay on librespot 0.8.0 and only recover daemon-side after drops
- reimplement librespot's session layer ourselves
- fork librespot, pin the upstream session-recovery fix, drop the fork later

Why:

- librespot 0.8.0 drops the AP session/dealer websocket every ~7–15 min and
  never self-recovers (`// TODO: Optionally reconnect`), so playback silently
  stops — the top user-reported reliability bug.
- The cure is upstream PR #1692, which is open and in no released version.
- Reimplementing librespot was rejected (rodio+CoreAudio SIGSEGVs on AirPods;
  portaudio is the deliberate macOS choice — see phase-9 embed doc).
- Daemon-side recovery (auto-reconnect, audio-flow watchdog, backoff) shipped
  in 0.1.68–0.1.71 but only *recovers* after the gap; it cannot prevent it.

What: `planetaryescape/librespot` branch `spotuify-session-recovery`, pinned by
rev. That branch is upstream `dev` (still version 0.8.0, no public-API removal
vs the tag) + PR #1692's commits. spotuify adapted to two additive API changes
(`SpotifyUri::to_uri()` is now infallible; new `PlayerEvent::SetQueue` variant
→ ignored). Constraints preserved: `librespot-playback default-features =
false`, `vergen = "=9.0.6"`.

This is explicitly temporary. **Drop the fork** when a librespot release > 0.8.0
ships the fix: delete the `[patch.crates-io]` block, bump versions, remove the
now-redundant daemon reconnect shims. Full rationale, rebuild steps, upstream
tracking list, and the removal checklist live in
`docs/maintenance/librespot-fork.md`. Re-evaluate at every dependency review
and before each release.

## D026: Spotify-only; no Apple Music support (2026-07-16)

Chosen: stay single-provider. Do not add Apple Music or any second music
service. Revisit only on the triggers listed in the feasibility study.

Considered:

- Apple Music as a second provider behind a catalog/player abstraction
- Apple Music as a metadata/playlist-only provider (no playback)
- stay Spotify-only

Why:

- **There is no librespot for Apple Music, and the gap is structural.**
  librespot is a clean-room reimplementation of Spotify's protocol. FairPlay
  Streaming has never been reimplemented — every working tool loads Apple's own
  compiled blobs (`libCoreFP.so` et al.) through the Android linker. That is
  undistributable, Linux-only, and DMCA §1201 circumvention. This alone takes out
  core principle #1 (player first, daemon owns playback) on Linux.
- The legitimate macOS paths are GUI-session-bound and daemon-hostile. MusicKit's
  `ApplicationMusicPlayer` needs the restricted `com.apple.application-identifier`
  entitlement (a CLI cannot hold one; needs an `.app` wrapper with an embedded
  provisioning profile) plus an interactive consent prompt. AppleScript Music.app
  has no queue API and library-only search — strictly worse than what we ship.
- AirPlay is an output transport, not a content source; it carries only audio you
  already hold in the clear. There is no Spotify Connect equivalent.
- Cost/auth: developer token needs the paid Apple Developer Program ($99/yr), and
  the music user token has no Linux/Rust minting path (Swift/JS/Android only).
- We have **no provider seam to slot into**. `PlayerBackend` is a real trait and is
  the easy 10%; the catalog layer is a 3,707-line concrete `SpotifyClient` called
  from 53 daemon sites, `ids.rs` hardcodes the `spotify` URI scheme, identity is a
  bare `spotify:`-prefixed `String` prefix-matched across 8 crates, and the store
  and Tantivy schemas carry literal `spotify_id` columns with no provider
  discriminator. `SPOTUIFY_FAKE_SPOTIFY` is a `fake: bool` + ~30 inline branches,
  not a second impl — it is evidence *against* a seam, not for one.
- Two providers sharing a URI-keyed store with no provider column would not
  collide, they would silently cross-contaminate: search and analytics would blend
  catalogs and double-count the same song under two URIs.

What: full findings, citations, file:line evidence, the unverified items, and the
re-validation triggers live in `docs/research/apple-music-feasibility.md`. Point
requesters there.

Note for future work: the provider-seam cleanup (real catalog trait, delete the
`if self.fake` branches, enforce `tests/workspace_boundaries.rs` instead of
waiving it) is worth doing **on its own merits** as architecture hygiene. It just
does not lead to Apple Music playback.

## D027: Mutation replay authority belongs to the daemon/store (2026-07-16)

Chosen: the durable daemon/store mutation claim keyed by `mutation_id` is the
replay and idempotency authority. An adapter receives `mutation_id` for
correlation and may suppress replays best-effort, but does not promise
exactly-once remote delivery; an ambiguous provider timeout makes that promise
impossible. Provider conformance therefore verifies receipt identity, outcomes,
and observable state changes without replaying writes. Daemon conformance owns
durable replay tests. The fake adapter retains replay/mismatch coverage for its
stronger in-memory behavior, not as a requirement on real adapters.

## D028: Defer playlist-item removal in the TUI (2026-07-17)

Chosen: expose provider-scoped playlist-item removal through IPC, CLI, and MCP,
but do not add an unrelated TUI action in the provider-abstraction work.

Why: the TUI has add-to-playlist and playlist-unfollow actions, but no existing
remove-selected-item action or playlist-ownership context to extend. A new key
would need an explicit design for which playlist occurrence is removed,
confirmation, and the distinction from library unsave. Revisit as a focused
contextual-action change; the CLI remains the complete, testable surface now.

## D029: Provider abstraction phases complete within D026 scope (2026-07-17)

Chosen: phases 0-9 and phase 10's D026-authorized dual-fake proof are complete.
Spotify is now an adapter behind provider-neutral core, URI, persistence,
search, sync, auth/config, protocol, daemon, player-policy, CLI, TUI, MCP, and
macOS client boundaries. The fake provider is an independent implementation
and executable conformance reference; two configured fake instances prove
registry routing, scoped search, and sync/store isolation.

Mutation replay is additive and compatibility-preserving. Current
`IpcClient` callers mint a UUIDv7 `mutation_id` for protected writes, and the
daemon/store durably replays the original terminal receipt/response for that
key. Legacy clients that omit the field remain accepted but are not
replay-suppressed. Per D027, no adapter promises remote exactly-once delivery
after an ambiguous provider timeout.

Deliberate scope boundaries remain:

- D026 keeps a real second adapter, cross-provider mappings, aggregation, and
  canonicalized analytics out of scope. They are product-gated, not unfinished
  provider-abstraction work.
- D028 keeps playlist-item removal out of the TUI until its interaction and
  ownership semantics are designed; IPC, CLI, and MCP expose it now.
- Scheduler integration tests use bounded real-time waits because freezing
  Tokio time also freezes SQLx acquire deadlines. Shutdown joins remain bounded.

Verification completed on the implementation tree:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- workspace tests across every crate (the two GA README posture assertions were
  excluded only because the working tree contains an unrelated user README;
  committed `HEAD:README.md` satisfies their required phrases)
- `cargo build --locked --release` on Linux and macOS
- `scripts/smoke.sh` against the release binary and fake provider
- macOS `Spotuify` test scheme: 87 tests passed; live-daemon suite skipped
- native dev-daemon drill: provider catalog/capabilities and unknown-provider
  error routing passed. Live Spotify search/playback was not exercised because
  the dev instance has no OAuth token; credentials and real playback were left
  untouched.

## D030: Remove exact playlist occurrences across clients (2026-08-13)

Chosen: revisit D028 with one occurrence-safe contract instead of a TUI-only
shortcut. IPC and MCP identify playlist items by URI plus exact zero-based
positions. The CLI exposes the same operation as one-based playlist rows through
`spotuify playlist remove-at`. URI-based `playlist remove` remains available and
keeps its existing semantics. This supersedes D028's deferral without rewriting
that historical decision.

The TUI's `Delete` action is contextual:

- playlist list: unfollow the selected playlist (not reversible);
- Liked Songs detail: unsave the marked or selected tracks;
- loaded playlist detail: remove the marked or selected exact occurrences.

Every TUI path opens confirmation first. Playlist-detail confirmation freezes
the occurrence payload so later selection changes cannot alter the write. The
client does not remove rows optimistically; the daemon remains the state owner.

The daemon validates preview and write against one authoritative full-playlist
read: positions must be non-empty, globally unique, in range, and paired with
the URI currently at each position. Provider mismatch, unsupported item kinds,
and local or unavailable placeholders fail before mutation. For providers with
playlist version tokens, successful writes record the removed rows and version
so undo can restore the same occurrences at their original positions.

## D031: Podcast playback speed + bookmarks (2026-08-22)

Chosen: both features are daemon-owned and exposed on every client (CLI, TUI,
MCP, macOS) in the same change.

**Playback speed.** librespot cannot change rate: 44.1 kHz is a compile-time
constant, `PlayerConfig`/`Spirc` expose no rate control, and the Connect
protocol's `playback_speed` field is only mirrored back as 0/1 for
pause/play. The stretch therefore runs in spotuify's own sink chain
(`LibrespotSinkChain::write`), the one place every decoded sample passes
through, using the header-only Signalsmith Stretch via the `ssstretch` crate
(MIT, `cxx` bridge — needs a C++14 compiler, no libclang). Rejected:
`soundtouch` (LGPL-2.1 is awkward for statically linked brew/cargo-install
binaries), `signalsmith-stretch` (runs bindgen at build time → libclang on
every build machine), `timestretch` (EDM-tuned, 0.x churn).

Consequences:

- Speed applies to **episodes only** (Spotify semantics); music always plays
  at 1.0. One global setting, range 0.5–3.5, persisted in the SQLite
  `daemon_settings` table (runtime UI state, not user-edited config TOML).
- Speed only takes effect on the embedded device. On a remote Connect
  device the setting is saved (`applied: false`) and used the next time the
  embedded player loads an episode.
- The embedded backend flips the sink rate on librespot `TrackChanged`
  (`UniqueFields::Episode` vs track). The daemon clock mirrors the rule
  (`effective_speed`) so extrapolated progress advances at the stretched
  rate between the player's position heartbeats.
- Wire: `playback-speed-set` / `playback-speed-get` → `playback-speed`
  response; `Playback.playback_speed` carries the effective rate.
  `PlaybackSpeed` is hundredths (`u16`) so protocol types stay `Eq`, and
  serialises as a plain number.

**Bookmarks.** Saved positions inside any media item with an optional note,
stored locally (`bookmarks`, v32), never sent to the provider. A bare
`bookmark-create` resolves item + position from the daemon's playback clock.
`bookmark-play` reuses the existing play path with `PlayContext.position_ms`
(already plumbed into both the embedded `seek_to` and the Web API body), so
there is no play-then-seek race. Bucket: `spotuify-platform`.

CLI: `spotuify speed [RATE|+|-]`, `spotuify bookmark {add,list,note,delete,play}`.
TUI: `[`/`]` speed, `B` bookmark now, screen `8`. MCP: `playback_speed_get`,
`playback_speed_set`, `bookmarks_list`, `bookmark_add`, `bookmark_play`,
`bookmark_delete`. macOS: speed menu (episodes only) + bookmark button in the
transport bar, Bookmarks destination.

## D032: Spectrum visualizer styles (cliamp port) (2026-08-24)

Chosen: 14 spectrum renderers behind one persisted setting, `viz.style`,
exposed on CLI, TUI, and MCP. Thirteen are ported from cliamp (MIT,
© Bjarne Øverli); `bars` stays spotuify's original widget, unchanged.

Considered and rejected:

- **Port cliamp's analyzer too.** cliamp runs its own FFT and some styles ask
  for 64 bands. spotuify's daemon already broadcasts 12 smoothed bands at
  30 Hz to every client, and a second analyzer would mean a second CPU cost
  and a second thing to keep in sync. The wide styles resample 12 → N with
  cliamp's own `resampleBandsLinear`, which costs detail, not correctness.
- **A style per config key** (`viz.flame_speed`, …). Every knob is another
  validation path and another thing to document. cliamp's tuning constants
  are good; they live as named `const`s next to their renderer instead.
- **Runtime-only style, like `viz.enabled`/`viz.source`.** Those two are a
  known persistence gap. A style the user picks and loses on restart is worse
  than no picker, so `SetVizStyle` writes the config file.
- **Reusing `ConfigReloaded` to announce the write.** Nothing was reloaded, and
  that event makes every TUI pop a "Config reloaded" toast (clobbering the
  acting surface's own) and run a full refresh. `SetVizStyle` instead emits
  `ClientPreferencesChanged { preferences }` carrying the whole fresh
  `ClientPreferences`, so a client applies it exactly where a seed would put
  it — no refetch, no refresh, no toast. The event is generic over
  `ClientPreferences`, not viz-specific, so later client-facing settings
  (themes) reuse it.

Consequences:

- `spotuify_protocol::VIZ_STYLES` is the single roster. Config normalisation,
  daemon validation, CLI listing, and the TUI picker all read it, so adding a
  renderer is one entry plus one file under `widgets/viz/`.
- Physics runs on a fixed 1/30 s timestep driven by the `SpectrumFrame` count,
  not wall-clock deltas. Golden-buffer tests are therefore exact, and a slow
  terminal catches up at most 4 frames per repaint instead of fast-forwarding.
  Each stateful style tracks the absolute frame it has stepped to, so drawing
  the same frame twice — the picker previews over the player panel — advances
  the animation once.
- cliamp's three colour tiers map onto the existing `spectrum_color` palettes
  rather than introducing new colours, so `viz.color_scheme` and the
  album-adaptive accent keep working for every style. Tier ratios are
  0.0 / 0.60 / 0.90 of panel height: 0.45 would collapse into the low tier
  under the existing `> 0.45` threshold.
- Style **preview** in the `ctrl+v` picker is deliberately client-local. It is
  modal view state (IPC bucket 4), so it never reaches the daemon; only Enter
  commits, and Esc restores what the picker opened with. Enter also applies
  locally so the user sees their pick immediately, so the commit watches the
  daemon's reply and puts the old style back if the write failed — otherwise a
  read-only config would leave the client disagreeing with everyone else.
- The daemon owns the style **in memory**; the config file is where it is
  persisted, not where it is read from. `diagnostics()` therefore never
  re-reads the file, and `SetVizStyle` does its write on the blocking pool:
  the config lock can wait seconds and `fsync`s both the file and its
  directory, which a tokio worker must not sit through.
- Every entry point canonicalises (trim + lowercase) *before* validating, via
  `canonical_viz_style`. Accepting `viz.style = " Classic-Peak "` on load while
  rejecting it from `config set` would be two contracts for one setting, and
  `config set` writes the canonical spelling so the file never depends on the
  loader repairing it.
- Each viewport that draws the spectrum — player panel, picker preview,
  fullscreen — owns its own `VizState`. The stateful styles key their buffers
  on panel size, so one shared state across two on-screen viewports reads as a
  resize every frame: the physics resets and `pulse` rebuilds its polar cache
  twice per frame instead of once ever.
- The fullscreen visualizer is a screen, not a modal: it paints *before* the
  overlay stack and yields Esc to anything opened on top of it.
- TUI keys: `v` unchanged (toggle), `V` is now the fullscreen visualizer, and
  `ctrl+v` opens the picker. Source cycling moved off `V` into that picker,
  which is also where `TuiAction::CycleVizSource` went.
- Two actions stay TUI-only. **Fullscreen Visualizer** (`V`) is a
  `client layout preference`: it resizes one client's panes and means nothing
  to a headless caller. **Visualizer Style** (`ctrl+v`) has the CLI equivalent
  `spotuify viz style <name>`; the overlay itself is a
  `client visualizer style picker`, i.e. modal state that never crosses IPC.
- Under `NO_COLOR` the ported styles keep their glyphs (Braille is UTF-8, not
  colour) and drop colour. The `#` ASCII fallback stays specific to `bars`.
- Because stepping is frame-driven, the motion styles freeze rather than settle
  when the daemon stops emitting frames (paused and decayed). Accepted for
  batch 1; a decay-to-rest tick would be the fix if it bothers anyone.

### Motion parity (F2, 2026-08-26)

Batches 1 and 2 kept cliamp's per-tick constants verbatim and stepped every
style once per 30 Hz `SpectrumFrame`. cliamp does not run one clock: it drives
each mode off its own timer, so those constants are only wall-clock-correct at
that mode's rate. The result was every particle and scroll style running 1.5x
fast and the oscilloscope family 0.5x, both consistently wrong rather than
subtly so. Rain fell 30 rows a second where cliamp falls 20.

Chosen: rescale the frame counter per class, not the constants. `viz/mod.rs`
converts spotuify's frame index into the tick index cliamp's clock would be on
at the same instant, through one function with a class per cadence:

- **anim** (`ANIM_HZ = 20`, cliamp `TickFast`) — every particle and scroll
  style, plus the `flame` / `terrain` / `mosaic` / `sand` / `geyser`
  simulations. `Ctx::anim_frame()`; advances on two frames in three.
- **wave** (`WAVE_HZ = 60`, cliamp `TickAnim` / `TickWave`) — the bar styles
  and the oscilloscope family. `Ctx::wave_frame()`; advances twice per frame.
- **fixed-timestep** — `classic-peak` and `classic-led` integrate rates in
  per-second units against `STEP_SECONDS`, so they were already wall-clock
  correct and read the raw frame.

Considered and rejected:

- **Scaling each constant** (`DECAY.powf(2/3)`, `probability * 2/3`, halved
  phase rates). Twenty-odd constants across sixteen files, each a place to get
  the exponent wrong, and each one then diverging from the cliamp line it was
  copied from. Rescaling the input leaves every constant readable next to its
  upstream source.
- **Running the daemon feed at 60 Hz and decimating per style.** Doubles FFT
  and broadcast work for every subscriber to fix a client-side arithmetic
  problem.
- **A second frame counter per class in `VizState`.** The counters would have
  to stay in lockstep anyway; deriving both from the one counter cannot drift.

Consequences:

- A style advancing on some frames and not others is the correct rendering of a
  20 Hz animation sampled at 30 Hz. `StepClock` already collapsed a repeated
  frame to zero steps, so the stateful simulations needed no new machinery —
  they take the rescaled frame and run 0 or 1 steps per repaint.
- `scope`'s ad-hoc doubled wobble rate (D035) is gone; it is back on cliamp's
  0.02 with the wave clock, which is the same number by a rule instead of by
  hand.
- The golden snapshots of all fifteen anim-class styles moved. `mirror`,
  `scope`, `classic-peak`, `classic-led`, and the reactive bar styles did not,
  which is the check that the classification is right.
- Parity is tested as a rate in real seconds, not as a ratio: `rain`,
  `terrain`, and `sakura` each have their cliamp-derived rows-or-columns per
  second measured off rendered buffers over a two-second run.
- New renderers must not read `ctx.frame`. The two accessors and
  `Ctx::seconds()` are the whole interface; the module header names which
  class a cliamp driver belongs to and how to tell.

## D033: 10-band equalizer (2026-08-24)

Chosen: a daemon-owned, persisted 10-band parametric EQ applied in the
embedded sink chain, exposed on CLI, TUI, MCP and macOS in one change.
Ported from cliamp (MIT, (c) Bjarne Overli): the preset table and the band
centre frequencies (70/180/320/600/1k/3k/6k/12k/14k/16k Hz, Q 1.4) are its
values, the DSP and the wiring are ours.

Placement mirrors D031's playback speed. librespot has no EQ, and the one
place every decoded sample passes through is `LibrespotSinkChain::write`, so
the filters live there — after the time-stretch and **before** the
visualizer tap, so the spectrum shows the signal that actually reaches the
speaker. Each band is one peaking-EQ biquad per channel (`biquad` 0.6,
`DirectForm2Transposed<f64>`; MIT/Apache-2.0, no_std, no build script).
Rejected: `fundsp` (a whole synthesis graph for ten biquads) and rolling our
own cookbook coefficients (the crate is 300 lines and already tested).

Consequences:

- The EQ applies to **music and episodes alike** — unlike speed, which is
  an episode-only Spotify semantic.
- A flat curve is a true bypass. `EqStage::process` returns without touching
  the buffer, so the cost for a listener who never opens the EQ is one
  relaxed atomic load per packet. Coefficients rebuild only when a
  generation counter moves, not per packet.
- **Nothing unbounded runs on the audio thread.** Every stage is a fixed
  cost per sample — ten biquads, plus D036's limiter — and the only work
  beyond that is ten `Coefficients::from_params`, which run when the
  generation counter moves, not per packet.
- **Clipping control: superseded by D036.** This decision shipped a static
  pre-gain of `-(cascade peak + 0.05)` dB ahead of the filters, plus a 10 ms
  ramp so changing it did not click. That made every boost preset
  permanently quieter (Bass Boost by 8.8 dB) to prevent clipping that only
  happens on the loudest transients. D036 replaced both with a peak limiter
  after the filters. Coefficients still switch instantly rather than
  crossfading.
- **Gains outside ±12 dB are rejected, not clamped.** `eq --band 0 100`
  errors rather than reporting success for a curve it did not apply;
  `EqBands::from_db` enforces it so the wire and MCP inherit the rule. The
  TUI's ±1 dB stepping still clamps, because there the rail is the intent.
- `eq-set` is serialised by a daemon-side lane and its player push is bounded
  at 5 s. Two concurrent sets cannot leave SQLite holding one curve while the
  sink plays another, and a wedged player actor degrades to `applied: false`
  instead of hanging the request.
- Gains are tenths of a dB (`i16`) internally so `Request`/`ResponseData`
  stay `Eq` and `Hash`, and plain dB numbers on the wire. `EqBands`
  deserialises only from exactly 10 finite values, so a short curve is a
  decode error rather than a silently zero-padded one.
- Setting a band clears the preset label; the curve then shows as `Custom`.
  Preset names are case-insensitive on input.
- Persisted as JSON in the SQLite `daemon_settings` table under key `eq`
  (runtime UI state, not user-edited config TOML). Unreadable JSON is warned
  about and ignored, leaving the default flat curve.
- Embedded device only. On a remote Connect device the curve is saved and
  the response carries `applied: false`, same semantics as speed.
- Wire: `eq-get` / `eq-set` -> `eq` response, plus an `eq-changed` event so
  every client renders the same curve without polling. `eq-set` takes
  exactly one of `preset` or `bands`; both or neither is a validation error.

CLI: `spotuify eq [PRESET|presets] [--band I DB]... [--reset]`.
TUI: `E` opens the editor overlay (h/l band, j/k +/-1 dB, p preset, r reset),
`Ctrl-e` cycles presets. (`e` is queue-selection and `V` is the visualizer
source, so neither letter was available.) The now-playing chip appears only
for a non-flat curve, and degrades `EQ <preset>` -> `EQ` -> nothing as the
transport narrows: three toggles already fill 22 of a compact transport's 26
columns, so an unconditional chip pushed `like` off the row.
MCP: `eq_get`, `eq_set`. macOS: preset menu in the transport bar.

## D034: user colour themes for the TUI (2026-08-25)

Chosen: terminal colour themes in cliamp's TOML format (MIT, (c) Bjarne
Overli), nine of its themes shipped embedded, user files in
`<config_dir>/themes/*.toml`, selected by a new `tui.theme` config key and
exposed on CLI, TUI, MCP and the macOS wire in one change.

Adopting cliamp's format rather than inventing one is the whole point: a
theme written for cliamp loads here unchanged, and the format is seven hex
strings, which is as small as a theme format gets. Six roles are required,
`bg` is optional (absent means "keep the terminal's background").

The daemon resolves the theme; clients never read a theme file.
`ClientPreferences` carries the resolved `ThemeSpec`, not its name, so the
seed and `ClientPreferencesChanged` are enough to paint. That keeps the
"daemon owns state" rule honest for a setting whose truth lives across two
places (the config file and a directory), and means a CLI `spotuify theme
winamp` repaints an open TUI with no extra round trip.

Consequences:

- The TUI's colour tokens changed from `const` items to accessor functions
  over a thread-local (`refactor(tui): token consts become accessors`, one
  behaviour-free commit before the feature). A `const` can never follow a
  runtime theme; there was no smaller change that worked.
- Only seven roles are named. The surfaces between background and text
  (panel fill, borders, chip background, unfilled seek bar) are derived by
  blending `bg` toward `fg` at the ratios the built-in palette already
  used, so a theme cannot produce unreadable chrome and a theme author
  cannot be asked about sixteen colours.
- `KIND_PODCAST` / `KIND_ALBUM` / `KIND_ARTIST` stay fixed in every theme.
  They are a legend: the hue identifies the category, so it has to mean the
  same thing under every palette.
- Album-adaptive accents still win over the theme when cover art is loaded.
  `UiPalette` gained an `adaptive` flag so the accessors can tell "derived
  from art" from "the compile-time default", which is what decides whether
  the theme or the cover supplies `accent`.
- The spectrum's three intensity tiers now read `danger()` / `warn()` /
  `success()` instead of the same three literals. That is exactly what
  cliamp's `red` / `yellow` / `green` roles are for. `rainbow` and
  `monochrome` are fixed palettes by design and ignore the theme.
- Built-ins ship in `spotuify-config`, not the daemon, because config
  validates `tui.theme` at write time and needs the same catalog the daemon
  resolves from. `ThemeSpec` itself lives in `spotuify-core` so
  `ClientPreferences` can carry it.
- cliamp's `accessibility_test.go` is ported: every shipped theme holds
  4.5:1 for all six roles on its own background. A theme that fails cannot
  be merged.
- An unreadable or incomplete user file is skipped with a daemon warning,
  never fatal. A `tui.theme` naming a file the user deleted falls back to
  the built-in palette rather than refusing to start; only *writes* of an
  unknown name are rejected, with the list in the error.
- `terminal-default`, `list`, and `path` are reserved names, refused with a
  warning. The sentinel is what "no theme" means, and the other two are
  `spotuify theme`'s own subcommands, so a theme called either could be
  listed but never applied.
- A file is never the sentinel. `is_terminal_default` requires all seven
  fields absent and `validate` has no sentinel escape, so a file holding
  only `yellow` and `red` (or nothing at all) is an error naming the first
  missing role rather than a theme that silently resolves to built-in
  colours.
- Only regular files under 64 KiB are read, checked on the opened handle
  and enforced with a bounded `take`, not a size precheck. The directory is
  walked unattended on the blocking pool, where a `.toml` symlinked to a
  FIFO blocks `open` forever and one aimed at `/dev/zero` reads until the
  process dies. The path is stat-ed *before* opening for exactly that
  reason: by the time a blocking `open` returns there is nothing to check.
- `set-theme` and `set-viz-style` share a `preferences_write_lock` held
  across validate -> write -> cache -> emit, the same shape D033 used for
  `eq-set`. Interleaved writes would otherwise leave the config file holding
  one value while the cache and the event clients just applied hold another,
  a disagreement that only surfaces on the next restart.
- `bg()` is `Color::Reset` for a theme with no background, which is right
  for a surface and wrong for ink: as a foreground, Reset is the terminal's
  own text colour, so a warning chip became light-on-yellow. `contrast_fg()`
  is the dark-ink role — the theme's `bg` when it has one, a near-black
  otherwise — and it never returns Reset.
- `UiPalette` stores only the cover's dominant colour; the accent, panel
  tint, rail, and soft selection fill blend against the *active theme* at
  read time. Storing the blends froze whichever theme was live when the
  artwork decoded, so switching theme with a cover loaded left the
  now-playing panel tinted for the old one until the track changed.
- Both clients show an applied theme that is no longer in the list: the CLI
  marks it `missing` with a note, the TUI picker lists it first as
  `(file removed)` and opens on it. Selecting row 0 instead would claim the
  user is on `terminal-default` while the terminal plainly is not. Enter on
  that row is a no-op close: the theme is already applied, and `SetTheme`
  for a name the daemon no longer has would answer "unknown theme".
- `theme list` carries the active theme in every machine format, not just
  the table: JSON/JSONL emit `{ active, active_missing, themes }` (JSONL on
  one line — the active theme is a property of the answer, not of a row),
  CSV gains `active` and `missing` columns and appends the orphan as a row,
  and `ids` stays names-only because it answers "what can I apply".
- `Reload` takes the preference lane too. It reads the config and then
  awaits through `apply_runtime_config`; a `SetTheme` landing in that gap
  would persist its theme and then have the stale load overwrite the cache
  and the broadcast, so clients kept the old colours until a restart.
- Theme files are opened with `O_NONBLOCK` on Unix. The stat precheck can
  go stale between the stat and the open, and a blocking `open` on a FIFO
  hangs before the handle check can reject it; non-blocking open returns
  immediately and the `is_file()` check does the rejecting.
- `spotuify reload` emits `ClientPreferencesChanged` alongside
  `ConfigReloaded`. No client re-seeds preferences on the latter, so a
  hand-edited `tui.theme` would otherwise never reach a running TUI. Only
  the reload path does this: `Reconnect` and `SetAudioOutput` also emit
  `ConfigReloaded` but never re-adopt the config, so broadcasting from
  there would hand clients the file's `viz.style` while the coordinator
  still held the old one.

CLI: `spotuify theme [<name>|list|path]`, following `spotuify eq [PRESET|
presets]` rather than adding a subcommand enum, so `spotuify theme winamp`
is one word. `list` and `path` are therefore unusable as theme names.
TUI: `t` opens the picker; arrows preview by repainting the whole interface
(the preview *is* the rest of the UI), Enter commits with revert-on-error,
Esc restores, `/` filters.
MCP: `themes_list`, `theme_set`. macOS: wire parity only, no UI.

## D035: Visualizer styles batch 2 — waveform on the wire (2026-08-25)

Chosen: 14 more cliamp renderers on top of D032's framework, taking
`VIZ_STYLES` to 28. Three of them (`wave`, `scope`, `heartbeat`) trace raw
samples, so `DaemonEvent::SpectrumFrame` gains an optional
`waveform: Vec<f32>` — 128 decimated mono samples in `-1.0..=1.0`, oldest
first.

Considered and rejected:

- **A separate `waveform-frame` event.** Two events at 30 Hz describing the
  same instant is two things to keep in phase and two subscriptions for every
  client. One event carrying an optional field keeps a frame a frame.
- **Gating the waveform on the configured style** — tried, then reverted in
  review. The appeal was obvious: 128 floats of JSON 30 times a second that 25
  of 28 styles never read, and `VizCoordinator` already caches the style
  (D032 round 2), so the gate looked free. It is not. *Configured* style and
  *drawn* style are different things — the `ctrl+v` picker previews a style
  locally while `viz.style` still names the old one, so gating left every
  preview of `wave`/`scope`/`heartbeat` tracing an empty buffer until Enter.
  The gate was also racy: `set_style` wrote the style and the gate flag under
  separate locks, so two concurrent `SetVizStyle`s could leave diagnostics
  reporting `wave` with the waveform off permanently. The daemon does not know
  what any subscriber is drawing and should not try to guess; a frame measures
  683 bytes on silence and stays inside a couple of KB on real audio, which
  over a Unix socket does not justify a vote mechanism, and sending it
  unconditionally means previews, the fullscreen visualizer, and any future
  client (the macOS app could draw it) work with no further plumbing. The
  ticker already stops emitting once audio decays, which is the bound that
  actually matters.
- **Averaging each decimation bucket instead of picking every Nth sample.**
  Averaging is a low-pass filter: a bucket spanning several cycles of a mid or
  high frequency sums to roughly zero, so an averaged trace collapses to a
  flat line on exactly the material a scope should show most motion for.
  Picking aliases instead, which is what a hardware oscilloscope does and what
  reads as a waveform. Documented on `AudioAnalyzer::latest_waveform`.
- **Porting `stereo`.** spotuify's tap is mono-mixed before it reaches the
  analyzer, so a left/right split would draw the same trace twice. Getting a
  real stereo tap means changing the sink chain and the analyzer's ring
  buffer, which is a bigger change than a renderer and belongs on its own.
  `scope` already covers the XY-scope idea from a mono source by phase-delaying
  the signal against itself.
- **Porting `logo`.** It draws cliamp's own wordmark.

Consequences:

- `waveform` is `#[serde(default, skip_serializing_if = "Vec::is_empty")]`.
  A new daemon populates it on every frame it emits, so the skip only fires
  the other way: an old daemon's frames carry no such key and decode with an
  empty vec. Every waveform renderer draws a resting trace from an empty slice
  — a flat line for `wave`/`heartbeat`, a centred beam for `scope` — rather
  than an empty panel, so a new client against an old daemon degrades instead
  of breaking.
- `AudioAnalyzer::latest_waveform` takes `&self`. Reading the ring must not
  perturb `process()`'s smoothing or the noise gate, or selecting a waveform
  style would change what every *other* client's bars look like.
- Four of the new styles are stateful (`terrain`, `mosaic`, `sand`, `geyser`)
  and join the existing `StepClock` + per-`VizViewport` `VizState` pattern.
  cliamp's `sakura`, `firework`, and `bubbles` are drivers upstream but are
  pure functions of the frame counter, so they are ported stateless — no
  buffers to rebuild on resize, and deterministic for free.
- cliamp animates `wave`/`scope`/`heartbeat` at 60 Hz and everything else at
  20 Hz; our feed is 30 Hz. `scope`'s wobble rate is doubled to hold its
  wall-clock period. The 20 Hz styles keep cliamp's constants verbatim, as
  D032's did — 1.5× faster motion, consistent across both batches, and each
  constant is named next to its renderer if it ever needs tuning.
  **Superseded** by D032's "Motion parity (F2)": the 1.5× is gone, the frame
  index is rescaled per class instead, and `scope`'s hand-doubled rate with it.
- `helpers.rs` gained `DotGrid` (monochrome, row-coloured) alongside the
  tiered `BrailleGrid`, plus `band_avg`. Seven of the new renderers build a
  dot field and colour it by row; that loop lives once.

No new CLI, MCP, or wire surface: the roster feeds all of them, so
`spotuify viz styles`, the `viz_style_set` enum, and the TUI picker pick the
new styles up on their own.

## D036: EQ peak limiter replaces the static headroom (2026-08-26)

Chosen: run the EQ at full level and catch overshoot with a peak limiter
after the filter bank. D033's static pre-gain is gone, along with
`eq_headroom_db` / `eq_peak_frequency_hz` / the cascade sweep in core and the
10 ms level ramp in `EqStage`.

The problem it fixes: D033 attenuated the whole signal by the cascade's peak
response so a full-scale sine could never leave the filters above 1.0.
Picking "Bass Boost" therefore made everything — including the treble the
preset does not touch — 8.8 dB quieter than flat, all the time. That trades a
rare artefact (clipping on the loudest bass transients) for a constant one (a
much quieter EQ). Users read the constant one as "the EQ is broken".

The limiter (`crates/spotuify-player/src/backends/limiter.rs`): per-frame
peak detector on the loudest channel, ceiling at -0.3 dBFS, gain
`threshold / peak` applied the same sample the threshold is exceeded, and an
exponential release that gives back ~90% of the reduction over 120 ms. One
gain for the whole frame, so a transient on one channel cannot shift the
stereo image. No allocation, no lock, no branch beyond the comparison.

Consequences:

- **Perceived level tracks the curve, not the worst case.** A -20 dBFS-RMS
  pink-ish probe through Bass Boost now comes out 4.2 dB *above* flat (it is
  a bass boost) and 6 kHz — a band the preset leaves at 0 dB — moves by less
  than 0.5 dB. Under D033 the same 6 kHz moved by -8.8.
- **Flat is still a true bypass, limiter included.** A flat curve returns
  from `EqStage::process` without touching the buffer, so a full-scale input
  comes out at full scale and the ceiling never applies. The limiter only
  exists to contain gain the EQ itself added.
- **No lookahead.** A hard ceiling with an instantaneous attack cannot let a
  sample through above it, which is the property that matters; smoothing the
  attack would need a delay buffer, add latency to a chain already fighting
  for its packet deadline, and only round a step that lands on an
  already-over-threshold sample. Release is smoothed because that step *is*
  audible — it is what pumping sounds like.
- **Release is defined as 90% recovery, not a 1/e time constant.** 120 ms
  reads as the time a listener would say the level came back. Much faster
  and the gain modulates inside one cycle of the bass it mostly catches
  (70 Hz is a 14 ms period); much slower and one transient ducks the next
  bar.
- **The meter is on the wire.** `ResponseData::Eq` gained `limiting_db`, an
  `EqLimiting` newtype holding tenths of a dB (unsigned) so `Request` and
  `ResponseData` keep their `Eq`/`Hash` derives — the same reason `EqBands`
  and `PlaybackSpeed` are integers. It serialises as the signed number a
  meter shows: `-2.4` while limiting, `0.0` when idle. `spotuify eq` prints
  `limiter: -2.4 dB` / `limiter: idle`, carries it in JSON, and repeats it
  as a CSV column the way `preset` and `applied` already are; the TUI editor
  shows the same line and `eq_get` carries it to MCP. Both the wire field
  and the macOS model default to idle, so a client upgraded ahead of the
  daemon it is already talking to — the window every `brew upgrade` opens —
  decodes the response instead of failing it.
- **The meter reads the packet's last frame, not its deepest.** The release
  is slow relative to a ~46 ms packet, so a transient anywhere inside it is
  still visible at the end; taking the packet minimum instead would hold a
  spike for a full packet after the limiter had let go of it. The daemon
  reports `idle` whenever the curve is not `applied`, so a reading left over
  from before the listener moved to a Connect device cannot go stale on
  screen.
- **A reading is only ever current.** The meter is written per packet, so
  anything that ends the stream of packets has to clear it or the last loud
  packet's reduction sits there being described as "current gain reduction".
  Five places do: `EqStage::new` and `Drop` (librespot rebuilds a sink after
  a panic and reconnects by dropping one, neither of which calls `stop`), the
  sink's `start`/`stop` (pause, seek, track change), `SharedEq::set_bands`
  (a reading belongs to the curve that produced it), and the daemon, which
  reports `idle` whenever the curve is not `applied`.
- **The meter is generation-tagged, in one word with the reading.**
  `process` loads the generation once at the top, so a packet can finish
  after the curve moved; an untagged store would let it overwrite the idle
  `set_bands` had just published, and only the *next* packet would repair
  that — if playback stopped there, `eq-get` stayed stale. The meter is a
  single `AtomicU64` — generation in the high 48 bits, tenths of a dB in the
  low 16 — written by a compare-exchange that drops readings from a
  superseded curve. `set_bands` publishes idle under the *new* generation
  before making that generation visible, so a packet on the old curve loses
  the compare and no packet can yet be running on the new one.
- **`EqStage` owns its `SharedEq` rather than taking it per call.** It has to
  reach the meter from `new` and from `Drop`, and a stage that could be
  handed a different `SharedEq` than the one its readings are tagged against
  would be a bug with no way to catch it. The publish-dedup cache is per
  stage while the meter is shared, so the cache starts empty and is
  invalidated on a generation change — otherwise it would claim a value the
  meter no longer holds and suppress the store that puts it back.
- **The daemon reads it without a round trip.** `PlayerBackend::eq_limiter`
  hands out an `EqLimiterMeter` — a clone of the same `Arc` the sink writes
  through — at install, alongside `audio_counter`. `eq-get` then costs one
  relaxed atomic load. A `TransportCmd` query would have put an actor round
  trip and a timeout on a diagnostic number.
- **Going flat no longer needs a bleed-out.** D033 kept processing through
  the ramp so the filters would not be cut mid-tail. With no level to ramp,
  bypass resumes on the next packet and passthrough is byte-identical again
  immediately. The coefficients were already unity by then, so the "tail"
  that ramp protected was one sample of stored state.

Considered and rejected:

- **Keep the pre-gain, make it smaller.** Any fixed attenuation is still
  paid on every sample of every track; halving it halves the loss and halves
  the protection. The distribution is the point — overshoot is rare, so pay
  for it when it happens.
- **Partial compensation (pre-gain + soft clip).** Two mechanisms to tune
  instead of one, and the soft clipper still distorts on exactly the
  material the limiter handles cleanly.
- **A user-facing preamp control.** Puts the arithmetic on the listener and
  needs a new persisted setting, a CLI flag, and a TUI control — to solve a
  problem the DSP can solve without being asked.

Verification is unit-level: the fake provider emits no audio, so the DSP is
proven by tests in `backends/eq.rs` and `backends/limiter.rs` (full-scale
70 Hz through Bass Boost never exceeds the ceiling and shows > 8 dB of
reduction; a 0.3-amplitude probe through the same curve shows none; release
is back within 0.2 dB of unity 200 ms after the transient; both channels of
a frame move by the same gain).

## D037: Event-stream forward compatibility is the client's job (2026-08-26)

Chosen: clients tolerate events they can't decode, and the roster is a two-way
contract. `DaemonEvent` gets an `Unknown { event, raw }` variant produced by a
hand-written `Deserialize` that falls back when the derived codec refuses a
frame, so an unknown tag — or a known tag whose payload this build can't
satisfy — costs one dropped event instead of the connection. `Unknown` keeps
the frame verbatim and re-serialises it unchanged, so a relay
(`spotuify events`) forwards what the daemon actually sent. Parity is enforced
by `DaemonEvent::all_kind_labels()` ↔
`clients/macos/Tests/SpotuifyKitTests/Fixtures/event-kinds.json` ↔ the Swift
decoder, the same mechanism `Request` has used since the macOS client landed.

Considered and rejected:

- **Bump `IPC_PROTOCOL_VERSION` for every new event.** Clients gate their UI on
  `protocol_version >= IPC_PROTOCOL_VERSION`, so this turns "the daemon can now
  tell you about EQ changes" into "your TUI refuses to start". Additive events
  are additive; the version is for shape changes clients must refuse.
- **Capability gating — clients declare the kinds they understand at subscribe
  time and the daemon filters.** More moving parts on the hot path, a new
  handshake to version, and it still leaves the client fragile the moment a
  daemon forgets to filter. Tolerance at the decoder is one place, always on,
  and testable without a daemon.
- **Leaving the released `#[serde(other)] Unknown` unit variant alone.** It
  already stopped the stream dying, but it threw the tag away: the TUI couldn't
  log what it dropped, `spotuify events --kind` had nothing to filter on, and a
  relay re-emitted `{"event":"unknown"}` in place of the real frame. Carrying
  the tag needs a struct variant, which `#[serde(other)]` does not support,
  hence the manual impls (`#[serde(remote = "Self")]` keeps the derived codec
  available as inherent functions, so only the fallback is hand-written).

Consequences:

- **New event fields must be `#[serde(default)]`.** Tolerance covers the
  unknown-tag case for free, but a missing required field on a *known* tag
  degrades that event to `Unknown` — the stream lives, the update is lost.
  `tests/event_tolerance.rs` pins both directions, and the rule is in the
  `spotuify-protocol` module docs.
- **`Unknown` carries an `UnknownReason`, and the two reasons get opposite
  reactions.** Treating them alike is how a resilience feature becomes a
  silent-failure feature: an undecodable `event-stream-lagged` would drop the
  one event whose entire job is to trigger a re-seed, and an undecodable
  `playback-changed` would leave MCP clients serving a stale resource forever.
  So `UndecodableKnownTag` makes the TUI warn (rate-limited to once per kind per
  minute, since a broken 30 Hz kind would otherwise bury the log) and refresh,
  and makes MCP invalidate by *label* rather than by variant. `UnknownTag` stays
  debug-and-ignore, because there is genuinely nothing to react to.
- **The roster is generated, not maintained.** `daemon_event_kinds!` expands one
  table of `(pattern => label)` rows into both `kind_label()` and
  `all_kind_labels()`, so a new variant fails to compile until it is listed, and
  the two can't disagree. The macOS fixture is generated from the same table
  plus a sample frame per kind, and Swift's parity test decodes those samples —
  proving the decoder handles each kind rather than comparing name lists.
- Swift gained the nine cases it never modelled (mutation receipts, the
  operation log, analytics import progress, viz source changes). `.unknown` is
  now strictly the newer-daemon fallback, and `ProtocolParityTests` proves each
  listed tag really decodes rather than trusting a hand-kept set.
- `spotuify events` makes the push channel a first-class CLI surface: JSONL by
  default with a `_received_at_ms` envelope field, `--kind`/`--once`/`--timeout`
  for scripts, a bounded 5-try reconnect, and a clean exit under `| head -1`.
  `--timeout` bounds every waiting step, including the reconnect backoff and the
  connect attempt — an outage must not outlive the patience the caller asked
  for.
  Workers had been hand-rolling raw socket clients to watch the daemon, which
  is exactly the CLI-everywhere gap the contract exists to prevent.
- Decoding routes through `serde_json::Value` once per event. At the visualizer's
  30 Hz that is the cheapest thing on the path — the frame was already parsed
  and allocated by the codec.
