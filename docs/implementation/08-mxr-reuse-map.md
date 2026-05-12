# mxr Reuse Map

This document is the implementation checklist for copying mxr-proven infrastructure into spotuify.

## Rule

Before writing new daemon/IPC/store/search/TUI infrastructure, inspect the corresponding mxr implementation. Copy the simplest working shape, then adapt names and domain structs.

## Phase 1 reuse: CLI output and mutations

Copy/adapt:

- `mxr/crates/daemon/src/output.rs`
- `mxr/crates/daemon/src/commands/selection.rs`
- `mxr/crates/daemon/src/commands/mutations/helpers.rs`

Adaptation:

- replace message IDs with Spotify URIs/IDs
- replace mail search selectors with track/playlist/search selectors
- keep output format defaults and renderer discipline
- keep dry-run/confirmation semantics

Verification:

- `spotuify search --format json|jsonl|csv|ids`
- `spotuify playlist add --dry-run`
- stdin IDs work for queue/playlist operations

## Phase 2 reuse: daemon and IPC

Copy/adapt:

- `mxr/crates/protocol/src/codec.rs`
- `mxr/crates/protocol/src/types.rs` shape
- `mxr/crates/daemon/src/server.rs`
- `mxr/crates/daemon/src/handler/mod.rs`
- `mxr/crates/tui/src/client.rs`

Adaptation:

- define `spotuify_protocol::{Request, Response, ResponseData, DaemonEvent}`
- route playback/search/playlist/library/device requests
- keep request IDs and response correlation
- keep daemon event stream model

Verification:

- CLI and TUI both talk over socket
- daemon survives TUI exit
- two CLI commands can run while TUI is open
- events appear in TUI without polling-only refresh

## Phase 3 reuse: SQLite and Tantivy lifecycle

Copy/adapt patterns from mxr store/search/sync rather than exact mail tables.

Relevant mxr concepts:

- SQLite is source of truth
- Tantivy is rebuildable derived state
- sync writes store first, index second
- diagnostics report index freshness
- reindex command repairs derived state

Adaptation:

- music schema: tracks, albums, artists, playlists, playlist_items, library_items, search_runs
- Tantivy docs represent media/searchable playlist rows
- remote search cache becomes indexed data after fetch

Verification:

- delete/rebuild index from SQLite
- local search works with network disabled
- cache status reports row counts and index freshness

## Phase 4 reuse: TUI action and UX plumbing

Copy/adapt:

- `mxr/crates/tui/src/keybindings.rs`
- `mxr/crates/tui/src/action.rs`
- `mxr/crates/tui/src/ui/hint_bar.rs`
- `mxr/crates/tui/src/ui/command_palette.rs`
- `mxr/crates/tui/src/ui/help_modal.rs`
- `mxr/crates/tui/src/ui/status_bar.rs`
- `mxr/crates/tui/src/ui/error_modal.rs`
- `mxr/crates/tui/src/async_result.rs`

Adaptation:

- action contexts: player, search, library, playlists, queue, devices, diagnostics
- commands: play/pause, next, repeat, queue, like, add to playlist, transfer device
- hints: max five contextual actions
- palette: filter invalid commands by context
- status priority: blocking error, pending work, toast, playback status

Verification:

- text search input does not trigger global playback keys
- hint bar changes by tab/selection/multi-select
- command palette exposes all valid CLI-equivalent actions
- blocking errors open modal; transient statuses stay in status bar

## Phase 5 reuse: docs and recipes

Copy/adapt mxr's docs style:

- blueprint index
- decision log
- implementation journey
- CLI recipes
- agent guide
- JSON output reference

spotuify-specific recipes should cover:

- `fzf` track picker
- `jq` playlist candidate filtering
- agent-created playlist preview/commit
- one-shot playback controls from shell

## When to extract shared crates

Do not extract during first copy. Add TODO comments or docs notes for repeated identical code. Extract only after spotuify has working daemon, IPC, CLI output, and TUI action plumbing.
