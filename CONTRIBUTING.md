# Contributing to spotuify

## Dev setup

```bash
cargo build --locked
cargo test --locked
```

Run locally:

```bash
cargo run -- --help
cargo run -- doctor
cargo run
```

## Non-negotiables

- Player reliability first.
- CLI is the canonical product surface.
- TUI and CLI are clients of the same runtime.
- Closing the TUI must not stop playback.
- SQLite cache is the target local source of truth.
- Tantivy index is rebuildable derived state.
- JSON/JSONL/CSV/IDs output must stay pipeable.
- Mutations are previewable where feasible.
- Agents use the same CLI humans use.
- Spotify provider quirks stay below provider/player boundaries.
- Copy/adapt mxr-proven infrastructure before inventing.

## Current required checks

Run all of these before sending changes:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
scripts/smoke.sh
```

`scripts/smoke.sh` uses the fake Spotify provider by default. Do not repeatedly run live Spotify API checks from tests or agent smoke runs.

## Real-system verification

Green unit tests are not enough. For user-facing work, exercise the real CLI/TUI surface.

Today:

```bash
scripts/smoke.sh
./target/release/spotuify --help
```

Live read-only Spotify API checks are opt-in:

```bash
SPOTUIFY_LIVE_API=1 scripts/smoke.sh
./target/release/spotuify devices --format json
./target/release/spotuify status --format json
./target/release/spotuify search "luther vandross" --type track --format json
```

Playback mutation checks should be opt-in because they affect the user's active Spotify session:

```bash
SPOTUIFY_LIVE_PLAYBACK=1 ./target/release/spotuify play "luther vandross"
SPOTUIFY_LIVE_PLAYBACK=1 ./target/release/spotuify next
```

## IPC boundaries

When daemon IPC exists, classify every new request before adding it:

1. `core-music`
2. `spotuify-platform`
3. `admin-maintenance`
4. `client-specific`

Rules:

- Core music should stay boring and stable.
- Platform capabilities are first-class.
- Admin surfaces stay separate from the core music contract.
- Client-specific shaping stays in clients.
- The daemon serves reusable truth/workflows, not screen payloads.

## Target crate boundaries

Keep these intact once the workspace split happens:

1. `core` depends on nothing internal.
2. `protocol` depends only on `core`.
3. `store` and `search` depend only on `core`.
4. `spotify` maps Web API data into core types and does not depend on clients.
5. `player` owns device activation and spotifyd/librespot orchestration.
6. `sync` orchestrates provider/player/store/search.
7. `daemon` is the integration point.
8. `cli` and `tui` are clients; they do not depend on store/search/sync/provider internals.
9. Do not use `#[path]` includes to simulate crate boundaries.

## Rules for changes

- Keep blast radius small.
- Do not refactor adjacent code unless the task requires it.
- Add tests for new behavior.
- Prefer integration coverage over mock-heavy unit tests.
- Do not wire a daemon feature for only one client surface when both TUI and CLI need it.
- Do not add TUI-only Spotify behavior.
- Update docs when behavior or architecture changes.
- Do not store tokens or secrets in docs, logs, fixtures, or bug reports.

## Copy-from-mxr workflow

For daemon, IPC, SQLite, Tantivy, CLI output, mutation helpers, and TUI UX plumbing:

1. Inspect the mxr implementation.
2. Copy the smallest proven shape.
3. Rename and adapt domain structs.
4. Keep behavior tests or smoke commands.
5. Only extract a shared crate after mxr and spotuify both have working copies.

Reference:

- `docs/blueprint/14-reuse-strategy.md`
- `docs/implementation/08-mxr-reuse-map.md`

## Spotify-specific cautions

- Spotify Web API controls playback but does not stream audio.
- Playback requires a visible Spotify Connect device and Spotify Premium.
- Queue removal/reorder is not exposed by the Web API.
- Official lyrics are not exposed by the Web API.
- Respect rate limits and `Retry-After`.
- Search API limits and supported endpoints may change; verify against current docs when errors come from Spotify.

## Docs hygiene

- Update `README.md` for changed user-facing behavior.
- Update `docs/blueprint/` when architecture or product rules change.
- Update `docs/implementation/` when execution plan changes.
- Update `docs/blueprint/13-decision-log.md` for settled architectural decisions.
- Keep `ARCHITECTURE.md` short and current.

## Architecture pointer

Start with [ARCHITECTURE.md](ARCHITECTURE.md), then read [docs/blueprint/README.md](docs/blueprint/README.md) and [docs/implementation/README.md](docs/implementation/README.md).
