# spotuify - Project Context for AI Agents

> A daemon-backed, CLI-first, keyboard-native Spotify controller and music library runtime for terminal users, built around a local cache, pipeable commands, and an impeccable player experience.

## Use spotuify to build spotuify (read this before debugging anything)

**When investigating a runtime bug, drive the binary yourself. Do not ask the user to rebuild, reproduce, and screenshot — that loop is slow and leaves blind spots between your guesses.**

Spotuify holds a hard contract: **every feature must be exposed via the CLI.** Anything the TUI / MCP / daemon can do, `spotuify <subcommand>` can do too. That contract exists in part so you (as an agent) can drive every feature end-to-end without a human in the loop. If you find a feature that only lives in the TUI or MCP, that's a CLI gap to file, not a "needs the user" situation.

The user maintains the live state of their machine and account; you have everything you need to verify your own changes:

- **Build:** `cargo build --release --bin spotuify`
- **Restart daemon:** `./target/release/spotuify daemon stop && ./target/release/spotuify daemon start`
- **Run the failing case:** `./target/release/spotuify <subcommand>` (`search`, `play`, `queue add`, `playlists`, etc.)
- **Read the daemon log:** `~/Library/Logs/spotuify/spotuify.log` — filter out tantivy noise with `grep -v tantivy`
- **Auth token for raw Spotify probes:** `security find-generic-password -s spotuify -w` returns a JSON blob with `.access_token`. Pipe it to `curl -H "Authorization: Bearer $TOKEN" https://api.spotify.com/v1/...` to probe the upstream directly.
- **Tantivy lockfile stuck:** `rm ~/Library/Application\ Support/spotuify/search_index/.tantivy-*.lock` after killing the daemon.
- **Stray daemons:** `pkill -9 -f 'spotuify daemon'`. If the daemon won't start with a `LockBusy` error, that's the cause.

**Only ask the user when the case genuinely needs human judgment:** visual TUI verification, a Spotify-account-state question (e.g. "are you on Premium?"), or a product decision. If you find yourself guessing limit values, parameter shapes, or what an error means — stop, drive the case, and read the actual response. One direct empirical test usually replaces five rounds of guesses.

The lesson came from the 2026-05-17 search-limit debug: I spent five iterations guessing `limit` values when one `curl` bisect across 1..50 would have given the truth. Don't repeat that.

### Operating rules

- **`cargo test` passing is necessary but not sufficient.** Unit tests can be all-green while a real workflow stays broken because no test exercised the daemon → store → Spotify → render path end-to-end. The CLI loop above is the integration gate.
- **JSON output is for you.** Most CLI surfaces accept `--format json` / `--format ids` and emit stable structured output. Read it, parse it (`jq`, `python3 -c`), act on it. Don't scrape human-formatted tables.
- **When something breaks, use spotuify to debug spotuify.** `spotuify doctor`, `spotuify daemon status`, `spotuify logs tail`, `spotuify ops list` all expose internal state. Reach for them before adding `eprintln!` or extra logging — the diagnostics that exist already cost time when you don't use them.
- **Adding a feature means adding both the CLI subcommand and the TUI / MCP surface.** The CLI is verified by you; the TUI is verified by humans. Wire both or wire neither — a feature that only lives in the TUI is incomplete.
- **The CLI is your API.** If you find yourself wanting to reach for an internal Rust function from a test, the right answer is usually "add the CLI subcommand and call that." That keeps the contract honest: every feature must serve a non-TUI user, and every test you write via the CLI is also a test that the contract still holds.

The CLI-everywhere contract is non-negotiable. You ARE one of the agents this project is designed for — working through the CLI keeps the project honest.

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
- Playback device: Spotify Connect via embedded librespot (in-daemon) or another visible Spotify device.
- Target database: SQLite.
- Target search: Tantivy.
- Target IPC: length-delimited JSON over Unix socket, copied/adapted from mxr.

## Current architecture

The codebase is a Cargo workspace split into focused crates (`spotuify-core`, `spotuify-protocol`, `spotuify-store`, `spotuify-search`, `spotuify-spotify`, `spotuify-player`, `spotuify-sync`, `spotuify-mcp`, `spotuify-cli`, `spotuify-tui`, `spotuify-daemon`, `spotuify-system`, `spotuify-audio`, `spotuify-lyrics`, `spotuify-keychain`). The daemon owns runtime state; everything else is a client.

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

