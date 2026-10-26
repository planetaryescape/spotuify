# 23: Each release must finish a listening journey

BK invoked orchestrate on 2026-10-26. Every rung goes through review, release-please, GitHub Releases and the Homebrew install before dependent implementation starts. The initial repair patch spans search, playlist actions, playback ownership and native CI; those changes form one usable recovery release. Splitting them into component milestones would leave the first client dependent on unreleased work.

The investigation baseline is `origin/main` at `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e`. Re-fetch before implementation and record the exact candidate and merged trees. The [handover](../implementation/2026-10-26-recovery-handover.md) separates completed checks from remaining gates.

## R1 restores the basic listening flow

Promise: “I can find and play the intended song, queue accessible playlists and keep another device in control.”

Include [18](18-listening-desk.md), [19](19-playback-and-provider-corrections.md), [20](20-search-and-action-corrections.md) and [21](21-native-ci-corrections.md). Search gives the active category the terminal width. Visible selection drives actions. Explicit playlist references and selected-track context work through CLI and MCP. Connect deactivation revokes local recovery intent. Provider errors and doctor findings retain their meaning. Bounded log-tail reads keep diagnostics usable.

Demo: search in a small terminal, switch category, sort and filter, then play the visible selection. Read an accessible playlist by URI without saving it, play its second track in context, and check Next. Queue the playlist and inspect its terminal operation. Transfer once to a named device and observe ownership and progress beyond the watchdog.

Acceptance: targeted regressions, canonical workspace tests including doctests, Clippy, native release build, fake CLI journey and installed release smoke. The native CI lane must run with isolated Rust/cache state. Existing fake fixtures establish routing and cancellation; they cannot establish sustained physical-device audio. S1-S3 retain that live boundary. Actual renderer captures establish compact-width behaviour; S4 retains human visual judgement.

Deploy target: GitHub Release and the canonical Homebrew install. Full verification tier because the patch touches concurrency and public CLI/MCP contracts. Root owns integration, blueprint, research, references and release documentation. Workers own isolated source changes and return evidence. Independent review must inspect the complete exact candidate.

Unknowns: playlist-content access and live transfer/handoff behaviour under the current account. Later read probes returned429, so do not loop or reset cooldowns. Runner approval Q3 is resolved; S5 requires actual native workflow execution.

Left out: remapping `e`, automatic credential escalation, new navigation shell, artwork redesign, audio backend replacement and structural file moves.

## R2 makes playlist loading finish visibly

Previously, R1 restores playlist actions and error classification. Now, a background playlist load must finish with tracks or an explicit terminal failure.

Promise: “Opening a playlist tells me whether it is still loading, empty or unavailable.”

Implement [issue001](../issues/001-playlist-loading-outcomes.md) after R1 deploys. Reproduce delayed success and provider failure with the fake provider first. Keep reusable result state in the daemon and expose the same terminal outcome through CLI, TUI and MCP; document any macOS-client compatibility consequence before changing the protocol.

Demo: open a delayed playlist, observe loading, then tracks. Open a denied playlist, observe a failure with a useful next action. Retry and verify an older result cannot replace the new request.

Acceptance: real CLI/fake-daemon success, empty, error and retry journeys; TUI state rendering; existing clients remain compatible. Full tier for protocol/shared state. Deploy through the same GitHub/Homebrew chain. Root owns contract and operational docs; the worker updates the affected command/UI references.

Left out: undocumented provider access workarounds and credential changes.

## R3 confirms the destination before reporting transfer success

Previously, R1 prevents automatic local recovery from reclaiming another device. Now, transfer waits for readiness and confirms the resulting destination.

Promise: “A device switch either reaches the device I chose or explains what prevented it.”

Implement [issue002](../issues/002-transfer-readiness.md) after R1; also depend on deployed R2 if its result-state contract is reused. Replace the fixed readiness delay only after reproducing delayed registration/readback with controlled fixtures. Keep newer user intent authoritative throughout waits.

Demo: select a slow device and observe eventual confirmed playback; select an unavailable device and receive a bounded failure; choose another target during the wait and verify the old transfer cannot win. Run one sustained live transfer when provider cooldown permits.

Acceptance: fake delayed-readiness, stale-readback, timeout and cancellation journeys plus live target/progress evidence. Full tier for concurrency. Deploy through GitHub/Homebrew. Root owns ownership invariants and the worker updates device-command guidance.

Left out: arbitrary longer sleeps, stale-track healing and automatic retries of denied mutations.

## R4 bounds storage while keeping diagnostics readable

Previously, R1 bounds log-tail memory. Now, the daemon’s stored logs have a byte/file budget.

Promise: “Leaving the daemon running cannot consume unbounded log storage.”

Implement [issue003](../issues/003-log-storage-budget.md) after R1. Read-only mxr research may run beside R2, but retention code must not race integration in the same files. Adopt the verified mxr rotation path where it fits. Define the budget, active-file handling and reader behaviour before implementation.

Demo: drive a daemon past the configured budget, inspect retained files and total bytes, then read recent logs and restart. Verify concurrent readers and writers survive rotation.

Acceptance: bounded on-disk bytes/files, preserved recent diagnostic output, restart and concurrent read/write tests. Full tier for persistence/retention. Deploy through GitHub/Homebrew. Root owns retention policy and operations documentation.

Left out: deleting current live logs during investigation and unrelated diagnostics features.

## R5 makes compact navigation feel coherent

Previously, R1 gives search one readable list. Now, the player anchor, loading/error states and contextual Open actions follow one consistent hierarchy.

Promise: “In a small terminal I can tell where I am, what is selected and what the next action will do.”

Implement [issue004](../issues/004-search-state-and-compact-player.md) after R1, and after R2 if its loading states are reused. The working concept remains Listening desk: one primary list, quiet now-playing anchor and details on demand. Q1 still distinguishes that proposal from BK’s explicit visual preference. Produce comps from the actual renderer before broader changes.

Demo: navigate search, a playlist and playback at40/60/80/140 columns, with loading, empty and denied states, using the shipped light/dark themes. Keep CLI and contextual actions aligned.

Acceptance: navigation/render regressions, actual renderer captures, human aesthetic judgement S4, installed client journey. At least standard tier; raise to full if protocol or shared state changes. Deploy through GitHub/Homebrew. Root owns the UX concept and screenshots; workers update help and command references.

Left out: speculative new features, wholesale file restructuring and visual changes without a user problem.

## Unresolved questions

- Q1: Does the Listening desk hierarchy feel right in BK’s actual terminal?
- S1-S3: What does the bounded live playlist/transfer/handoff check show after cooldown?
- S5: Does the exact candidate pass native CI on the activated runner?
