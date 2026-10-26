# 24: Keep the remaining unknowns testable

## Spikes need observed outcomes

| # | Question | Evidence / exact next action | Blocks |
|---|---|---|---|
| S1 | What failed when `e` queued the playlist? | Recorded queue operation failed “playlist is not accessible”; daemon log2026-10-26T10:33 shows items403. Later read-only probes returned 429. Test permission propagation with fixtures; no more live retries now. | Provider error repair verified by fixtures; current live recheck remains open |
| S2 | Why did the selected device reject transfer? | Operation `01a177c6-6746-73e0-8ee7-7108c3b92c80` returned plain PUT/me/player404. The stored pre-state identifies the prior device, not the attempted target. Run one target-named transfer after cooldown and inspect its operation. | Live transfer diagnosis |
| S3 | Does an external device retain playback after handoff? | Source confirms deactivation was mistaken for session loss. Run corrected binary, hand off once, check target/progress and no local reconnect/resume beyond watchdog. | R1 live completion |
| S4 | Does search feel calm and obvious at BK's normal sizes? | Compare actual renderer captures at 40/60/80/140 columns and shipped light/dark themes; use category navigation and playlist row actions. | Human visual acceptance |
| S5 | Does dedicated local CI run without touching developer state? | Runner 21 is online following BK’s approval. The approved JSON routing labels are set. Run native fake smoke; verify toolchain/cache/keychain state after pass and failure. | R1 native completion |

## Assumptions remain visible

| # | Question | Current assumption | Needed before |
|---|---|---|---|
| Q1 | Does BK accept the Listening desk concept and one primary list at wide sizes too? | Blueprint assumes one readable category list, compact now-playing anchor and contextual details. This is Codex's proposal delegated by BK, not a quoted BK decision. | Broader visual redesign; direct defect fixes may proceed |
| Q2 | How should discovered playlists with unreadable contents behave? | Preserve accurate permission error; retain explicit play action with its own result; no automatic first-party fallback or credential escalation. | Any proposed workaround beyond current correction |
| Q3 | May a dedicated spotuify macOS runner be registered and the repository routing variable enabled? | Resolved: BK approved both on 2026-10-26. Dedicated runner 21 is online; native workflow proof remains S5. | Resolved; no approval blocker |

There are no product questions blocking the direct code corrections in rung 1. Q1 is required before extending the visual redesign beyond those corrections. Q3 is resolved by BK’s explicit instruction to enable the runner and orchestrate the blueprint.
