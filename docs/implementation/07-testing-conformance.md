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

These should be runnable by an agent after changes without hitting Spotify's live API:

```text
scripts/smoke.sh
```

The default smoke script uses `SPOTUIFY_FAKE_SPOTIFY=1` with an isolated temp runtime for CLI doctor/devices/search checks. Live Spotify API smoke checks are opt-in only to avoid looking like malicious traffic:

```text
SPOTUIFY_LIVE_API=1 scripts/smoke.sh
```

Playback smoke tests are separately opt-in because they mutate real playback:

```text
SPOTUIFY_LIVE_PLAYBACK=1 ./target/release/spotuify play "luther vandross"
SPOTUIFY_LIVE_PLAYBACK=1 ./target/release/spotuify next
```

Do not add normal unit/integration tests that repeatedly call Spotify's live API. Use the fake provider by default and reserve live checks for explicit, manually requested smoke runs.

## Conformance rule

If a feature appears in TUI, there must be either:

- a CLI command for the same capability, or
- an explicit decision-log entry explaining why it is TUI-only.
