# Quality and Architecture Audit

Worktree: `/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/quality-architecture`
Branch: `codex/audit-quality-architecture-20260619`
Base inspected: `124b911`

## Findings

### P1 - TUI still links backend/runtime crates directly

- Priority: P1
- Issue: The TUI is documented as a daemon client, but `spotuify-tui` depends directly on store, search, Spotify provider, player, sync, CLI, and daemon crates.
- Evidence:
  - `AGENTS.md:123-125` says the TUI is a client and state changes originate from the daemon.
  - `AGENTS.md:196-204` says `cli` and `tui` must not depend on daemon/store/search/sync/provider internals.
  - `crates/spotuify-tui/Cargo.toml:9-18` declares deps on `spotuify-store`, `spotuify-search`, `spotuify-spotify`, `spotuify-player`, `spotuify-sync`, `spotuify-cli`, and `spotuify-daemon`.
  - `crates/spotuify-tui/src/app.rs:26-34` imports CLI action results, protocol IPC, Spotify client types, and config directly.
  - `crates/spotuify-tui/src/app.rs:2403`, `2663`, `2818`, `3053`, and `3071` call daemon server helpers directly from TUI code.
- Impact: Client/runtime separation is compiler-optional rather than enforced. Future TUI work can accidentally bypass IPC, duplicate daemon state ownership, or add features that are not exposed through CLI/MCP.
- Recommended action: Create a narrow `spotuify-launcher`/IPC-facing TUI adapter for daemon lifecycle and move any remaining provider/store/search access behind protocol requests. Tighten `spotuify-tui` allowed deps toward `core + protocol + launcher` plus explicitly documented UI-only helpers.
- Confidence: High
- Validation idea: Add/update a workspace boundary test that fails if `spotuify-tui` depends on `spotuify-store`, `spotuify-search`, `spotuify-spotify`, `spotuify-player`, `spotuify-sync`, `spotuify-cli`, or `spotuify-daemon`, then migrate one edge at a time until it passes.

### P1 - Workspace boundary tests now bless drift instead of preventing it

- Priority: P1
- Issue: `tests/workspace_boundaries.rs` says it encodes the target dependency rules, but its allowlist has been expanded to current implementation edges, including stale exceptions.
- Evidence:
  - `tests/workspace_boundaries.rs:44-55` documents the intended rules, including `spotuify-cli`, `spotuify-tui`, and `spotuify-mcp` as protocol-only.
  - `tests/workspace_boundaries.rs:87-107` allows `spotuify-daemon` to depend on `spotuify-cli` and `spotuify-launcher`.
  - `crates/spotuify-daemon/Cargo.toml:9-23` does not actually depend on `spotuify-cli`, so the allowed `daemon -> cli` edge is stale and would permit regression.
  - `tests/workspace_boundaries.rs:119-135` allows TUI to depend on almost the full backend surface.
  - `tests/workspace_boundaries.rs:241-247` ignores the root binary backend-dependency assertion indefinitely.
- Impact: The main architecture guardrail can pass while the dependency graph moves farther from the daemon/client model. Reviewers lose a quick signal for accidental back-edges.
- Recommended action: Split the test into `target_rules` and `temporary_exceptions`. For each exception, require a linked decision/follow-up and assert only edges that exist today. Remove stale allowances like `daemon -> cli`; convert ignored root assertion into a tracked, explicit TODO test with a narrowing target.
- Confidence: High
- Validation idea: Run `cargo test --test workspace_boundaries --quiet`, then deliberately add `spotuify-cli` to daemon deps and confirm the test fails after the stale allowance is removed.

### P2 - Root binary remains a large assembly plus business-logic module

- Priority: P2
- Issue: Phase 7 describes `src/main.rs` as thin dispatch, but the file still defines the full clap tree and many operational workflows.
- Evidence:
  - `docs/implementation/10-phase-7-workspace-split.md:16-37` says `src/main.rs` should be `thin dispatch: tui | cli | daemon | mcp`.
  - `docs/implementation/10-phase-7-workspace-split.md:72` marks reducing `src/main.rs` to dispatcher plus legacy shims as complete.
  - `src/main.rs` is 3,809 lines (`wc -l src/main.rs`).
  - `src/main.rs:1-20` still declares legacy modules such as `actions`, `app`, `daemon`, `search`, `spotify`, `store`, `sync`, and `ui`.
  - `src/main.rs:44-911` defines the clap `Cli`, `Command`, and related command enums.
  - `src/main.rs:1437-1470` owns cache reset/repair workflow.
  - `src/main.rs:1506-1776` owns launchd/systemd/Task Scheduler service install/uninstall rendering and execution.
  - `src/main.rs:1844-2013`, `2023-2105`, `2248-2378`, and `2454-2636` own onboarding, logs, analytics/ops handling, and bug-report tar assembly.
