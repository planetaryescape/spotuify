# An audio reconnect replaces a queued playlist with one track

Status: closed; verified on installed Homebrew 0.1.111.

At release commit `6c90d79c0d9c9c6a2596d62f193595c5a8c49b73`, queueing a readable three-track playlist while idle starts its first song. A system audio output change then calls `reconnect_player`. The actor shuts down the embedded player and reloads only the current track URI and position. That single-track load replaces the playlist context and queue. The first song reaches its natural end with no next track and playback stops. [Captured reproduction](../build-log/recovery/evidence/playlist-queue-before.json).

The correction retains librespot's native playback state through reconnect and reloads the current audio without replacing that context. Idle collection queueing passes the complete resolved list to the player, so recovery does not depend on a second playlist fetch or pending context pages. Ordinary shutdown still releases the player; ownership and mutation guards still cancel superseded reconnects.

Done when an accessible playlist queued through the CLI advances naturally to its second and third tracks, preserving the remaining queue after a forced reconnect or audio output change. Check both idle start and append to an active session, and retain paused playback behavior. Fake-provider Next checks alone do not establish this outcome.

[Native candidate evidence](../build-log/recovery/evidence/playlist-queue-after.json) records idle queueing, active append, reconnect during a manually queued track, natural transitions through all three tracks, and paused reconnect followed by resume.

[PR 157](https://github.com/planetaryescape/spotuify/pull/157) merged as main head `720d0c16b4653029316f04c0b8c8a0237fbb088b`; the source fix is `2d3831cbbc916a905b2ec29ae0fee744a89117f7` (tree `9e26c48483822b929e446560c5467045082eb09a`). [Release PR 158](https://github.com/planetaryescape/spotuify/pull/158) published `v0.1.111` at `d3550b81d5b61eda97f00bf1bcf9cc4304e2eebe` (tree `cdbe1067b142f1d3e0c6fa4077959be9d160717e`). The tagged Cargo install and installed Homebrew queue journey passed. [Installed evidence](../build-log/recovery/evidence/playlist-queue-installed111.json) records the initial status assertion timeout and the same queue's successful continuation: actual end-of-track progression to the third track, 60 seconds of advancing progress and fresh PCM. The timeout came from a separate playback-status race tracked in [issue 007](007-stale-playback-poll.md). The R3 foreign-cluster/queued-activation race remains separate.
