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
