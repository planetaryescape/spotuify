# Transfer readiness uses a fixed wait and immediate readback

Baseline: `origin/main` at `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e`, `crates/spotuify-daemon/src/handlers/playback.rs:537,605-664`. Source inspection 2026-10-26.

An idle embedded device gets a fixed 1500 ms registration wait. An external transfer can reassert the current contextless track when immediate readback does not yet show the target. [Spotify documents](https://developer.spotify.com/documentation/web-api/reference/transfer-a-users-playback) that execution order across Player endpoints is not guaranteed. This source observation does not prove the original404 was caused by either branch.

Next action: use a fake transport whose device registration and playback readback become ready after controlled delays. Measure whether a bounded readiness poll and delayed ownership confirmation remove false healing without delaying a valid transfer unnecessarily. Preserve position and pause state; never start a stale song after a later user action.

Done when delayed readiness, delayed confirmation, denied transfer and changing user intent each produce one terminal receipt and a correct authoritative snapshot. Run one live transfer after cooldown and observe sustained target playback beyond the watchdog. Do not reset cooldowns to run it.
