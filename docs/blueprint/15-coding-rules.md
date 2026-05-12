# spotuify - Coding Rules

## Principle

spotuify should be boring inside and delightful outside. Use plain Rust, clear boundaries, explicit errors, and testable command surfaces.

## Architecture rules

- Daemon owns durable runtime state.
- CLI and TUI are clients.
- TUI must not directly own Spotify business logic once daemon exists.
- Protocol data is reusable domain data, not screen-specific payloads.
- SQLite is source of truth for local cache.
- Tantivy is rebuildable derived state.
- Spotify provider maps remote data into core model; app logic should not spread Spotify JSON quirks everywhere.

## CLI/TUI parity rules

- If a feature only exists in TUI, it is incomplete.
- If a feature only exists in CLI because it is intentionally non-visual/admin, document that decision.
- New protocol requests should get CLI coverage first or at the same time as TUI coverage.
- CLI output must be usable by agents and shell tools.

## Error handling rules

- No raw Spotify error should be the final UX if it can be mapped to an actionable domain error.
- Keychain, network, IPC, spotifyd, image loading, and search indexing must have bounded failure behavior.
- `doctor` must never hang indefinitely.
- TUI input handling must never await network or keychain work.

## Mutation rules

- Bulk/destructive mutations need `--dry-run` where feasible.
- Dry-run must use the same selection path as commit.
- Mutation results should include receipts with counts and partial failures.
- Non-TTY broad mutations should fail closed unless `--yes` is passed.

## Output rules

Data commands should support:

- `table`
- `json`
- `jsonl`
- `csv`
- `ids`

Human table output can change. Machine output should be treated as a compatibility contract.

## TUI rules

- Contextual hint bar shows at most five actions.
- Command palette hides irrelevant actions and explains disabled actions.
- Text input captures keys before global shortcuts.
- Hidden panes are not focusable.
- Empty states teach the next action.
- Blocking errors use a modal; transient messages use status bar.
- Closing TUI does not mutate playback.

## Search rules

- Search filters live in shared `SearchSpec`, not TUI-only state.
- Local search and remote Spotify search are separate sources.
- Remote search results should be cacheable.
- Index rebuild from SQLite must be possible.

## Copy-from-mxr rules

Before writing new infrastructure, inspect mxr for a solved implementation.

Copy/adapt first for:

- IPC codec and socket client/server
- daemon lifecycle
- output formats
- selection helpers
- mutation helpers
- TUI keybinding/action/hint/palette system
- async result reconciliation
- SQLite/Tantivy lifecycle

Do not extract shared crates until repeated working code proves the abstraction.

## Verification rules

Current required checks:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
./target/release/spotuify doctor
```

As CLI parity lands, every new command should add a smoke-testable path.
