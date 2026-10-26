# Transfer readiness uses a fixed wait and immediate readback

Baseline: `origin/main` at `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e`, `crates/spotuify-daemon/src/handlers/playback.rs:537,605-664`. Source inspection 2026-10-26.

An idle embedded device gets a fixed 1500 ms registration wait. An external transfer issues a `PlaySource::Single` replay when immediate readback does not show the target; the branch does not check whether the original playback has a collection context. [Spotify documents](https://developer.spotify.com/documentation/web-api/reference/transfer-a-users-playback) that execution order across Player endpoints is not guaranteed. This source observation does not prove the original 404 was caused by either branch.

Next action: use a fake transport whose device registration and playback readback become ready after controlled delays. Measure whether a bounded readiness poll and delayed ownership confirmation remove false healing without delaying a valid transfer unnecessarily. Preserve position and pause state; never start a stale song after a later user action.

Done when delayed readiness, delayed confirmation, denied transfer and changing user intent each produce one terminal receipt and a correct authoritative snapshot. Run one live transfer after cooldown and observe sustained target playback beyond the watchdog. Do not reset cooldowns to run it.


At recovery candidate `5ba879db01b0e2121ab5ed41a1e81d19959fe7a3`, `handlers/playback.rs:641-715` treats absent, default and stale source-device readbacks alike. The Spotify adapter suppresses readback errors with `.ok()` at `spotify/src/provider.rs:1182-1194`; HTTP204 also becomes default playback. The replay can therefore discard playlist/album context. Its failure is logged without failing the transfer body. A healed snapshot is applied after `persist_command_result` even if that function rejected the stale mutation generation. This is source evidence; it does not establish which branch caused BK’s original transfer failure.

The transport mutation lane serializes requests, but a newer command can advance the generation while waiting behind the current transfer. Generation checks must cover side effects and final state throughout readiness, readback and any retry. Existing transfer fixtures cover restricted targets and404; the fake provider transfers immediately. A controlled delayed/failing-readback transport fixture is needed for the reproduction matrix. Keep this as R3 work after R1 deployment rather than freezing an undeployed result contract.
