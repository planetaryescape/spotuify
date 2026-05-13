# spotuify - Project Context for AI Agents

> A daemon-backed, CLI-first, keyboard-native Spotify controller and music library runtime for terminal users, built around a local cache, pipeable commands, and an impeccable player experience.

## Name

- Write: `spotuify` lowercase, in code font when inline.
- Package / binary: `spotuify`.
- Preferred local playback device name on this machine: `spotuify-hume`.

## Language and stack

- Language: Rust edition 2021.
- Async runtime: Tokio.
- TUI: Ratatui + crossterm.
- HTTP: reqwest.
- Config: TOML + serde.
- Credentials: system keyring, currently macOS Keychain.
- Playback device: Spotify Connect via spotifyd/librespot or another visible Spotify device.
- Target database: SQLite.
- Target search: Tantivy.
- Target IPC: length-delimited JSON over Unix socket, copied/adapted from mxr.

## Current architecture

Current code is a single binary. `src/app.rs` owns most TUI state/actions, `src/spotify.rs` owns Spotify Web API calls, `src/auth.rs` owns OAuth/keychain, `src/config.rs` owns config, and `src/spotifyd.rs` owns spotifyd startup helpers.

Do not mistake current shape for the target architecture.

## Target architecture

Daemon-backed. The daemon is the system. TUI, CLI, scripts, and agents are clients connected by local JSON IPC.

```text
TUI / CLI / Scripts / Agents  <--- Unix socket JSON --->  Daemon
                                                              |
                                                Store SQLite + Search Tantivy
                                                              |
                                         Spotify Web API + Spotify Connect device
```

Single unified binary: `spotuify` with subcommands. `spotuify` opens TUI, `spotuify daemon` manages daemon lifecycle, and one-shot commands like `spotuify next` talk to the daemon.

## IPC contract boundary

Classify future IPC into four buckets:

1. `core-music`: playback, devices, queue, playlists, library, search, Spotify mutations.
2. `spotuify-platform`: cache/index state, agent playlist plans, saved recipes, local workflow features.
3. `admin-maintenance`: status, events, logs, doctor, bug reports, reset, repair, reindex.
4. `client-specific`: pane state, selected row, modal state, view-only grouping; keep this out of daemon IPC.

Rules:

- The daemon serves reusable truth/workflows, not screen payloads.
- Product/platform capabilities are first-class surfaces, not leftovers.
- Admin surfaces stay in IPC but stay conceptually separate from core music.
- Spotify weirdness is handled below this layer in provider/player code.

## Core principles

1. **Player first**: If play/pause/seek/queue/device activation is flaky, the app is broken.
2. **CLI first**: The CLI is the canonical user, script, test, and agent surface. If a capability only exists in the TUI, it is incomplete.
3. **Daemon-backed**: TUI is a client, not the system. Music keeps playing after TUI exits.
4. **Local cache**: SQLite is the local source of truth for cached metadata. Spotify remains remote authority.
5. **Search is navigation**: Tantivy/local search is a first-class feature, not a UI filter bolted on later.
6. **Pipeable output**: JSON/JSONL/CSV/IDs output is a product feature.
7. **Safe mutations**: Broad/destructive mutations need dry-run or explicit documented reason why impossible.
8. **Agents use normal commands**: No hidden agent-only backdoor.
9. **Copy mxr first**: Reuse mxr architecture/plumbing before inventing new infrastructure.
10. **Correctness beats cleverness**: Plain Rust, explicit errors, bounded timeouts, boring persistence.

## Development principles

### CLI first, TUI supported

New capabilities should land in CLI first or at the same time as TUI. The CLI is the fastest path to verification, scripting, and agent use.

### Test with real command surfaces

Green unit tests are not enough. For user-facing work, verify with CLI commands that exercise the real flow.

Current checks:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
scripts/smoke.sh
```

`scripts/smoke.sh` uses the fake Spotify provider by default. Do not add tests or default agent smoke checks that repeatedly call Spotify's live API.

Live read-only Spotify API checks are opt-in only:

```bash
SPOTUIFY_LIVE_API=1 scripts/smoke.sh
```

Playback mutation smoke checks should be opt-in because they affect real playback.

### Wire both clients or wire neither

The target system has TUI and CLI as clients of the same daemon request. Every new protocol capability should have:

- daemon handler
- CLI subcommand
- TUI action if useful interactively
- output or event shape
- tests or smoke path

If a capability deliberately lacks a surface, document why in `docs/blueprint/13-decision-log.md`.

### Mutations must be previewable

Playlist creation, playlist edits, bulk likes, bulk follows, and batch queue operations need `--dry-run` where feasible. Dry-run should use the same selection path as the real mutation.

### Pipeable JSON is mandatory

Read/list/status/search surfaces must keep machine-readable output stable. Human table output is additive.

### Complete user journeys

A flow is not done until it reaches the user-visible outcome and reports success/failure. For example, "search works" means query parsing, result rendering, selection, play/queue/add actions, and errors are all handled through CLI and TUI.

### Bounded external dependencies

Keychain, Spotify Web API, spotifyd, daemon IPC, and image loading must have bounded failure behavior. Never let TUI input, `doctor`, or CLI commands hang indefinitely.

## Target crate dependency rules

These apply once spotuify becomes a workspace:

1. `core` depends on nothing internal.
2. `protocol` depends only on `core`.
3. `store` and `search` depend only on `core`.
4. `spotify` maps Spotify Web API into core types. It does not depend on daemon, TUI, CLI, store, or search.
5. `player` owns device activation and spotifyd/librespot orchestration.
6. `sync` depends on core/store/search/provider/player and orchestrates data flow.
7. `daemon` is the integration point.
8. `cli` and `tui` are clients. They must not depend on daemon/store/search/sync/provider internals.
9. Architectural seams are Cargo seams. Do not fake crate boundaries with `#[path]` includes.

## Copy-from-mxr implementation policy

Before implementing daemon, IPC, SQLite, Tantivy, output formats, mutation helpers, or TUI action plumbing, inspect mxr and copy/adapt the proven code path.

Reference docs:

- `docs/blueprint/14-reuse-strategy.md`
- `docs/implementation/08-mxr-reuse-map.md`

## Non-negotiables for contributors and agents

- Player reliability is not optional.
- CLI-first product surface.
- TUI is a client, not the system.
- Music keeps playing after TUI exits.
- SQLite cache is local truth; search index is rebuildable.
- JSON/JSONL output must stay pipeable.
- Mutations are dry-runnable where feasible.
- Provider-specific Spotify behavior stays below provider/player boundaries.
- Every external operation has a timeout.
- Copy mxr-proven infrastructure before inventing.
- Keep blast radius small.

## Settled design decisions

See `docs/blueprint/13-decision-log.md` for full context. Highlights:

- Daemon-backed, not TUI-owned.
- CLI is canonical.
- Spotify Connect device handles playback; Web API controls it.
- Local search is cache/Tantivy first, remote Spotify search as provider.
- Output formats are stable product contract.
- Lyrics are optional future provider, not core Spotify Web API capability.
- TUI UX follows contextual action registry.
- Copy mxr before inventing shared infrastructure.

## Documentation map

- `ARCHITECTURE.md` - short architecture summary.
- `docs/blueprint/` - target architecture, product rules, decisions.
- `docs/implementation/` - phased execution plan.
- `CONTRIBUTING.md` - contributor workflow and checks.
