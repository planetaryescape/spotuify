# Phase 8 - MCP Server

## Goal

Expose the daemon's Request/Event surface as a Model Context Protocol (MCP) server so LLM clients (Claude Code, Cursor, Continue, agent harnesses) can use spotuify as a first-class tool without shelling out to the CLI. Cover lyrics/radio/recommendations replacements via mercury bus (Phase 9). Adopt destructive-action confirmation patterns.

## Strategic rationale

- Existing Spotify MCP servers (varunneal, tylerpina, Carrieukie, iankan04) are all Python and Web-API-only. None have local cache, librespot playback, mercury bus access, or analytics.
- **No prominent Rust-native Spotify MCP exists.** Single largest 2026 differentiator the blueprint does not yet name.
- spotuify's daemon already speaks length-delimited JSON over local IPC with typed Request/Response/Event types. Exposing those as MCP tools is incremental, not a rewrite.
- Embedding librespot (Phase 9) unlocks endpoints the Web-API-only MCP servers can't offer: lyrics, radio, related-artists, recommendations — all post-Nov-2024 alive on mercury.

## Reference patterns

| Pattern | Source | Lesson |
|---|---|---|
| Confirmation popups on destructive actions | spotify-player commit #966 | Every destructive MCP tool should require explicit `confirm: true` argument |
| Mercury bus for lyrics | spotify-player `client/mod.rs:642-661` | `hm://lyrics/v1/track/{id}` |
| Mercury bus for radio | spotify-player `client/mod.rs:949-1019` | `hm://autoplay-enabled/query`, `hm://radio-apollo/v3/stations/` |
| `login5().auth_token()` | spotify-player `token.rs:8-46` | Useful for first-party/keymaster mode and future native-session reads. D016 keeps dev-app PKCE as the default Web API path until sustained keymaster polling is no longer required. |

## Deliverables

- New crate `crates/spotuify-mcp` producing a `spotuify-mcp` binary.
- `spotuify mcp [--stdio | --http <addr>]` subcommand wired into `main.rs`.
- MCP tool definitions for every safe daemon Request, with JSON Schema.
- MCP resource definitions for playback state, devices, playlists, lyrics, and doctor. Stdio/HTTP resource reads map to daemon requests; invalidation tags are defined for future push subscriptions.
- Auto-spawn daemon if not running.
- Destructive operations gated by `confirm: true` in MCP tool args.
- README docs: Claude Code / Cursor / Continue config snippets.
- Decision-log entry D011.

## Tools to expose

| MCP tool | Backing | Notes |
|---|---|---|
| `search` | `Request::Search` | Default `source: hybrid`. JSON Schema includes `query`, `kind`, `limit`. |
| `now_playing` | `Request::PlaybackGet` | Track + device + progress + lyrics line if available |
| `play` | `Request::PlaybackCommand::PlayUri` | Exact Spotify URI chosen from `search` results |
| `play_uri` | `Request::PlaybackCommand::PlayUri` | Direct URI |
| `pause` / `resume` / `next` / `previous` | Transport | |
| `seek` / `volume` | Transport | |
| `shuffle` / `repeat` | Transport | |
| `queue_add` | `Request::QueueAdd` | URIs or query |
| `queue_show` | `Request::QueueGet` | |
| `devices_list` | `Request::DevicesList` | |
| `transfer_device` | `Request::DeviceTransfer` | Idempotent |
| `playlists_list` | `Request::PlaylistsList` | |
| `playlist_tracks` | `Request::PlaylistTracks` | |
| `playlist_plan` | local `spotuify-protocol::agent_playlists::build_playlist_plan` | Read-only deterministic scaffold; no daemon needed |
| `playlist_resolve_tracks` | daemon `Request::Search` workflow | Resolves plan candidate searches into track candidates via daemon search |
| `playlist_create` | `Request::PlaylistCreate` | **Requires `confirm: true`** to commit; without it returns dry-run preview only |
| `playlist_add` | `Request::PlaylistAddItems` | **Requires `confirm: true`** |
| `playlist_remove` | `Request::PlaylistRemoveItems` | **Requires `confirm: true`**; typed protocol request, daemon handler, and MCP bridge route are wired. |
| `library_save` / `library_unsave` | `Request::LibrarySave` / `Request::LibraryUnsave` | **Requires `confirm: true`**; `library_unsave` has a dedicated protocol request and daemon handler, verified by MCP bridge routing tests. |
| `lyrics` | Phase 16 lyrics provider | Returns synced lines + provider + offset |
| `radio_start` | Deferred Mercury station workflow | Not exposed in the live MCP manifest until the daemon has a typed station request and verified mercury response parsing. |
| `related_artists` | Deferred Mercury related-artists workflow | Not exposed in the live MCP manifest until the daemon has a typed related-artists request and verified mercury response parsing. |
| `analytics_top` | Phase 10 derivations | Tracks/artists/albums by window |
| `analytics_habits` | Phase 10 | Day/week/month rollups |
| `ops_log` | Phase 12 | Recent mutations |
| `undo_last` | Phase 12 `Request::OpsUndo` | Reverts last mutation (no confirm needed — undo is the safety net) |

## Resources to expose

- `spotuify://playback` — subscribable; refreshes on `DaemonEvent::PlaybackChanged`.
- `spotuify://devices` — refreshes on `DevicesChanged`.
- `spotuify://playlists` — refreshes on `PlaylistsChanged`.
- `spotuify://now_playing/lyrics` — live lyrics stream tied to current track and position.

## Confirmation pattern

Every destructive tool MUST take a `confirm: bool` argument:
- `false` (default) → returns a preview object (`MutationPreview` from Phase 12) and does NOT execute.
- `true` → executes, returns receipt.

