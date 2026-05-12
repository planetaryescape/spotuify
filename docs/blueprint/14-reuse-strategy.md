# spotuify - Reuse Strategy

## Principle

Do not rewrite architecture that mxr already solved.

spotuify should copy, paste, adapt, and then refactor from mxr for shared terminal-app infrastructure. The structs and domain handlers change from mail to music, but daemon lifecycle, IPC, output formats, SQLite, Tantivy, action dispatch, and TUI async reconciliation should be close cousins.

## Copy first, extract later

The right order is:

1. Copy a proven mxr implementation into spotuify.
2. Replace domain structs and request variants.
3. Ship working CLI/TUI behavior.
4. Compare repeated patterns across mxr and spotuify.
5. Extract reusable crates only after the seam is obvious.

Premature shared crates risk freezing the wrong abstraction. Repeated copy/adapt work reveals the right abstraction.

## mxr systems to reuse

| Concern | mxr source area | spotuify adaptation |
|---|---|---|
| IPC codec | `mxr/crates/protocol/src/codec.rs` | same length-delimited JSON shape, spotuify protocol types |
| Protocol model | `mxr/crates/protocol/src/types.rs` | `Request`, `Response`, `DaemonEvent` for music |
| Daemon dispatch | `mxr/crates/daemon/src/handler/mod.rs` | dispatch Spotify/player/search/store requests |
| Socket server | `mxr/crates/daemon/src/server.rs` | same local socket server and event stream |
| CLI entry | `mxr/crates/daemon/src/lib.rs` | parse CLI, ensure daemon, dispatch commands |
| CLI formats | `mxr/crates/daemon/src/output.rs` | table/json/jsonl/csv/ids defaults |
| Selection helpers | `mxr/crates/daemon/src/commands/selection.rs` | IDs, stdin IDs, `--search` selectors for tracks/playlists |
| Mutation helpers | `mxr/crates/daemon/src/commands/mutations/helpers.rs` | dry-run, confirmation, mutation receipts |
| TUI IPC client | `mxr/crates/tui/src/client.rs` | TUI talks to spotuify daemon, not Spotify API |
| TUI async result flow | `mxr/crates/tui/src/async_result.rs`, `mxr/crates/tui/src/lib.rs` | background results reconcile into app state |
| Keybindings | `mxr/crates/tui/src/keybindings.rs` | contextual keymap and multi-key chords |
| Command palette | `mxr/crates/tui/src/ui/command_palette.rs` | music action palette with contexts and recents |
| Hint bar | `mxr/crates/tui/src/ui/hint_bar.rs` | contextual top actions for player/search/playlist/device |
| Diagnostics | `mxr/crates/tui/src/ui/diagnostics_page.rs` | auth/device/API/cache/index diagnostics |
| Action trace | `mxr/crates/tui/src/app/recorder.rs` | debug action recording/export |

## Candidate shared crates

These are not immediate work. They become candidates once mxr and spotuify both carry stable copies.

### `pe-local-ipc`

- length-delimited JSON codec
- Unix socket path resolution
- client/server boilerplate
- request correlation IDs
- event stream support

### `pe-daemon-runtime`

- daemon start/stop/restart/status
- stale socket cleanup
- foreground/background modes
- version mismatch handling
- health snapshots

### `pe-cli-output`

- `table`, `json`, `jsonl`, `csv`, `ids` renderers
- TTY/piped output default selection
- error rendering and exit-code helpers

### `pe-mutations`

- selection resolution
- dry-run preview
- confirmation gates
- mutation receipts
- partial failure reporting

### `pe-ratatui-actions`

- action registry
- keybinding parser
- contextual hint ranking
- command palette primitives
- searchable help model

### `pe-local-search-runtime`

- SQLite source-of-truth patterns
- Tantivy index lifecycle
- reindex status
- freshness metadata
- rebuild-from-store discipline

## Extraction threshold

Extract only when all are true:

- mxr and spotuify both have working copies
- the shared code is at least 80% identical
- the domain-specific hooks are explicit
- tests can cover the shared crate without either app
- extraction reduces code, not just moves code

## Anti-goals

- Do not build a generic framework before spotuify works.
- Do not force music concepts into mail abstractions.
- Do not hide domain behavior behind over-generic trait names.
- Do not make shared crates a blocker for shipping CLI/TUI parity.
