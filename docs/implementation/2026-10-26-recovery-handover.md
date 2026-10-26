# The repair candidate is under review; the dedicated runner is active

Working tree: `/Users/bhekanik/code/bhekanik/.worktrees/spotuify-recovery-20261026`, branch `fix/listening-reliability-recovery`. Baseline and merge base: freshly fetched `origin/main` at `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e`, rechecked on 2026-10-26. All changes remain uncommitted at this checkpoint. Nothing has been pushed, released or installed. BK has since authorised blueprint orchestration through the ordinary review/release chain. The original checkout retains its pre-existing AGENTS and Cargo runner changes.

## The implemented corrections

Search has one full-width active category. Type tabs, sorting, filtering and mouse selection share the visible-item index. Arrow/page/top/bottom movement now stays in the displayed type; Tab/g/type clicks switch category explicitly. Search row truncation measures terminal cells and preserves grapheme clusters. Playlist Enter plays the highlighted track in its collection.

CLI playlist read/play accepts explicit URI/share links without saved-list membership. Queue Add accepts album/playlist references with collection-read capability gates. CLI `play-uri --context` and MCP `context_uri` expose selected-track context playback. A context requires a track/episode target, an allowed collection and matching provider. Liked Songs uses the existing provider-scoped sentinel.

Connect deactivation is separate from true session loss. Automatic recovery carries ownership and mutation-generation guards through waiting and actor registration. Newer intent cancels stale automatic work. Explicit reconnect remains authoritative. Transfer errors name the selected target and retain their typed cause; restricted devices fail before execution. Generic playlist 403 remains a permission error, with no invented private classification or automatic credential escalation.

Doctor requests daemon-owned provider checks and recomputes health after appended findings. Log tail reads use a bounded 1 MiB snapshot and explicit overflow errors. Native macOS fake smoke, isolated optional local runner routing, queued Cargo and signing cleanup are implemented. The original checkout's Cargo runner safety changes were copied verbatim for verification.

## Completed verification has a clear boundary

- `scripts/cargo-test --workspace --locked`: exit 0, 2,014 passed, zero failed, one existing ignored architecture test, including doctests. Log `../build-log/recovery/evidence/spotuify-recovery-workspace-tests-final.log`.
- That full pass preceded the final category-navigation correction, CLI nonleaf-context guard/new integration test, scoped compiler-lint allowances, and a correction to the no-active-device command hint. Those final changes still need the focused rerun below. Do not describe the complete final tree as fully tested.
- Expanded real CLI/fake-daemon journey: search/play, direct playlist URI and share-link equivalence, selected second-track contextual play, Next following the collection, confirmed playlist queue append, device transfer, daemon-backed doctor with six API checks, consistent health classification, and daemon stop all passed. Evidence `../build-log/recovery/evidence/spotuify-recovery-fake-journey.json`; reproducer `../build-log/recovery/evidence/fake-journey.py`. A permanent regression now lives in `tests/cli_fake_daemon.rs` and is included in the pending focused run.
- `SPOTUIFY_BIN=./target-cli/debug/spotuify scripts/smoke.sh`: exit 0, log `../build-log/recovery/evidence/spotuify-recovery-smoke.log`.
- Shell runner/CI wrapper/release-scope/signing-cleanup suites, actionlint and shellcheck passed. Signing tests use dummy credentials and stubs, not the actual keychain.
- Changed-file rustfmt, whitespace checks, generated CLI reference and docs validation pass. Blueprint validator: 28 documents, 54 decisions, 8 defined spikes/questions, zero errors/warnings. The extension adds 13 decisions. Layer A checked 24 changed Markdown files and changed none; fenced code, frontmatter and inline code were protected. Evidence `../build-log/recovery/evidence/spotuify-recovery-prose-clean.json`.
- Actual renderer text captures at 40x24, 60x24, 80x24, 140x32 are available under `../build-log/recovery/evidence/spotuify-search-<size>.txt`. Refresh from the final compiled test after the pending rerun. The companion is `../build-log/recovery/evidence/companion.html`; it is open in Dia. Human theme/font/input judgement remains S4.

## The machine gate prevented the final compile

During 2026-10-26T11:22-11:27 UTC, macOS reported `kern.memorystatus_vm_pressure_level:2` and `memory_pressure -Q` reported 51-52% free. `agent-work` requires pressure level 1 plus at least 25% available memory. The native release build waited without starting; its own waiting job was cancelled to release the queue slot. Other jobs and global memory policy were left untouched. Log `../build-log/recovery/evidence/spotuify-recovery-release-build.log` records the wait. This is an uncompleted build gate, not a compiler failure.

The first workspace Clippy run on Rust 1.99.1 stopped on async-trait-generated `double_must_use` errors in unchanged traits. The locked macro injects must_use; upstream PR 17833 addresses the regression. Scoped allowances now cover only eight trait definitions. One test-only deprecated atomic method retains its Rust 1.88-compatible spelling with a method-level allowance. No lint rerun after those edits has completed. Evidence `../build-log/recovery/evidence/spotuify-recovery-clippy.log` and [the audit](../research/2026-10-26-reliability-audit.md).

## Resume with these commands when pressure returns to normal

