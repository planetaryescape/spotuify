# spotuify - System Architecture

## High-level architecture

```text
┌──────────────┐     JSON over Unix socket     ┌──────────────────────────────┐
│ TUI          │◄─────────────────────────────►│ Daemon                       │
│ ratatui      │                               │                              │
└──────────────┘                               │ ┌─────────┐ ┌──────────────┐ │
                                               │ │ Sync    │ │ Player       │ │
┌──────────────┐     JSON over Unix socket     │ │ Engine  │ │ Controller   │ │
│ CLI          │◄─────────────────────────────►│ └────┬────┘ └──────┬───────┘ │
│ commands     │                               │      │             │         │
└──────────────┘                               │ ┌────┴────┐ ┌──────┴───────┐ │
                                               │ │ SQLite  │ │ Spotify Web  │ │
┌──────────────┐     JSON over Unix socket     │ │ Store   │ │ API Client   │ │
│ Agents       │◄─────────────────────────────►│ └────┬────┘ └──────┬───────┘ │
│ scripts      │                               │      │             │         │
└──────────────┘                               │ ┌────┴────┐ ┌──────┴───────┐ │
                                               │ │ Tantivy │ │ spotifyd /   │ │
                                               │ │ Search  │ │ Connect dev  │ │
                                               │ └─────────┘ └──────────────┘ │
                                               └──────────────────────────────┘
```

## Why daemon-backed

We considered keeping the current monolithic shape where the TUI directly calls Spotify. We reject that as the long-term architecture because:

1. Music should continue after the TUI exits.
2. CLI commands should be fast one-shot controllers.
3. Search and playlists should be locally cached and indexed.
4. Agents need a stable command/protocol surface.
5. Network and keychain failures should be centralized and observable.
6. Multiple clients should not duplicate Spotify logic.

## Daemon responsibilities

- Own OAuth token access and refresh.
- Own Spotify Web API client and rate-limit handling.
- Own spotifyd lifecycle and preferred device activation.
- Maintain local SQLite cache.
- Maintain Tantivy index derived from SQLite.
- Serve JSON IPC requests from CLI and TUI.
- Broadcast events to subscribed clients.
- Run background sync jobs.
- Record action traces and recent API failures.
- Reconcile optimistic mutations with Spotify truth.

## Client responsibilities

### CLI

- Parse user commands.
- Start daemon if appropriate.
- Send typed requests.
- Render human or machine output.
- Respect exit codes.
- Avoid direct Spotify calls except bootstrap/doctor fallbacks.

### TUI

- Render daemon-backed state.
- Dispatch actions.
- Show contextual hints, command palette, status, diagnostics.
- Handle optimistic local UI effects only when daemon returns a mutation receipt.
- Never own durable Spotify/library/search truth.

### Agents and scripts

- Use the same CLI and output contracts as humans.
- Prefer `--format json` or `--format jsonl`.
- Use `--dry-run` before broad mutations.
- Use IDs over display names where possible.

## IPC protocol

Use length-delimited JSON over a Unix domain socket.

Target socket paths:

- macOS: `~/Library/Application Support/spotuify/spotuify.sock`
- Linux: `$XDG_RUNTIME_DIR/spotuify/spotuify.sock`

Core message envelope:

```json
{
  "id": "request-id",
  "payload": { "type": "request", "request": "search", "body": {} }
}
```

Protocol buckets:

| Bucket | Examples |
|---|---|
| `core-music` | playback, devices, queue, library, playlists, search |
| `spotuify-platform` | cached search, saved recipes, agent playlist plans, local profiles |
| `admin-maintenance` | daemon status, sync, logs, doctor, reindex, bug report |
| `client-specific` | TUI layout state, selected row, modal state |

Client-specific data must not leak into daemon protocol unless it becomes reusable domain state.

## Target Rust workspace

The current codebase is a single package. The target architecture can be reached incrementally, but the end state should use real crate boundaries:

```text
spotuify/
├── crates/
│   ├── core/        # domain types, IDs, errors, capabilities
│   ├── protocol/    # IPC Request/Response/Event types
│   ├── store/       # SQLite migrations and queries
│   ├── search/      # Tantivy indexing/query engine
│   ├── spotify/     # Spotify Web API provider
│   ├── player/      # device activation, spotifyd, playback orchestration
│   ├── sync/        # background sync and reconciliation
│   ├── daemon/      # socket server and runtime
│   ├── cli/         # clap commands and output renderers
│   └── tui/         # ratatui frontend
└── src/main.rs      # thin product binary entrypoint
```

## Copy-from-mxr architecture rule

The daemon/client architecture should be copied from mxr as much as possible:

- daemon lifecycle and restart/status commands
- length-delimited JSON IPC over Unix sockets
- protocol `Request`/`Response`/`Event` split
- CLI client wrapper
- TUI client wrapper
- async result reconciliation in TUI
- output format conventions
- mutation preview/receipt helpers
- SQLite source-of-truth plus rebuildable Tantivy index

Domain structs will differ, but the plumbing should not be rewritten from scratch.

## Dependency rules

1. `core` depends on no internal crate.
2. `protocol` depends only on `core`.
3. `store` and `search` depend on `core` only.
4. `spotify` depends on `core` and auth/config helpers, not TUI or CLI.
5. `player` depends on `core` and `spotify`.
6. `sync` depends on `core`, `store`, `search`, `spotify`, and `player`.
7. `daemon` is the integration point.
8. `cli` and `tui` are clients of `protocol`; they should not directly depend on `store`, `search`, or provider internals.
