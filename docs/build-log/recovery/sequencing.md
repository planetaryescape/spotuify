# R1 must deploy before dependent repairs

| Slice | Prerequisite | Owner | Observed effort / wait | Expected wait |
|---|---|---|---|---|
| R1 listening recovery | release 0.1.110, native Actions, live device and visual gates | root integration; isolated workers | Activation suite 2,017 passed; Clippy 19.42s; native release build 13m02s; independent review 516.93s. Lockfile suite 2,017 passed; native refresh 8m05s and both smokes pass; final hosted CI passes. | No deployment date established. PR 154 integrated; complete release PR 155 and close live S1–S5 gates. |
| R2 playlist outcomes | deployed R1 | assigned after R1 | Not started | No lead time established |
| R3 confirmed transfer | deployed R1; R2 if its contract is reused | assigned after prerequisite | Not started | Provider cooldown/live-device gate is separate |
| R4 stored log budget | deployed R1 | assigned after R1 | Research may overlap R2; code not started | No lead time established |
| R5 compact navigation | deployed R1; R2 if reused; reviewed renderer comps | root UX; assigned worker | Existing captures at 40/60/80/140 columns | Human visual judgement remains open |

The source records are the [roadmap](../../blueprint/23-recovery-roadmap.md) and [handover](../../implementation/2026-10-26-recovery-handover.md). The final source gates are documented in [the R1 build log](2026-10-26-r1.md). Total agent effort, BK attention and unrecorded wait have not been reconstructed; they remain unknown.
