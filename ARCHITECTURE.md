# Architecture

spotuify is intended to be daemon-backed music infrastructure. The daemon is the system. TUI, CLI, scripts, and agents are clients.

For the full design record, read [docs/blueprint/README.md](docs/blueprint/README.md). This file is the short version.

## Current state

The codebase is currently a single Rust binary. The TUI, CLI setup commands, Spotify Web API client, auth, config, logging, and spotifyd helpers live in one package.

That is an implementation waypoint, not the final architecture.

## Target shape

```text
TUI / CLI / scripts / agents
              |
              v
            daemon
         /     |      \
    SQLite  Tantivy   player runtime
                       |
              Spotify Web API + Spotify Connect device
                       |
                 spotifyd / official Spotify app
```

SQLite is the local source of truth for cached Spotify metadata. Tantivy is rebuildable from SQLite. Spotify remains the remote authority for account state. spotifyd or another Spotify Connect device is the audio player.

## IPC contract

Target transport: length-delimited JSON over a Unix socket using an envelope like `IpcMessage { id, payload }`, copied/adapted from mxr.

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
- spotifyd is a long-lived Spotify Connect player, not TUI state.
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
- `doctor` and player commands must never hang indefinitely on keychain, network, daemon, or Spotify operations.