## Daemon owns state. Clients are views.

The daemon is the canonical state holder for everything user-visible: current playback, queue, devices, search results, library, recent. Clients — TUI, CLI, MCP, future ones — render that state. They never own it.

**Optimistic UI updates live on the daemon, not in any client.** When a transport mutation lands, the daemon updates its `playback_clock` and emits a `DaemonEvent::PlaybackChanged { action: "optimistic-<verb>", playback: Some(snapshot) }` immediately, before the Spotify API call returns. Every subscriber — the TUI's event listener, a CLI `--watch` window, MCP — sees the same instant feedback. The authoritative `PlaybackChanged` from the command result follows and reconciles via the clock's source-priority ranking.

When you're tempted to write `app.<field> = <new_value>` in the TUI to "make it feel snappier", stop. That mutation is invisible to every other client. Push it to the daemon as an `optimistic-*` event emit and let the existing subscriber path render it. The TUI's `merge_playback` / `handle_art_url_change` / queue-rail / devices-rail already react to those events — nothing to invent on the client side.

The rule covers more than playback: queue reorders, library saves, playlist edits, search results — all of them. If a state field is read by the user, the daemon owns it; the client subscribes.

## Release Shorthand

- User phrase `ship it` means full release flow, not just a local commit.
- Required sequence:
  1. Commit release-ready changes.
  2. If the current version/tag already exists, bump to the next release version first. Never try to overwrite an existing tag, GitHub release, or release asset.
  3. Push `main`.
  4. Create and push the release tag `v{version}`.
  5. Wait for the tag-driven release workflow to finish: binaries, GitHub Release, Homebrew update.
  6. Verify install surfaces against the released version:
     - `brew install planetaryescape/spotuify/spotuify` / `brew upgrade planetaryescape/spotuify/spotuify`
     - `cargo install --git https://github.com/planetaryescape/spotuify --tag v{version} --locked spotuify`
  7. Report final released version and any install lag/failures.
- Release artifacts generated locally (`spotuify-v*.tar.gz`, `spotuify-v*.zip`, checksums, generated Formula files) are not source files. Do not commit them. Delete or ignore them after verification.

## Core principles

1. **Player first**: If play/pause/seek/queue/device activation is flaky, the app is broken.
2. **CLI first**: The CLI is the canonical user, script, test, and agent surface. If a capability only exists in the TUI, it is incomplete.
3. **Daemon-backed**: TUI is a client, not the system. Music keeps playing after TUI exits. State changes — even optimistic ones — originate from the daemon.
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

Keychain, Spotify Web API, embedded librespot, daemon IPC, and image loading must have bounded failure behavior. Never let TUI input, `doctor`, or CLI commands hang indefinitely.

## Target crate dependency rules

These apply once spotuify becomes a workspace:

1. `core` depends on nothing internal.
2. `protocol` depends only on `core`.
3. `store` and `search` depend only on `core`.
4. `spotify` maps Spotify Web API into core types. It does not depend on daemon, TUI, CLI, store, or search.
5. `player` owns device activation and embedded librespot (Spirc) orchestration.
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

### Running tests

- Use `scripts/cargo-test` as the canonical test runner. It reaps stale `cargo` processes and points `CARGO_TARGET_DIR` at `target-cli/`, isolating CLI cargo from rust-analyzer's locks on `target/`.
- Prefer `scripts/cargo-test -p <crate> --tests` over `--workspace`. The workspace is 14 crates; a full test/build commonly runs minutes and overruns agent Bash timeouts, leaving orphaned `cargo` holding the target-dir lock.
- Never pipe `cargo test` through `tail`/`head`/`grep` inside an agent Bash invocation. The pipeline can outlive the parent shell when the Bash tool times out; the orphaned `cargo` then blocks the next run. Capture full output, or use `--quiet`, or run via `scripts/cargo-test` which handles cleanup.
- If a run unexpectedly hangs, `pgrep -f 'cargo test'` first; zombies from prior turns are the most common cause.

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
