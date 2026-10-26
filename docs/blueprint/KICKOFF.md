# Resume the listening repairs

Read `AGENTS.md`, then [17](17-reliability-recovery.md) through [24](24-recovery-questions.md), the appended decisions in [13](13-decision-log.md), and [the handover](../implementation/2026-10-26-recovery-handover.md). Original chapters describe earlier intent; the correction evidence is dated 2026-10-26.

1. Fetch the default branch and name its SHA. The investigation baseline is `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e`. Preserve existing uncommitted work; use an isolated worktree if the checkout is dirty.
2. Inspect the final diff and handover before rebuilding or retrying live operations. Use the existing fake provider for repeatable tests. Do not repeatedly call Spotify while the account is rate-limited.
3. Read [18](18-listening-desk.md) before changing a screen. The concept is proposed, not an invented BK preference. Broader redesign needs BK's Q1 answer and a reviewed comp.
4. Finish the next incomplete journey in [23](23-recovery-roadmap.md). Check real CLI behaviour and actual renderer output. A passing unit suite is required but cannot prove physical device playback.
5. Run S2/S3 once when provider cooldown permits. Retain the target, receipt and observations beyond the watchdog. Do not reset cooldowns or switch credentials to manufacture success.
6. BK approved the dedicated local CI runner and routing variable. Runner21 is online; finish S5 against the exact release candidate. Use mxr source at the pinned SHA in [22](22-recovery-reuse-map.md); re-fetch before adopting newer changes.
7. BK invoked orchestrate on 2026-10-26. Run the complete review, release-please and Homebrew flow for each rung, following `AGENTS.md`. Do not implement a dependent rung before its prerequisite is deployed.

Unresolved: Q1's broader visual direction, S2/S3 live transfer/handoff verification and S5’s native workflow proof.
