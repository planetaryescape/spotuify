# Consolidated Audit Backlog

Date: 2026-06-19
Base: `124b911` through merged audit reports on `main`

This plan consolidates the five audit lanes:

- `plans/audit/outstanding-work.md`
- `plans/audit/half-finished-features.md`
- `plans/audit/bugs-security.md`
- `plans/audit/races-concurrency.md`
- `plans/audit/quality-architecture.md`

Priority guide:

- P1: user-visible correctness, safety, or architecture contract breach that should be fixed before new feature work.
- P2: important reliability, maintenance, or docs/product-contract gap.
- P3: lower-risk cleanup or polish that still has concrete evidence.

## Recommended execution order

1. Fix queue/radio/mutation safety first: `radio_start`, mutation lanes, playlist remove CLI, dry-run parity, and operation undo gaps overlap.
2. Fix trust-boundary/security issues next: cover-art URL validation, limit caps, updater identity checks, token comparison/log redaction.
3. Repair client/daemon ownership gaps: TUI artist follow state, TUI backend dependencies, boundary tests.
4. Close analytics/scrobbling/MCP live-resource gaps.
5. Clean architecture/docs drift and lower-priority diagnostics/distribution work.

## P1

| Category | Issue | Evidence | Recommended action |
| --- | --- | --- | --- |
| Bugs/security | `radio_start` can queue tracks over MCP without destructive confirmation and defaults to non-dry-run. | `crates/spotuify-mcp/src/tools.rs:233`, `crates/spotuify-mcp/src/confirm.rs:29`, `crates/spotuify-mcp/src/bridge.rs:291`, `crates/spotuify-daemon/src/handlers/library.rs:171` | Treat queueing radio as destructive when `dry_run=false`, default MCP to preview, require confirmation, and route queueing through receipt/operation machinery. |
| Race/concurrency | Several mutating requests bypass `DaemonState::mutation_lane`, including `QueueAddMany`, playlist unfollow/set-image, artist follow/unfollow, and notification queue actions. | `crates/spotuify-daemon/src/state.rs:826`, `crates/spotuify-protocol/src/lib.rs:194`, `crates/spotuify-daemon/src/handlers/playback.rs:694`, `crates/spotuify-daemon/src/handlers/reminders.rs:108` | Make mutation lanes a complete taxonomy; add tests that every mutating request has a lane or explicit exemption. |
| Half-finished | `playlist_remove` exists in MCP/daemon but has no CLI command. | `crates/spotuify-mcp/src/tools.rs:188`, `crates/spotuify-mcp/src/bridge.rs:250`, `crates/spotuify-daemon/src/handlers/playlists.rs:153`, `crates/spotuify-cli/src/cli_args.rs:284` | Add `spotuify playlist remove` with dry-run/confirmation/output support and fake-daemon tests. |
| Half-finished | TUI artist follow/unfollow flips local UI state and ignores daemon failures. | `crates/spotuify-tui/src/app.rs:3742`, `crates/spotuify-tui/src/app.rs:3747`, `crates/spotuify-tui/src/app.rs:3755`, `crates/spotuify-daemon/src/handlers/library.rs:306` | Move optimism to daemon events or remove local flip; surface failures and roll back stale UI state. |
| Outstanding work | `playback_progress` exists and is pruned, but production never inserts samples. | `docs/implementation/13-phase-10-analytics-derivations.md:128`, `docs/implementation/13-phase-10-analytics-derivations.md:132`, `crates/spotuify-store/src/lib.rs:2794`, `crates/spotuify-store/src/listen_facts.rs:440` | Add a bounded daemon playback-progress writer or defer/remove the table/prune surface until there is a writer. |
| Outstanding work | Last.fm scrobbling recipe is a non-posting stub and prints credential-bearing request fields. | `docs/recipes/scrobble-lastfm.sh:7`, `docs/recipes/scrobble-lastfm.sh:30`, `docs/recipes/scrobble-lastfm.sh:43`, `docs/recipes/README.md:35` | Implement signed POST with redacted dry-run, or rename/move it to a clearly non-runnable example. |
| Quality/architecture | TUI depends directly on backend/runtime crates despite the daemon-client architecture. | `AGENTS.md:123`, `AGENTS.md:196`, `crates/spotuify-tui/Cargo.toml:9`, `crates/spotuify-tui/src/app.rs:26`, `crates/spotuify-tui/src/app.rs:2403` | Move TUI backend access behind IPC/launcher adapters and tighten allowed TUI dependencies with a boundary test. |
| Quality/architecture | Workspace boundary tests now bless drift and stale exceptions. | `tests/workspace_boundaries.rs:44`, `tests/workspace_boundaries.rs:87`, `tests/workspace_boundaries.rs:119`, `tests/workspace_boundaries.rs:241`, `crates/spotuify-daemon/Cargo.toml:9` | Split target rules from temporary exceptions, remove stale allowances, and make exceptions expire through explicit follow-ups. |