Run in the recovery worktree. Cargo uses the shared queue; do not start overlapping Cargo commands against this target directory.

```sh
sysctl kern.memorystatus_vm_pressure_level
memory_pressure -Q
CARGO_BUILD_JOBS=4 scripts/cargo-test -p spotuify-cli -p spotuify-tui -p spotuify-daemon -p spotuify --locked
CARGO_TARGET_DIR=target-cli CARGO_BUILD_JOBS=4 cargo clippy --workspace --all-targets -- -D warnings
CARGO_TARGET_DIR=target-cli CARGO_BUILD_JOBS=4 cargo build --locked --release --bin spotuify --no-default-features --features 'embedded-playback system-integrations loopback-cpal portaudio-backend'
SPOTUIFY_BIN=./target-cli/release/spotuify scripts/smoke.sh
python3 -I /Users/bhekanik/.dotfiles/.skills/blueprint/scripts/check_blueprint.py docs/blueprint
```

Inspect each command's result before claiming its gate passed. Update help snapshots through insta and regenerate from `site/scripts/generate-cli-reference.mjs`, rather than editing generated pages by hand. If a remaining Rust 1.99 lint is genuine, fix it; do not broaden the generated-code allowance.

## Live and external gates remain separate

The installed Homebrew binary remains 0.1.109; it is not the corrected debug binary. Installed logs and operations establish the original playlist 403, transfer 404 and reconnect sequence. Two later read-only Spotify probes returned 429, so live probing stopped. The exact access/transfer causes and sustained physical-device handoff/PCM remain S1-S3. Do not reset cooldowns or swap credentials to manufacture success.

Q1 remains the proposed Listening desk concept, including a single list at wide sizes. Q2 assumes unreadable playlist contents produce an accurate permission failure and a separate explicit play action. Q3 is resolved: BK approved the dedicated runner and routing variable. Runner21, `bk-m3max-spotuify`, is online and `SPOTUIFY_MACOS_ARM64_RUNNER` is set to its four labels. The corrected action explicitly selects sccache with isolated cache state. [Activation evidence](../build-log/recovery/evidence/runner-setup/activation.md) records service/tool checks; actual native workflow execution remains S5. Pull requests retain hosted macOS routing.

Four open follow-ups: [playlist loading outcomes](../issues/001-playlist-loading-outcomes.md), [transfer readiness](../issues/002-transfer-readiness.md), [log storage budget](../issues/003-log-storage-budget.md), [compact-player/search decisions](../issues/004-search-state-and-compact-player.md). [CLI/MCP context parity](../issues/005-contextual-play-cli-gap.md) is implemented with final regression verification pending. Keep structural reorganisation separate from behaviour repairs.

[Blueprint index](../blueprint/README.md), [questions](../blueprint/24-recovery-questions.md), [roadmap](../blueprint/23-recovery-roadmap.md), [runtime evidence](../research/2026-10-26-reliability-audit.md), [mxr reuse](../research/2026-10-26-mxr-reuse.md).

## The review snapshot is preserved outside the repository

`/tmp/spotuify-recovery/changes.patch` and `changes.tar.gz` contain the tracked diff and new files against the baseline. `/tmp/spotuify-recovery/source-manifest.json` records source-file hashes. Use the existing worktree for review; the original checkout already has some runner edits included in this baseline diff. No commit was created.


## Orchestration has a durable owner and a release sequence

The current ladder in [23](../blueprint/23-recovery-roadmap.md) supersedes the initial three overlapping repair rungs. R1 ships the existing complete repair patch; R2 adds terminal playlist-loading outcomes, R3 confirms transfer readiness/destination, R4 bounds stored log bytes and R5 completes compact navigation. Do not implement a dependent rung before its prerequisite deploys.

The control ledger is `/Users/bhekanik/code/bhekanik/.orchestrate/spotuify-recovery-20261026/ledger.md`. Source, runner and review worktrees are registered there. The original dirty checkout is preserved. Cleanup approval remains pending; retain all generated resources until authority is recorded.

At11:48 UTC, targeted final tests entered shared queue task314 with four workers. Memory pressure had returned to1; the queue was occupied by other projects. Evidence is `docs/build-log/recovery/evidence/final-targeted-tests.log`. Do not start another Cargo command on this target while314 is active. After it completes, run the pending Clippy/native-release/smoke commands above and inspect each outcome.

The Claude subscription capability probe failed on its weekly quota, and the already configured Anthropic API route failed with insufficient credit. No billing/account setting was changed. A separate worker is checking the existing Gemini OAuth CLI for a constrained independent review route. Full-tier R1 cannot merge without the required different-provider review or BK’s explicit stage waiver.

Exact next action: inspect task314 and the source review, apply only evidenced in-scope corrections, commit the exact candidate, then run root and independent whole-diff review against that SHA/tree. Continue through PR CI, release-please, Homebrew and installed smoke once all required gates pass. The installed binary is still0.1.109 until that chain completes.

The source review found and corrected a cancellation race before the final candidate: deactivation now increments the mutation generation before awaiting session tracking. The registration-race regression holds the tracker mutex and proves that stale playing-clock state cannot authorise resume. The test-only lock helper follows the existing session-tracker test access pattern.
