# Playlist queue progression fails after an audio reconnect

BK requested a fix after the 0.1.110 release could not establish natural playlist progression. The installed binary reproduced the failure: a three-track playlist started, an audio output change triggered reconnect, and the first track ended with an empty queue. The private runtime capture and [curated evidence](evidence/playlist-queue-before.json) agree. Source is release `6c90d79c`; the freshly fetched `origin/main` base `ed0a3b483e853be5e2fc14a0022c7cc4a5076425` has identical application code.

The reconnect actor calls the backend's ordinary shutdown and then `play_uri` with only the current URI and position. The embedded adapter converts that to a one-track Spirc load. Pinned librespot `f76335856e978d1cd6dd1352f2fdecd3a2a224f5` already returns `SavedPlaybackState` when its session becomes invalid; the app aborts the task and discards it. The correction uses that state for reconnect and keeps the existing native teardown safety boundary.

The idle collection path already resolves all playlist tracks. It now supplies that full ordered list to the embedded player and publishes the correct current head and remainder. This avoids depending on pending context-resolution pages during queue recovery. The native queue is retained directly; it is not rebuilt from a capped Web API queue snapshot.

The local native candidate `53c773afe1cdbbd739cb683de2d67665e9fb9e6e` passed that live journey: queued playlist → reconnect → natural end of first track → second track with remainder → natural end → third track. It also passed active-session append, another reconnect while playing a manually queued track, and paused reconnect followed by resume. Fresh sink PCM was observed on each new track. These were actual end-of-track transitions after seeking near each ending, without manual Next. [Curated evidence](evidence/playlist-queue-after.json).

The native build passed in 5m43s. Two bounded state-capture lifecycle tests passed. The new handler test initially raced the asynchronous queue operation; a test-only correction waits for its QueueChanged event before inspecting recorded calls. The corrected focused test passed. The canonical workspace suite passed 2,020 tests, zero failed and 1 existing ignored test across 82 groups, including doctests, in 353 seconds. Hosted CI passed every executed check on `ad6f7c59`. The independent whole-diff review passed at maximum reasoning effort; [report and triage](evidence/queue-independent-review/triage.md). The native fake-provider smoke passed. Publication and installed-release verification remain pending. This is a repair within R1, ahead of dependent R2–R5 work.

## Unresolved questions

- Does the published Homebrew release pass the same end-of-track journey after installation?
- Paused reconnect passed; existing cancellation regressions must remain green. The previously recorded late foreign-cluster/queued-activation race remains R3 work.

## Measured checks

| Check | Observed time | Result |
|---|---|---|
| Native player type check | 43.69s | Pass |
| Native state-capture tests | 1m20s compilation; 0.02s tests | Two passed |
| Native release build | 5m43s | Pass |
| First handler test attempt | 263.84s | Failed on a missing asynchronous completion barrier |
| Corrected handler test | 48.21s | Pass |
| Canonical workspace gate | 353s | Pass, including doctests |
| Independent whole-diff review | 649.68s | Pass, 57 requests at maximum effort |

The native build used the existing warm target. The first live candidate attempt was routed to the development instance by the production protection guard and failed on development credentials. It was stopped. The real check deliberately targeted the production instance using both `SPOTUIFY_INSTANCE=spotuify` and `SPOTUIFY_ALLOW_PROD_INSTANCE_FROM_TARGET=1`; no credential configuration changed. The installed Homebrew daemon remains the rollback target until the corrected release is installed.