- Impact: Command behavior and operational helpers remain hard to reuse/test outside the root binary, and root-level changes can pull backend crates into every build. The docs make the extraction look complete, so the remaining risk is easy to miss.
- Recommended action: Move cohesive command handlers into `spotuify-cli` or `spotuify-daemon` modules by surface area: service install helpers, bug-report assembly, logs, analytics/ops parsing, and remaining cache maintenance. Keep root `main.rs` to parse globals and dispatch to crate-level entrypoints.
- Confidence: High
- Validation idea: Add a lightweight test or lint that fails when `src/main.rs` declares legacy business modules or exceeds a chosen LOC threshold after extraction.

### P2 - Architecture docs disagree on the actual crate map

- Priority: P2
- Issue: The docs present multiple incompatible workspace states: single package target, 14 crates, a non-existent keychain crate, and a real 15-crate workspace that includes `spotuify-launcher`.
- Evidence:
  - `docs/blueprint/01-architecture.md:105-123` still says the current codebase is a single package and shows target crates without `system`, `audio`, `lyrics`, `mcp`, or `launcher`.
  - `ARCHITECTURE.md:7-10` says the current state includes a `keychain` crate.
  - `AGENTS.md:57` lists workspace crates but omits `spotuify-launcher`.
  - `AGENTS.md:234` says the workspace is 14 crates.
  - `Cargo.toml:14` includes root plus `crates/*`; `find crates -mindepth 1 -maxdepth 1 -type d | wc -l` reports 15 crates.
  - `crates/spotuify-launcher/Cargo.toml:1-10` defines the omitted `spotuify-launcher` crate.
- Impact: Agents and contributors get contradictory boundary instructions. That raises the chance of adding code in the wrong crate or accepting dependency drift as intentional.
- Recommended action: Make one architecture doc the source of truth for current crate topology and dependency rules, then update `AGENTS.md`, `ARCHITECTURE.md`, and `docs/blueprint/01-architecture.md` to reference it. Include `spotuify-launcher`; remove `keychain` unless it exists.
- Confidence: High
- Validation idea: Add a docs posture test that compares documented crate names against `crates/*/Cargo.toml` package names, allowing only explicitly marked future/retired crates.

### P3 - Output-format helpers are duplicated across CLI, daemon, and root

- Priority: P3
- Issue: CSV escaping, yes/no formatting, and JSON/JSONL printing are implemented independently in several places even though output formats are a stable product contract.
- Evidence:
  - `AGENTS.md:128` and `AGENTS.md:180-182` describe JSON/JSONL/CSV/IDs as a stable product contract.
  - `crates/spotuify-cli/src/output.rs:1511-1548` defines JSON and CSV helpers.
  - `crates/spotuify-daemon/src/status.rs:6-75` implements daemon status formatting plus local `yes_no` and `csv_value`.
  - `crates/spotuify-daemon/src/diagnostics.rs:227-239` implements doctor report formatting, and `crates/spotuify-daemon/src/diagnostics.rs:808-838` duplicates `yes_no`, `csv_row`, and `csv_value`.
  - `src/main.rs:2443` defines another CSV cell helper.
- Impact: Small escaping/schema differences can break pipeable output without touching protocol types. The duplication also makes it harder to snapshot and review output behavior consistently.
- Recommended action: Move primitive output helpers into a small shared crate/module below CLI and daemon, or expose them from `spotuify-protocol` if the dependency direction stays clean. Keep surface-specific renderers local, but centralize CSV escaping and JSON/JSONL primitives.
- Confidence: Medium
- Validation idea: Add table-driven tests for CSV escaping and JSONL emission in the shared helper, then replace local helper copies and assert existing CLI/daemon output tests still pass.

## Verification

Commands run:

- `git rev-parse --show-toplevel`
- `git branch --show-current`
- `git rev-parse --short HEAD`
- `git status --short`
- `sed -n ... AGENTS.md Cargo.toml ARCHITECTURE.md README.md docs/implementation/10-phase-7-workspace-split.md docs/blueprint/01-architecture.md`
- `find crates -maxdepth 2 -name Cargo.toml -print | sort`
- `rg -n "spotuify-..." crates src Cargo.toml --glob 'Cargo.toml'`
- `nl -ba tests/workspace_boundaries.rs`
- `nl -ba Cargo.toml`
- `nl -ba crates/spotuify-tui/Cargo.toml crates/spotuify-daemon/Cargo.toml crates/spotuify-cli/Cargo.toml`
- `wc -l src/main.rs crates/spotuify-tui/src/app.rs crates/spotuify-tui/src/ui.rs crates/spotuify-cli/src/commands.rs crates/spotuify-daemon/src/lib.rs crates/spotuify-protocol/src/lib.rs`
- `rg -n "use spotuify_..." src/main.rs crates/spotuify-tui/src crates/spotuify-daemon/src`
- `rg -n "OutputFormat|csv_row|csv_value|yes_no" crates/spotuify-cli/src crates/spotuify-daemon/src src`
- `git ls-files | wc -l`
- `find crates -mindepth 1 -maxdepth 1 -type d | wc -l`

Checks intentionally not run before writing: full `cargo test` / `scripts/smoke.sh`; this is a report-only audit with no code changes.

## Residual Risk

This pass focused on obvious quality/architecture issues with file:line evidence. I did not exhaustively audit every handler for runtime bugs, and I did not perform live Spotify or daemon workflow verification because the deliverable was a static report.
