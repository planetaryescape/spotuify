# Testing and Conformance

## Goal

Prevent the app from regressing into a broken TUI by making CLI, protocol, and TUI behavior testable.

## Test layers

### Unit tests

- config parsing
- search spec parsing
- output renderers
- action registry filtering
- selection resolution
- duration parsing for seek

### CLI tests

- help snapshots
- clap parse tests
- JSON output schema tests
- ids output line format
- error exit code mapping

### Daemon tests

- request/response dispatch
- fake Spotify provider
- mutation receipt lifecycle
- event broadcast
- timeout behavior

### Store/search tests

- migrations apply
- sync writes expected rows
- reindex rebuilds Tantivy from SQLite
- local search returns expected IDs

### TUI tests

- action dispatch by context
- text input isolation
- hint selection
- command palette filtering
- multi-select behavior

## Smoke suite

These should be runnable by an agent after changes:

```text
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
./target/release/spotuify doctor
./target/release/spotuify devices --format json
./target/release/spotuify search "luther vandross" --type track --format json
```

Playback smoke tests should be opt-in because they mutate real playback:

```text
SPOTUIFY_LIVE_PLAYBACK=1 ./target/release/spotuify play "luther vandross"
SPOTUIFY_LIVE_PLAYBACK=1 ./target/release/spotuify next
```

## Conformance rule

If a feature appears in TUI, there must be either:

- a CLI command for the same capability, or
- an explicit decision-log entry explaining why it is TUI-only.
