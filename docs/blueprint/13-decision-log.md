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
