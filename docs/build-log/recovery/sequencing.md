# R1 is published; follow-up repairs remain separate

| Slice | Prerequisite | Owner | Observed effort / wait | Expected wait |
|---|---|---|---|---|
| R1 listening recovery | Release `0.1.110` published and installed; external handoff, earlier-stall investigation and visual gates remain | root integration; isolated workers | Canonical workspace suite: 2,017 passed, zero failed, one ignored across 82 groups; hosted CI 40339891523 and release-tree review passed; refreshed native build 9m55s and smoke exit 0; tag ARM64 build 19m51s; tagged Cargo install 11m58s | Homebrew and production daemon are on 0.1.110. A settled resume failed with flat PCM and native teardown timeout; a fresh process passed 72.182 seconds of advancing playback and fresh PCM. The earlier stall remains unexplained. External handoff awaits user device choice; S4 remains open. |
| R2 playlist outcomes | deployed R1 (`0.1.110`) | root assigns after live/visual closeout | Not started | Queue expansion still loses classified 403 reasons; see [issue 001](../../issues/001-playlist-loading-outcomes.md). No lead time established. |
| R3 confirmed transfer | deployed R1; R2 if its contract is reused | assigned after prerequisite | Not started | Provider cooldown/live-device gate is separate. |
| R4 stored log budget | deployed R1 | assigned after R1 | Research may overlap R2; code not started | No lead time established. |
| R5 compact navigation | deployed R1; R2 if reused; reviewed renderer comps | root UX; assigned worker | Existing captures at 40/60/80/140 columns | Human visual judgement remains open. |

The source records are the [roadmap](../../blueprint/23-recovery-roadmap.md) and [handover](../../implementation/2026-10-26-recovery-handover.md). The final source gates are documented in [the R1 build log](2026-10-26-r1.md). Total agent effort, BK attention and unrecorded wait have not been reconstructed; they remain unknown.
