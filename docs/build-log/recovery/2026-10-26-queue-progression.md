# Playlist queue progression fails after an audio reconnect

BK requested a fix after the 0.1.110 release could not establish natural playlist progression. The installed binary reproduced the failure: a three-track playlist started, an audio output change triggered reconnect, and the first track ended with an empty queue. The private runtime capture and [curated evidence](evidence/playlist-queue-before.json) agree. Source is release `6c90d79c`; the freshly fetched `origin/main` base `ed0a3b483e853be5e2fc14a0022c7cc4a5076425` has identical application code.

The reconnect actor calls the backend's ordinary shutdown and then `play_uri` with only the current URI and position. The embedded adapter converts that to a one-track Spirc load. Pinned librespot `f76335856e978d1cd6dd1352f2fdecd3a2a224f5` already returns `SavedPlaybackState` when its session becomes invalid; the app aborts the task and discards it. The correction uses that state for reconnect and keeps the existing native teardown safety boundary.

The idle collection path already resolves all playlist tracks. It now supplies that full ordered list to the embedded player and publishes the correct current head and remainder. This avoids depending on pending context-resolution pages during queue recovery. The native queue is retained directly; it is not rebuilt from a capped Web API queue snapshot.

Validation and live results are pending. The acceptance journey is queued playlist → reconnect → natural end of first track → second track with remainder → natural end → third track. Also check active-session append and paused reconnect. This is a repair within R1, ahead of the dependent R2–R5 work.

## Unresolved questions

- Does the corrected native build pass the same live end-of-track reproduction?
- Do paused playback, a changed user intent and a foreign device retain their existing behavior through the reconnect path?
