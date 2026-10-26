# A late playback poll can temporarily restore the previous track in status

Status: observed on installed `0.1.111`; the same source behavior is present in the `0.1.110` baseline. Playback and queue continued; no fix is included in the queue correction.

During installed queue verification, the native player advanced from one track to the next at 18:02:00.423 UTC. A Web API refresh applied a snapshot of the previous track at 18:02:00.445. CLI status showed the previous track until a later poll at 18:02:23.040. The same queue continued playing, and the follow-up capture verified natural progression from the second track to the third. [Curated installed evidence](../build-log/recovery/evidence/playlist-queue-installed111.json) retains both the timed-out immediate-status assertion and the successful continuation.

Baseline `v0.1.110` commit `6c90d79c0d9c9c6a2596d62f193595c5a8c49b73` and release `v0.1.111` commit `d3550b81d5b61eda97f00bf1bcf9cc4304e2eebe` share this path. `PlaybackGet` returns the clock snapshot and starts a background refresh (`crates/spotuify-daemon/src/handlers/playback.rs#L95-L106`). The refresh captures `mutation_seq`, awaits Web API playback, and stamps its sample time after the response (`crates/spotuify-daemon/src/handler.rs#L4271-L4317`). A native track change does not increment that sequence when the embedded provider already owns transport (`crates/spotuify-daemon/src/state.rs#L5502-L5521`), so the old poll can still be persisted (`state.rs#L6033-L6064`).

`PlaybackClock::apply_web_api_poll` protects fresh local state only when the item URI is unchanged. When the poll URI differs, it adopts the polled item and progress as `RemotePoll` (`crates/spotuify-daemon/src/clock.rs#L390-L419`). The test `clock_uri_change_always_rebases_even_from_web_api` asserts that behavior (`clock.rs#L1073-L1093`). The refresh, clock and relay code are unchanged from `0.1.110` to `0.1.111`; the queue release changed reconnect handling, not poll freshness. This issue is separate from the queue progression repair.

Next action: add a deterministic delayed-poll regression where a local `TrackChanged` event arrives before a response naming the previous track. Keep genuine remote track changes and device handoffs reconcilable.

Done when a late poll cannot overwrite newer local track state in either the daemon clock or durable playback cache, while a current remote poll still reconciles a real external track change.
