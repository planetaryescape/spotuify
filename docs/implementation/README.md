# spotuify - Implementation Plan

This directory is the execution plan for the blueprint.

The plan is phased so each phase leaves the app more useful than before. Do not skip CLI verification. Do not add TUI-only behavior.

## Documents

| # | Document | Purpose |
|---|---|---|
| 00 | [Current State](00-current-state.md) | What exists today and what is broken or missing |
| 01 | [Phase 0 - Stabilize](01-phase-0-stabilize.md) | Fix current app and establish smoke checks |
| 02 | [Phase 1 - CLI Parity](02-phase-1-cli-parity.md) | Extract shared actions and add CLI for current features |
| 03 | [Phase 2 - Daemon Protocol](03-phase-2-daemon-protocol.md) | Add daemon, IPC, CLI/TUI clients |
| 04 | [Phase 3 - Local Store and Search](04-phase-3-local-store-search.md) | SQLite cache, sync, Tantivy index |
| 05 | [Phase 4 - TUI Redesign](05-phase-4-tui-redesign.md) | Player-first mxr-style TUI UX |
| 06 | [Phase 5 - Agent Playlists](06-phase-5-agent-playlists.md) | Research/preview/commit playlist workflows |
| 07 | [Testing and Conformance](07-testing-conformance.md) | CLI/TUI/protocol test strategy |
| 08 | [mxr Reuse Map](08-mxr-reuse-map.md) | Concrete source areas to copy/adapt from mxr |

## Implementation rules

1. CLI first or same time as TUI.
2. Shared action/protocol layer before UI-specific code.
3. Every external dependency has a timeout.
4. Every broad mutation has dry-run or explicit reason why impossible.
5. Every machine-readable output has a stable schema.
6. Every phase includes commands an agent can run to verify it.
7. Before implementing daemon, IPC, SQLite, Tantivy, output formats, or TUI async flow, inspect mxr and copy/adapt the proven code path.
