# R1 must deploy before dependent repairs

| Slice | Prerequisite | Owner | Observed effort / wait | Expected wait |
|---|---|---|---|---|
| R1 listening recovery | final tests, source review, independent review, native CI | root integration; isolated workers | Initial final-build attempt waited11:22-11:27 UTC at VM pressure2 without compiling. Targeted tests queued11:48 UTC. | Measure queue, CI and release separately; no estimate replaces a gate. |
| R2 playlist outcomes | deployed R1 | assigned after R1 | Not started | No lead time established |
| R3 confirmed transfer | deployed R1; R2 if its contract is reused | assigned after prerequisite | Not started | Provider cooldown/live-device gate is separate |
| R4 stored log budget | deployed R1 | assigned after R1 | Research may overlap R2; code not started | No lead time established |
| R5 compact navigation | deployed R1; R2 if reused; reviewed renderer comps | root UX; assigned worker | Existing captures40/60/80/140 columns | Human visual judgement remains open |

The source records are the [roadmap](../../blueprint/23-recovery-roadmap.md) and [handover](../../implementation/2026-10-26-recovery-handover.md). Transcript timestamps establish only the observed windows above. Total agent effort and BK attention have not been reconstructed; they are unmeasured, not zero.
