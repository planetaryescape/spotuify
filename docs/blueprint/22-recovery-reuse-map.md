# 22: Copy the mxr fixes that address an observed failure

Apply D-045 and D-050. Source commits reviewed on 2026-10-26:

- spotuify `origin/main`: `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e`.
- mxr `origin/main`: `3f111fb79440068663286de804909e918467ab48`.

Read the source at these commits before copying. Re-fetch before implementation resumes. The review window was 2026-08-26 through2026-10-26; the detailed commit/path inventory is in [the mxr research](../research/2026-10-26-mxr-reuse.md).

## Reuse addresses specific contracts

| Candidate | Disposition | Reason |
|---|---|---|
| Isolated self-hosted Rust homes | Adopt | A persistent runner must not mutate developer toolchain/cache state |
| Queued native builds | Adopt | Local CI must share the machine's existing build queue |
| Native fake smoke | Adapt | Exercise spotuify's CLI/daemon/provider path on macOS |
| Keybinding/help consistency | Adapt existing spotuify registry | Fix observed queue hints and visible selection mismatch without copying mail actions |
| Mutation receipts and readiness | Preserve existing spotuify contract | A new mail-specific queue abstraction does not fix the observed music selection defect |
| Bounded tail reads | Adopt | Inspect at most 1 MiB plus a boundary byte; reject requests exceeding that budget explicitly |
| Byte-bounded log storage | Defer to tracked issue | Seven daily files do not establish a total disk budget |
| Cache publication fencing | Defer pending a reproduced stale-publication case | Avoid replacing search/cache coordination based only on similarity |
| Gmail rate-limit gate | Reject direct transplant | Spotify already has persistent scoped Retry-After handling; provider semantics differ |
| Wholesale TUI layout or mail navigation | Reject | Music selection and playback ownership need their own interaction design |

The isolated-home action is adapted from mxr, with its provenance in a source comment. Do not introduce a shared crate merely for one copied shell action.

## Current strengths need no replacement

Spotuify already has a daemon-owned playback clock, typed provider boundary, operation history, fake provider, scoped backoff and CLI mutation finalisation. Keep those mechanisms and add regression cases at their failed boundaries. A recent upstream commit is a lead, not proof that spotuify needs the same change.

## Verify the reusable mechanism

For local runner isolation, inspect environment paths in a dry test and assert that developer Cargo/rustup state is untouched. For queue wrappers, stub `agent-work` to check argument preservation and failure propagation. For runtime changes, test deactivation separately from true session loss. The acceptance check follows the copied mechanism, not the source project's test count.

## Pinned source paths make reuse reproducible

All mxr paths below are read at `origin/main` = `3f111fb79440068663286de804909e918467ab48`:

| Path | Lines | Use |
|---|---:|---|
| `.github/actions/self-hosted-rust-home/action.yml` | 26 | Copied/adapted isolation, with task-specific name, cache directory/port and bounded download |
| `.github/workflows/release.yml` | 439 | Runner matrix and setup ordering reference |
| `.github/workflows/ci.yml` | 676 | Trusted local jobs, bounded native subset reference |
| `scripts/cargo-test` | 42 | No unrelated process reaping; worker defaults |
| `scripts/ci_change_scope.sh` | 141 | Scope test precedent; don't copy entire mail matrix |
| `crates/keymap/src/lib.rs` | 688 | Declarative registry and generated contextual key labels |
| `crates/keymap/keymap.toml` | 4,456 | Read as scope example, do not copy mail bindings |
| `crates/tui/src/ipc.rs` | 640 | Worker request timeout, safe read retry and reconnect state |
| `crates/tui/src/search_ipc.rs` | 191 | One search page per user request, errors propagated |
| `crates/tui/src/daemon_events.rs` | 1,009 | Operation outcomes, actionable failures, quiet background health |
| `crates/tui/src/app/state/toasts.rs` | 230 | Bounded transient notification queue |
| `crates/tui/src/ui/error_modal.rs` | 76 | Inspectable long error detail and dismissal hints |
| `crates/daemon/src/logging.rs` | 401 | Byte budget and cross-process rotation |
| `crates/daemon/src/commands/logs.rs` | 479 | Bounded readers and explicit log management |
| `crates/client/src/lib.rs` | 1,897 | Connection identity/compatibility reference; extract ideas rather than copy entire module |
| `crates/daemon/src/sync_history.rs` | 338 | Optional recurring error aggregation |
| `crates/daemon/src/handler/mode_cache.rs` | 420 | Cache invalidation/fencing reference, deferred until measured |
| `crates/provider-gmail/src/rate_limit_gate.rs` | 120 | Server-imposed cooldown principle only |

