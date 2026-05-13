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
- spotifyd/librespot or official apps are the playback devices
- spotuify should control, not impersonate a streaming client unless we deliberately embed librespot later

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

Why:

- these actions do not mutate reusable app state
- daemon IPC should not expose screen cursor, modal, hint, or layout state
- CLI parity remains mandatory for reusable music capabilities

## D010: Embedded librespot (Phase 9, decision gate)

Chosen: embed librespot in the daemon behind a `--features embedded-playback` cargo feature; keep `--backend spotifyd` supported for users who want crash isolation.

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

Implementation lands in Phase 9; not part of the current Phase 6/7/8 batch.

## D011: MCP server as a first-class spotuify surface (Phase 8)

Chosen: ship `spotuify-mcp` as a workspace crate and a separate binary, exposing the daemon's Request set as Model Context Protocol tools and resources over stdio (default) or HTTP.

Why:

- No prominent Rust-native Spotify MCP exists in 2026; the Python servers (varunneal, tylerpina, Carrieukie) are Web-API-only with no local cache, no librespot playback, no analytics
- The daemon already speaks length-delimited JSON over Unix socket with typed Request/Response/Event; exposing the same types as MCP tools is incremental
- LLM clients (Claude Code, Cursor, Continue) can consume spotuify as a tool without shelling out
- Mercury-bus tools (lyrics/radio/related-artists, Phase 9 gated) and analytics tools (Phase 10 gated) give MCP clients capabilities the Python servers can't match

Discipline:

- Destructive tools (`playlist_create`, `playlist_add`, `library_save`, etc.) require explicit `confirm: true` in args. Without it the bridge returns a preview. Mirrors spotify-player commit #966 at the MCP layer.
- `undo_last` bypasses confirm -- it IS the safety net.
- Tools deferred to later phases surface a clear `LocalDeferred` marker rather than silently failing.

Pure-function core (tool catalogue, confirm gating, request bridge) tested with 31 unit tests; insta golden manifest snapshot locks the public tool surface so additions/renames are always a code-review event. The rmcp wire integration (stdio + HTTP transport) lands as a follow-up on top of the same core.

## D012: Operation log + undo (Phase 12)

Chosen: every daemon mutation records an `operations` row with a reversal plan, surfaced via `spotuify ops log` / `spotuify ops undo` and the MCP `undo_last` tool.

Why:

- Phase 8 lets LLMs mutate state; without undo, a misfired tool call is unrecoverable without manual SQL or Spotify-app intervention
- jj's `op log` + `op undo` pattern is the established 2026 shape for "I let an agent do things and want a back button"
- Phase 6's two-stage receipts already capture mutation intent; the operations table extends it with persistent reversal plans plus snapshot_id concurrency tokens for safe rollback

Implementation lands in Phase 12; not part of the current Phase 6/7/8 batch.
