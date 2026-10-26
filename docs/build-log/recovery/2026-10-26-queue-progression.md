# Playlist queue progression fails after an audio reconnect

BK requested a fix after the 0.1.110 release could not establish natural playlist progression. The installed binary reproduced the failure: a three-track playlist started, an audio output change triggered reconnect, and the first track ended with an empty queue. The private runtime capture and [curated evidence](evidence/playlist-queue-before.json) agree. Source is release `6c90d79c`; the freshly fetched `origin/main` base `ed0a3b483e853be5e2fc14a0022c7cc4a5076425` has identical application code.

The reconnect actor calls the backend's ordinary shutdown and then `play_uri` with only the current URI and position. The embedded adapter converts that to a one-track Spirc load. Pinned librespot `f76335856e978d1cd6dd1352f2fdecd3a2a224f5` already returns `SavedPlaybackState` when its session becomes invalid; the app aborts the task and discards it. The correction uses that state for reconnect and keeps the existing native teardown safety boundary.

The idle collection path already resolves all playlist tracks. It now supplies that full ordered list to the embedded player and publishes the correct current head and remainder. This avoids depending on pending context-resolution pages during queue recovery. The native queue is retained directly; it is not rebuilt from a capped Web API queue snapshot.

The local native candidate `53c773afe1cdbbd739cb683de2d67665e9fb9e6e` passed that live journey: queued playlist → reconnect → natural end of first track → second track with remainder → natural end → third track. It also passed active-session append, another reconnect while playing a manually queued track, and paused reconnect followed by resume. Fresh sink PCM was observed on each new track. These were actual end-of-track transitions after seeking near each ending, without manual Next. [Curated evidence](evidence/playlist-queue-after.json).

The native build passed in 5m43s. Two bounded state-capture lifecycle tests passed. The new handler test initially raced the asynchronous queue operation; a test-only correction waits for its `QueueChanged` event before inspecting recorded calls. The corrected focused test passed. The canonical workspace suite passed 2,020 tests, zero failed and one existing ignored test across 82 groups, including doctests, in 353 seconds. Hosted CI passed every executed check on `ad6f7c59`. The independent whole-diff review passed at maximum reasoning effort; [report and triage](evidence/queue-independent-review/triage.md). The native fake-provider smoke passed.

[PR 157](https://github.com/planetaryescape/spotuify/pull/157) merged at main head `720d0c16b4653029316f04c0b8c8a0237fbb088b`; its source fix is `2d3831cbbc916a905b2ec29ae0fee744a89117f7` (tree `9e26c48483822b929e446560c5467045082eb09a`). The test-only barrier is `52442a04d52d6545dcc57d378f94b3c3f0778a04`; hosted CI passed on equivalent commit `ad6f7c590ee0ffed72096bdaeca03ae9a3165bbb`. [Release PR 158](https://github.com/planetaryescape/spotuify/pull/158) published tag `v0.1.111` at `d3550b81d5b61eda97f00bf1bcf9cc4304e2eebe` (tree `cdbe1067b142f1d3e0c6fa4077959be9d160717e`); the public [changelog](https://spotuify.app/changelog/) lists the fix. A `--locked` Cargo source install from the exact tag exited 0 after 654.72 seconds (9m07s compile) and asserted `spotuify 0.1.111`; its throwaway install root was removed. Release workflow 40370416502 completed all eight jobs and published eight assets. The Homebrew Formula is at commit `413e23fdf3806cce0d1f5cb892bd17bafd0cd631`. Installed Homebrew `0.1.111` uses the sole path `/opt/homebrew/bin/spotuify`; see [release](evidence/release-111.json) and [install](evidence/cargo-install111.json) proof.

The installed journey passed queueing a playlist from idle, reconnecting, and naturally reaching all three tracks. The initial 24-second check for the second track in CLI status timed out: a Web API poll temporarily replaced the newer local track in the clock. The queue and audio continued, and status converged on a later poll. The same queue was then continued without another append; an actual end-of-track event moved playback from the second track to the third. Six watchdog samples over 60 seconds showed third-track progress increasing from 10,044 to 60,191 ms with fresh PCM, then playback was paused. [Installed evidence](evidence/playlist-queue-installed111.json) preserves the initial assertion timeout and successful continuation. The status race is a separate baseline issue in [issue 007](../../issues/007-stale-playback-poll.md).

## Unresolved questions

- The late foreign-cluster/queued-activation race remains R3 work; this correction does not claim to fix it. A transient native registration failure can consume the opaque saved state, so the backend reports an error and fails closed rather than replacing the queue.
- External-device handoff and S4 visual acceptance remain open under R1.

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
| Tagged Cargo source install | 654.72s wall; 9m07s compilation | Pass |
| ARM64 release binary compile | 11m38s | Pass |
| Intel macOS release binary compile | 19m00s | Pass |
| Installed status assertion | 24-second polling window | Timed out on the separate status race |
| Installed continuation and audio observation | 68.43s | Natural second-to-third transition; 60 seconds of fresh PCM |

The native build used the existing warm target. The first live candidate attempt was routed to the development instance by the production protection guard and failed on development credentials. It was stopped. The real check deliberately targeted the production instance using both `SPOTUIFY_INSTANCE=spotuify` and `SPOTUIFY_ALLOW_PROD_INSTANCE_FROM_TARGET=1`; no credential configuration changed. The installed Homebrew `0.1.111` journey passed for queue progression. A separate status-lag assertion timed out and is tracked in issue 007; the queue and audio continued through the next track.