This matches spotify-player commit #966 ("Add confirmation popups on destructive actions") for the TUI; we apply the same discipline at the MCP layer. An LLM that wants to confirm asks the user; the MCP server doesn't second-guess.

## Authentication & transport

- **stdio mode (default)**: trusts the process owner. Best for editor integrations.
- **HTTP mode**: `spotuify mcp --http 127.0.0.1:PORT` with `SPOTUIFY_MCP_TOKEN` bearer-token auth. For remote agents and harnesses.
- TLS not handled internally; expose via local-only address or a reverse proxy.
- Rate-limit MCP tool calls per (session, tool) to prevent agent loops from exhausting Spotify quota.

## Architecture

```text
crates/spotuify-mcp/
├── src/
│   ├── lib.rs
│   ├── server.rs           // JSON-RPC 2.0 over stdio/HTTP
│   ├── tools.rs            // tool catalogue + schemas
│   ├── resources.rs        // subscribable resources
│   ├── confirm.rs          // destructive-action gating
│   └── bridge.rs           // map MCP request → spotuify Request → MCP response
└── tests/
    └── mcp_handshake.rs    // golden manifest test
```

The MCP server is a thin bridge:
1. Receive MCP tool call.
2. Validate (`confirm` for destructive ops).
3. Translate to `spotuify-protocol::Request`.
4. Send to daemon over UDS.
5. Wait for `Response`.
6. Translate to MCP result.
7. Return.

Subscribed resources fan out `DaemonEvent`s as MCP `resource.updated` notifications.

## Agent playlist workflow clarification

`agent_playlists::build_playlist_plan` (shared from `spotuify-protocol`) is intentionally a deterministic scaffold heuristic, not an LLM call. The actual planning happens in the upstream agent (Claude, GPT, local model). MCP makes this explicit:

1. LLM proposes plan JSON matching `PlaylistPlan` schema.
2. LLM calls `playlist_resolve_tracks` MCP tool against the plan.
3. LLM calls `playlist_create` with `confirm: false` to preview.
4. LLM relays preview to user.
5. User approves.
6. LLM calls `playlist_create` with `confirm: true`.
7. Receipt comes back; LLM can call `undo_last` if user rejects after the fact.

Document this loop in README and in `docs/blueprint/09-agent-workflows.md`.

## Work items

1. [x] Add the MCP transport. The shipped implementation hand-rolls the small MCP/JSON-RPC surface over stdio and HTTP instead of taking an SDK dependency.
2. [x] Define MCP tool catalogue and schemas in `crates/spotuify-mcp/src/tools.rs` and `src/rpc.rs`.
3. [x] Bridge MCP tool call -> daemon Request -> MCP tool result. Verified for `playlist_remove`, `library_unsave`, `playlist_plan`, `playlist_resolve_tracks`, `ops_log`, `undo_last`, and structured destructive previews by MCP routing/RPC regressions.
4. [x] Bridge `resources/read` -> daemon Request. Verified by `resource_uri_maps_to_daemon_request`; event invalidation tags are implemented, while live push subscription fanout remains a transport follow-up.
5. [x] Add `spotuify mcp` subcommand. Default stdio mode is covered by CLI parser/help tests and MCP initialize tests.
6. [x] Add `--http <addr>` mode with bearer-token auth. Covered by bearer-token, loopback-bind, and unsupported-SSE tests in `crates/spotuify-mcp/src/http.rs`.
7. [x] Add confirmation gating on every destructive tool with explicit LLM-facing errors.
8. [x] MCP capability negotiation for tools and resources. Prompts are not advertised because no prompt catalogue is shipped.
9. [x] Mercury/provider-backed tools: `lyrics` is wired through the daemon lyrics request. `radio_start` and `related_artists` are deliberately absent from the live manifest until typed daemon requests and verified mercury parsers exist; advertising deferred tools to agents was removed by `future_mercury_tools_are_not_advertised_as_callable` and the manifest snapshot.
10. [x] Analytics tools: `analytics_top`, `analytics_habits`, `analytics_search`, and `analytics_rediscovery`.
11. [x] Undo tool: `undo_last`, `ops_log`.
12. [x] README snippets for Claude Code, Cursor, and Continue. The shipped command is the unified binary form: `spotuify mcp`.
13. [x] MCP manifest golden test.

## Verification

- `claude mcp add spotuify` succeeds; tools appear in `claude mcp list`.
- LLM can run `search` → `play` → `now_playing` end-to-end in a fresh Claude Code session.
- MCP manifest validates against current MCP spec.
- `playlist_create` with `confirm: false` returns a structured preview without mutating; `confirm: true` produces the same receipt as `spotuify playlist create --yes`.
- `playlist_add` / `playlist_remove` without `confirm` returns a clear "confirmation required" error; LLM must explicitly re-call with `confirm: true`.
- Future `radio_start` must return a station of URIs sourced from mercury (`autoplay-enabled` + `radio-apollo`) and must not call the dead Web API `/recommendations` endpoint.
- `lyrics` returns synced lines for a track that has them, plain text for tracks that don't, "not available" for missing ones.
- Killing the daemon while an MCP session is active surfaces a clear error; the next tool/resource call retries the daemon socket.
- `undo_last` reverts the last destructive op visible via `ops_log`.
- IPC requests now carry operation source attribution, so MCP-originated
  mutations are recorded as `source = mcp` and can be filtered by
  `ops_log`.

## Definition of done

The shipped Phase 8 slice exposes implemented daemon capabilities over
MCP stdio/HTTP, gates destructive tools with `confirm: true`, records
MCP mutations with `source = mcp`, and keeps deferred Mercury
radio/related-artist surfaces out of the live manifest. A live Claude
Code focus-playlist smoke remains manual because it requires an
interactive MCP client and Spotify account.
