# The recovery source passes tests and Clippy; release gates remain open

Worktree: `/Users/bhekanik/code/bhekanik/.worktrees/spotuify-recovery-20261026`, branch `fix/listening-reliability-recovery`. Fresh investigation base and merge base: `origin/main` at `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e`. Repair source is committed at `ca361dde8cb4a7ab00eb6b52c1196991ec580673`; `d9158ba1f387d34d309cf6f1bb2c1554b4658a19` adds documentation corrections. The pending site dependency patch is separate from those Rust checks. Nothing has been pushed, released or installed. The original checkout retains its pre-existing AGENTS and Cargo runner edits.

BK authorised the dedicated runner and invoked orchestrate on 2026-10-26. Follow the [release ladder](../blueprint/23-recovery-roadmap.md): ship R1 before implementing dependent repairs. The durable control ledger is `/Users/bhekanik/code/bhekanik/.orchestrate/spotuify-recovery-20261026/ledger.md`; it records exact candidate trees, ownership, reviews, resource budgets and queue IDs.

## The candidate repairs selection and playback ownership

Search has one full-width active category. Sorting, filtering, mouse targets and list movement share the visible indices; Tab/g/type clicks switch category. Truncation measures terminal cells and preserves grapheme clusters. Enter inside a playlist plays the highlighted track in context. `e` remains queue with a contextual hint.

CLI playlist read/play accepts explicit URI/share links without saved-list membership. Queue Add accepts album/playlist references with collection-read capability gates. CLI `play-uri --context` and MCP `context_uri` expose selected-track context playback. A context requires a track/episode target, an allowed collection and matching provider. Liked Songs retains the existing provider-scoped sentinel.

Connect deactivation is distinct from true connection loss. Automatic recovery checks ownership and mutation generation through waiting, teardown and registration. Deactivation increments the generation before awaiting session tracking. The strengthened regression holds the tracker mutex while the clock still appears to be playing, then proves the actor cannot resume. Foreign playback snapshots remain intact. Explicit reconnect retains user authority.

Transfer errors name the selected target and preserve their typed cause; restricted devices fail before execution. Generic playlist 403 remains a permission error without invented private classification or automatic credential escalation. Doctor requests daemon-owned provider checks and recomputes its aggregate health after appended findings. Log tail reads inspect at most 1 MiB plus a boundary byte and explicitly report overflow. Stored log retention remains a later repair.

## Current proof covers the real CLI and daemon path

- Canonical `scripts/cargo-test --workspace --locked`: exit 0, **2,016 passed, zero failed**, one existing ignored architecture test, including doctests. Compilation took 4m 58s. Evidence: [workspace log](../build-log/recovery/evidence/final-workspace-tests.log).
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0; 5m 08s. Evidence: [Clippy log](../build-log/recovery/evidence/final-clippy.log). The narrow async-trait compatibility allowances now pass; no blanket lint suppression was added.
- `tests/cli_fake_daemon.rs` passed all 14 tests, including explicit URI/share equivalence, second-track context playback, Next following the collection, confirmed playlist queue, fake-device transfer, six daemon provider checks, consistent health and daemon stop. This drives the actual binary and IPC rather than calling internal routing helpers.
- `SPOTUIFY_BIN=./target-cli/debug/spotuify scripts/smoke.sh`: exit 0 with a retained short runtime path. Evidence: [debug smoke](../build-log/recovery/evidence/final-debug-smoke-short-path.log). The first durable-ledger socket path exceeded macOS SUN_LEN; [its failed attempt](../build-log/recovery/evidence/final-debug-smoke.log) is retained. This was corrected in the harness path.
- Exact-source fmt, shell runner/CI wrapper/release-scope/signing cleanup tests, actionlint, changed-line anti-slop, generated CLI reference validation and whitespace checks pass. Signing uses stubs, without touching the real keychain.
- Blueprint validator: 28 documents, 54 decisions, 8 defined spikes/questions, zero errors/warnings. Layer A checked 29 changed Markdown files and changed none, protecting code/frontmatter. The final secret scan found no leaks across the three candidate commits.
- Actual renderer captures at 40/60/80/140 columns are in [evidence](../build-log/recovery/evidence/). Their right-margin padding is trimmed. Renderer code matches the candidate; the subsequent navigation regressions now pass. The [interactive companion](../build-log/recovery/evidence/companion.html) explains layout and event ownership. Human theme/font/input judgement remains S4.

The earlier 2,014-test pass predates the final navigation and CLI guards and is historical. The first final targeted run stopped on three hand-built URI strings in new test fixtures; the canonical constructor correction is included in the 2,016-test pass. Neither failed attempt is hidden by the later result.

## The dedicated runner is enabled; native execution still needs proof