## P2

| Category | Issue | Evidence | Recommended action |
| --- | --- | --- | --- |
| Bugs/security | Cover-art IPC accepts arbitrary URLs and daemon-fetches them with `reqwest`. | `crates/spotuify-protocol/src/lib.rs:188`, `crates/spotuify-daemon/src/handlers/media.rs:16`, `crates/spotuify-system/src/cover_cache.rs:126`, `crates/spotuify-system/src/cover_cache.rs:203` | Restrict to HTTPS and allowed Spotify image hosts; reject loopback/private/link-local targets and unsafe redirects before request. |
| Bugs/security | Public request limits are unbounded at daemon/store boundaries. | `crates/spotuify-cli/src/cli_args.rs:399`, `src/main.rs:195`, `crates/spotuify-daemon/src/handlers/library.rs:24`, `crates/spotuify-daemon/src/handler.rs:574`, `crates/spotuify-store/src/lib.rs:302` | Add central daemon/protocol caps, mirror them in clap parsers, and fail fast with documented validation errors. |
| Bugs/security | macOS updater verifies checksums but not signing identity/notarization. | `clients/macos/Sources/SpotuifyKit/Services/AppUpdater.swift:70`, `clients/macos/Sources/SpotuifyKit/Services/AppUpdater.swift:87`, `clients/macos/Sources/SpotuifyKit/Services/AppUpdater.swift:193`, `clients/macos/scripts/build-dmg.sh:6` | Verify Developer ID signature, Team ID/designated requirement, and Gatekeeper/notarization before staging or swapping. |
| Race/concurrency | First-party bearer refresh is not single-flight. | `crates/spotuify-daemon/src/state.rs:450`, `crates/spotuify-daemon/src/state.rs:1755`, `crates/spotuify-daemon/src/state.rs:1954`, `crates/spotuify-daemon/src/state.rs:1976` | Add an async single-flight guard around first-party bearer acquisition and compare-and-swap rotated refresh-token persistence. |
| Race/concurrency | Reminder firing is fetch-then-insert without atomically claiming the due row. | `crates/spotuify-daemon/src/reminders.rs:78`, `crates/spotuify-daemon/src/reminders.rs:114`, `crates/spotuify-store/src/lib.rs:1638`, `crates/spotuify-store/src/lib.rs:1650`, `crates/spotuify-store/src/lib.rs:3207` | Claim due reminders with a conditional update/transaction before emitting and add uniqueness on `(reminder_id, due_at_ms)`. |
| Half-finished | Artist follow/unfollow operation records cannot be undone. | `crates/spotuify-protocol/src/operations.rs:173`, `crates/spotuify-protocol/src/operations.rs:228`, `crates/spotuify-daemon/src/handlers/library.rs:285`, `crates/spotuify-daemon/src/handlers/library.rs:319` | Add pre-state/reversal plans, mark operations reversible, and wire undo execution. |
| Half-finished | Non-dry-run `radio_start` queues tracks outside the normal mutation/receipt path. | `crates/spotuify-mcp/src/tools.rs:232`, `crates/spotuify-mcp/src/bridge.rs:291`, `crates/spotuify-daemon/src/handlers/library.rs:171`, `crates/spotuify-daemon/src/handlers/library.rs:176` | Route radio queueing through `QueueAddMany`/operation receipts or return structured queued/failed counts. |
| Half-finished | Playlist set-image and unfollow lack CLI dry-run previews. | `crates/spotuify-cli/src/cli_args.rs:375`, `crates/spotuify-cli/src/commands.rs:1053`, `crates/spotuify-cli/src/cli_args.rs:359`, `crates/spotuify-cli/src/commands.rs:1064` | Add `--dry-run` JSON previews and no-mutation tests for both destructive commands. |
| Outstanding work | ListenBrainz scrobble recipe references missing `spotuify analytics show <uri>`. | `docs/recipes/scrobble-listenbrainz.sh:31`, `docs/recipes/scrobble-listenbrainz.sh:53`, `crates/spotuify-cli/src/cli_args.rs:515` | Add the enrichment command only if scrobbling is validated; otherwise update recipes to use existing surfaces honestly. |
| Outstanding work | MCP lyrics resource is subscribable but never invalidated. | `crates/spotuify-mcp/src/resources.rs:41`, `crates/spotuify-mcp/src/resources.rs:71`, `crates/spotuify-mcp/tests/resources.rs:96`, `docs/implementation/11-phase-8-mcp-server.md:67` | Invalidate lyrics on playback changes or mark lyrics non-subscribable until a specific event exists. |
| Outstanding work | `--no-media-controls` remains documented, but only `SPOTUIFY_NO_MEDIA_CONTROLS` exists. | `docs/implementation/17-phase-14-system-integration.md:107`, `docs/blueprint/13-decision-log.md:536`, `crates/spotuify-daemon/src/state.rs:2220` | Add the daemon flag or update docs/help to settle on the env-var interface. |
| Outstanding work | Linux musl/arm64, AUR, Scoop, `.deb`, and signing follow-ups remain unshipped. | `docs/implementation/14-phase-11-cross-platform.md:105`, `docs/implementation/14-phase-11-cross-platform.md:163`, `.github/workflows/release.yml:53` | Keep as release-ops backlog; prioritize channels only when demand validates them. |
| Quality/architecture | Root `src/main.rs` remains a large assembly plus business-logic module. | `docs/implementation/10-phase-7-workspace-split.md:16`, `docs/implementation/10-phase-7-workspace-split.md:72`, `src/main.rs:1`, `src/main.rs:44`, `src/main.rs:2454` | Move service install, bug report, logs, analytics/ops, and cache maintenance handlers into the appropriate crates. |
| Quality/architecture | Architecture docs disagree on current crate topology. | `docs/blueprint/01-architecture.md:105`, `ARCHITECTURE.md:7`, `AGENTS.md:57`, `AGENTS.md:234`, `crates/spotuify-launcher/Cargo.toml:1` | Make one current crate-map source of truth and update `AGENTS.md`, `ARCHITECTURE.md`, and blueprint docs. |

