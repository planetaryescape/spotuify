# An audio reconnect replaces a queued playlist with one track

Status: reproduced on installed 0.1.110; corrected local native build passes the live journey; publication and installed verification remain.

At release commit `6c90d79c0d9c9c6a2596d62f193595c5a8c49b73`, queueing a readable three-track playlist while idle starts its first song. A system audio output change then calls `reconnect_player`. The actor shuts down the embedded player and reloads only the current track URI and position. That single-track load replaces the playlist context and queue. The first song reaches its natural end with no next track and playback stops. [Captured reproduction](../build-log/recovery/evidence/playlist-queue-before.json).

The correction retains librespot's native playback state through reconnect and reloads the current audio without replacing that context. Idle collection queueing passes the complete resolved list to the player, so recovery does not depend on a second playlist fetch or pending context pages. Ordinary shutdown still releases the player; ownership and mutation guards still cancel superseded reconnects.

Done when an accessible playlist queued through the CLI advances naturally to its second and third tracks, preserving the remaining queue after a forced reconnect or audio output change. Check both idle start and append to an active session, and retain paused playback behavior. Fake-provider Next checks alone do not establish this outcome.

[Live correction evidence](../build-log/recovery/evidence/playlist-queue-after.json) records idle queueing, active append, reconnect during a manually queued track, natural transitions through all three tracks, and paused reconnect followed by resume.