GitHub runner 21, `bk-m3max-spotuify`, is online. Its installation is `/Users/bhekanik/.actions-runner/spotuify`, with launch agent `actions.runner.planetaryescape-spotuify.bk-m3max-spotuify`. Repository variable `SPOTUIFY_MACOS_ARM64_RUNNER` contains `["self-hosted","macOS","ARM64","spotuify-builder"]`. Q3 is resolved. [Activation evidence](../build-log/recovery/evidence/runner-setup/activation.md) records the digest, service/tool checks and owner-only credential permissions.

The candidate action explicitly enables sccache and isolates Cargo, rustup, cache directory and server port. Pull requests remain hosted; trusted main and ARM64 release jobs can use the dedicated runner once the workflow patch lands. Setup readiness is not a successful native workflow; S5 remains open.

Native PortAudio release build is queue task339, with evidence at `docs/build-log/recovery/evidence/final-native-release-build-capped.log`. Task335 was cancelled after discovering that the shared wrapper overwrites outer worker caps with five. The replacement sets four inside the queued command; the CI helper and its existing regression now exercise that override boundary. After it completes, run:

```sh
SPOTUIFY_BIN=./target-cli/release/spotuify \
  SPOTUIFY_SMOKE_DIR=/Users/bhekanik/code/.orchestrate/spotuify-r1-native-smoke-20261026 \
  scripts/smoke.sh
```

Register that new runtime path in the ledger before creating it. Keep Cargo commands sequential on `target-cli`, cap workers at four and use the shared queue. Never bypass its memory gate or cancel another run’s work.

## The site audit patch removes the existing CI blocker

The latest recorded CI failure was Site Audit & Build in run 39856945130. A fresh base audit found 12 vulnerabilities; the proposed compatible patch reports zero. It pins Astro 7.3.3 and updates affected transitive lock entries, including sharp 0.35.5 and selector-parser 7.1.6. [Audit evidence](../build-log/recovery/evidence/site-audit/review.md) contains before/after output, upstream source comparison and integrity checks.

The managed npm policy permits a reviewed urgent security fix to bypass release age for one command. Only `@expressive-code/core@0.44.3` needed that exception; its implementation is unchanged, it has no install lifecycle scripts, and its dependency bump selects the fixed parser. The name-scoped exception leaves other dependencies quarantined and does not change npm configuration. Standard install refused unapproved lifecycle scripts; `npm ci --ignore-scripts` succeeded. Worker site build passed as task337:179 pages built and indexed, with native worker pools capped at four. The first build334 was cancelled to apply those caps. The dependency worker used baseline documentation inputs; hosted CI will validate the exact final generated content. Root independently audited the integration lock and confirmed zero vulnerabilities.

## Independent review and live verification remain separate gates

Claude’s configured subscription failed on weekly quota; its configured API credential failed on insufficient credit. Gemini’s configured OAuth rejected the installed client. No billing or account configuration changed. The existing Pi/Ollama DeepSeek Pro route returned READY with verified response-model metadata, but its configuration reports high and drops the effort parameter. A native no-history max request also returned READY without exposing effective effort/model metadata. Maximum reasoning remains unconfirmed.

BK was asked whether to use that available different-provider route for R1 or wait for Claude. The question is pending. The invoked orchestrate skill requires independent review for this concurrency/public-API stage; do not infer a waiver or substitute another OpenAI model. Root source review is green, but independent review has not run.

Installed Homebrew remains 0.1.109. Source tests and fake journeys cannot establish physical-device audio. Recorded live failures were playlist 403 and transfer 404; later read probes returned 429, so probing stopped. S1-S3 still require one bounded account/device replay after cooldown, including target/progress and PCM beyond the watchdog. Do not reset backoff or switch credentials to manufacture success.

## Resume from the candidate rather than repeating the investigation

Inspect task339, finish native smoke, commit the reviewed site patch and update the exact clean review snapshot and whole-diff brief. Resolve the pending independent-model choice and run the full required review. Then fetch the base again, finish pre-push checks, create the PR and run the complete release-please/Homebrew/install-smoke chain. Record merged-tree equivalence and native Actions evidence. Do not implement R2 until R1 deploys.

Next rungs: [playlist loading outcomes](../issues/001-playlist-loading-outcomes.md), [confirmed transfer](../issues/002-transfer-readiness.md), [stored log budget](../issues/003-log-storage-budget.md), [compact navigation](../issues/004-search-state-and-compact-player.md). [CLI/MCP context parity](../issues/005-contextual-play-cli-gap.md) is implemented and its final integration regression passes.

All run-created worktrees, build output and failed-attempt evidence are retained while cleanup approval is pending. Preserve the original dirty checkout and the permanent runner service.