## P3

| Category | Issue | Evidence | Recommended action |
| --- | --- | --- | --- |
| Bugs/security | MCP bearer comparison is normal string equality, not constant-time. | `crates/spotuify-mcp/src/http.rs:24`, `crates/spotuify-mcp/src/http.rs:100`, `crates/spotuify-mcp/src/http.rs:102`, `docs/security-audit-rubric-v2.md:92` | Parse bearer token and compare candidate token bytes with a constant-time equality helper. |
| Bugs/security | Hook commands can leak embedded secrets into logs and bug reports. | `crates/spotuify-daemon/src/hook_executor.rs:80`, `crates/spotuify-system/src/hooks.rs:224`, `src/main.rs:2516`, `src/main.rs:2549` | Log executable/hash/redacted command only and apply URL/token redaction to log tails in bug reports. |
| Race/concurrency | Diagnostic event log drops events whenever the ring lock is busy. | `crates/spotuify-daemon/src/state.rs:432`, `crates/spotuify-daemon/src/state.rs:2558`, `crates/spotuify-daemon/src/state.rs:2565`, `crates/spotuify-daemon/src/state.rs:2577` | Use a bounded append worker/non-async ring lock, or expose dropped-event counts in diagnostics. |
| Half-finished | MCP `now_playing` promises lyrics but only requests playback. | `crates/spotuify-mcp/src/tools.rs:55`, `crates/spotuify-mcp/src/bridge.rs:91`, `crates/spotuify-core/src/lib.rs:28`, `crates/spotuify-protocol/src/lib.rs:1148` | Either fix the manifest copy or aggregate `PlaybackGet` plus `LyricsGet`. |
| Half-finished | macOS saved-shows view is not refreshed on `LibraryChanged`. | `clients/macos/Sources/SpotuifyKit/Stores/LibraryStore.swift:35`, `clients/macos/Sources/SpotuifyKit/Stores/LibraryStore.swift:80`, `crates/spotuify-daemon/src/handlers/library.rs:240` | Include `loadShows(force: true)` in the observer or refresh only affected collection from event URI. |
| Outstanding work | Playlist planner is intentionally a deterministic scaffold/stub. | `docs/implementation/06-phase-5-agent-playlists.md:15`, `crates/spotuify-protocol/src/agent_playlists.rs:94`, `crates/spotuify-cli/src/cli_args.rs:284` | Do not expand unless validated; make CLI/docs copy say "scaffold" or "heuristic" clearly. |
| Outstanding work | TUI cover-art terminal protocol reporting remains a diagnostics follow-up. | `docs/implementation/18-phase-15-cover-art.md:97`, `docs/implementation/18-phase-15-cover-art.md:131`, `crates/spotuify-tui/src/app.rs:768` | Add TUI-local diagnostics/status for selected image protocol and fallback reason. |
| Quality/architecture | Output-format helper logic is duplicated across CLI, daemon, and root. | `AGENTS.md:128`, `crates/spotuify-cli/src/output.rs:1511`, `crates/spotuify-daemon/src/status.rs:6`, `crates/spotuify-daemon/src/diagnostics.rs:808`, `src/main.rs:2443` | Centralize CSV/JSONL primitive helpers below CLI/daemon, then snapshot existing output behavior. |

## Validation gates for backlog work

Use targeted checks while fixing each item, then run broader gates once several items land:

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `scripts/cargo-nextest -p <crate>`
- `scripts/cargo-test --workspace`
- `scripts/smoke.sh`
- Real CLI verification for user-facing paths, using `./target/release/spotuify <subcommand>` where relevant.

## Audit artifacts

All lane reports were merged as report-only commits. No source code was changed by this audit pass. The main checkout still has the user's pre-existing untracked `TODO.md`.
