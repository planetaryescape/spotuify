# 24: Keep the remaining unknowns testable

## Spikes need observed outcomes

| # | Question | Evidence / exact next action | Blocks |
|---|---|---|---|
| S1 | What failed when `e` queued the playlist? | The recorded queue operation returned “playlist is not accessible”; the daemon log at 2026-10-26T10:33 shows HTTP 403. The Spotify adapter classifies some 403 causes, but playlist URI queue expansion still drops that reason; see [issue 001](../issues/001-playlist-loading-outcomes.md). Earlier read-only probes returned 429; one bounded installed-client read now returns a typed 403 with app-permission/development-mode guidance. | Queue error fidelity is deferred to R2; live queue journey remains unverified |
| S2 | Why did the selected device reject transfer? | Original operation `01a177c6-6746-73e0-8ee7-7108c3b92c80` returned HTTP 404 without the attempted target. A later local transfer returned “queued”; an overlapping play returned “confirmed”, but playback was subsequently stopped. The overlap does not establish causality. After the user selects an external device, retry without overlapping playback and inspect the selected target and operation result. | Live transfer diagnosis |
| S3 | Does an external device retain playback after handoff? | R1 source distinguishes deactivation from connection loss and prevents stale playback events from reclaiming recovery ownership. One fresh-process playback window passed for 72.182 seconds: progress advanced 72,113 ms, PCM frame age stayed 1–29 ms, reconnect count stayed zero, and health remained connected after 60 seconds. The earlier process had flat PCM and native teardown timeout; that failure remains unexplained. External handoff awaits a user device choice. | Investigate the prior stall; verify external handoff |
| S4 | Does search feel calm and obvious at BK's normal sizes? | Compare actual renderer captures at 40/60/80/140 columns and shipped light/dark themes; use category navigation and playlist row actions. | Human visual acceptance |
| S5 | Does dedicated local CI run without touching developer state? | Main native job 121608687753 and tag-driven ARM64 release job 121644413570 passed. The release workflow published four-platform assets, checksums and the Homebrew Formula. Signing was skipped because repository signing secrets were absent; stub tests cover signing cleanup. | Resolved for the configured native workflow; signing remains skipped without secrets |

## Assumptions remain visible

| # | Question | Current assumption | Needed before |
|---|---|---|---|
| Q1 | Does BK accept the Listening desk concept and one primary list at wide sizes too? | Blueprint assumes one readable category list, compact now-playing anchor and contextual details. This is Codex's proposal delegated by BK, not a quoted BK decision. | Broader visual redesign; direct defect fixes may proceed |
| Q2 | How should discovered playlists with unreadable contents behave? | Preserve accurate permission error; retain explicit play action with its own result; no automatic first-party fallback or credential escalation. | Any proposed workaround beyond current correction |
| Q3 | May a dedicated spotuify macOS runner be registered and the repository routing variable enabled? | Resolved: BK approved both on 2026-10-26. Dedicated runner 21 is online; native main CI job 121608687753 passed on that runner. | Resolved; no approval blocker |

R1 is published and installed. One fresh-process playback window passed, but the earlier stall remains unexplained; S1 queue error fidelity, S2 transfer and S3 external handoff remain open, as does S4 visual acceptance. Q1 is required before extending the visual redesign beyond the shipped corrections. Q3 is resolved by BK’s explicit instruction to enable the runner and orchestrate the blueprint.
