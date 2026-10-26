# Architecture

spotuify is daemon-backed music infrastructure. The daemon is the system. TUI, CLI, MCP, scripts, agents, and the macOS app are clients.

For the full design record, read [docs/blueprint/README.md](docs/blueprint/README.md). This file is the short version.

## Current state

The codebase has 18 workspace packages: the root binary plus 17 focused crates (core / protocol / store / search / spotify / player / sync / mcp / cli / tui / daemon / system / audio / lyrics / config / launcher / provider-fake). The daemon owns runtime state; everything else is a client.

## Target shape

```text
TUI / CLI / MCP / macOS / scripts / agents
                     |
                 local JSON IPC
                     |
                   daemon
                     +-- SQLite cache
                     +-- Tantivy index
                     +-- Spotify Web API (metadata, account state, remote transport)
                     +-- embedded librespot (local Spotify Connect player)
```

SQLite is the local source of truth for cached Spotify metadata. Tantivy is rebuildable from SQLite. Spotify remains the remote authority for account state. Embedded librespot is the local Spotify Connect device and local transport. The Spotify provider transport also handles operations against other Connect devices; local playback does not require a spotifyd subprocess.

## Playback observations are ordered before publication

The playback clock is live runtime state; SQLite holds durable cached snapshots.
On-demand refreshes and scheduled sync capture both the command mutation sequence
and the native player-transition revision before reading a provider. Commit checks
reject superseded observations. The final live-state check runs inside the clock
write lock, including device-activity reconciliation.

Native semantic events update the clock before session tracking or metadata I/O.
They signal a coalescing cache worker, which takes the transport commit lane,
rechecks provider ownership, and snapshots the current clock after waiting.
It persists current state instead of replaying an old event payload. Cache errors
are logged; cancellation or failed I/O can leave the durable snapshot behind.

For the active embedded device, a different-track provider snapshot with an older
state-change timestamp is rejected. Foreign-device snapshots remain reconcilable.
Missing timestamps, equal timestamps and clock skew limit that comparison.
SQLite breaks equal snapshot timestamps by descending insertion ID.

Inspect the live surfaces without changing playback:

```sh
spotuify status --format json
spotuify queue --format json
spotuify daemon status --format json
```

Implementation: [poll commit](crates/spotuify-daemon/src/state.rs),
[clock](crates/spotuify-daemon/src/clock.rs),
[native cache worker](crates/spotuify-daemon/src/native_playback_cache.rs), and
[installed verification](docs/build-log/recovery/evidence/playback-status-installed112.json).

## IPC contract

Transport: length-delimited JSON over a Unix socket on Unix and a Tokio named pipe on Windows, using an envelope like `IpcMessage { id, payload }`, copied/adapted from mxr.

Classify IPC additions into four buckets:

1. `core-music`
   Playback, devices, queue, playlists, library, search, Spotify mutation receipts.
2. `spotuify-platform`
   Cached search, local playlist plans, agent workflows, saved recipes, search/index runtime.
3. `admin-maintenance`
   Status, events, logs, doctor, bug reports, local reset, repair/reindex.
4. `client-specific`
   Pane state, selection state, modal state, grouped UI rows, widget-specific shaping.

Daemon rule: serve reusable truth and workflows, not screen payloads.

Provider rule: Spotify quirks stay below the provider boundary, but capability differences stay visible where behavior differs.

## Player contract

The player is central. If playback is flaky, the app is broken.

- Closing the TUI must never stop playback.
- CLI playback commands must be fast one-shot controllers.
- The daemon owns preferred device activation.
- Embedded librespot is the long-lived Spotify Connect player, hosted in the daemon process.
- Raw Spotify `No active device` errors should become actionable spotuify errors.

## Principles

1. Player first.
2. CLI-first product surface.
3. Daemon-backed architecture.
4. SQLite cache as local truth.
5. Tantivy search is rebuildable derived state.
6. Unix composition through stable JSON/JSONL/CSV/IDs output.
7. Mutations are previewable where feasible.
8. TUI is a client, not the system.
9. Agents use the same CLI humans use.
10. Correctness beats cleverness.

## Copy-from-mxr rule

Do not greenfield shared infrastructure that mxr already solved.

Copy/adapt mxr for:

- daemon lifecycle
- socket IPC
- request/response/event protocol shape
- CLI output formats
- mutation preview/confirmation/receipt helpers
- SQLite source-of-truth patterns
- Tantivy rebuild/index lifecycle
- TUI action dispatch
- contextual hints and command palette
- async result reconciliation

Extract shared crates only after mxr and spotuify both have working copies and the seam is obvious.

## What this means in practice

- CLI, TUI, and agents should reuse daemon workflows instead of inventing separate Spotify logic.
- TUI should shape its own views from reusable daemon data.
- Search/status/doctor/events are protocol surfaces, not debugging leftovers.
- New user-facing capabilities should ship with CLI, TUI, and protocol coverage unless deliberately excluded in the decision log.
- Local cache/search must remain repairable from SQLite.
- `doctor` and player commands must never hang indefinitely on auth-file I/O, network, daemon, or Spotify operations.
