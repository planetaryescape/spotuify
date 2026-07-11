# Races and Concurrency Audit

Worktree: `/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/races-concurrency`
Branch: `codex/audit-races-concurrency-20260619`
Date: 2026-06-19

Scope reviewed: daemon state, IPC request fanout, mutation lanes, event broadcast/subscription, token refresh, player actor/reconnect, queue warming, search worker, sync loop, reminders, and SQLite write paths.

## Summary

- Findings: 4
- Priority: 1 P1, 2 P2, 1 P3
- Theme: several core loops are deliberately hardened, but newer mutating surfaces and first-party auth paths have weaker serialization than the older transport/dev-app token paths.

## P1 - Some mutating requests bypass mutation lanes

Issue: `DaemonState::mutation_lane` serializes only a subset of mutating requests. Several request variants that mutate the same remote/local resources return `None`, so their optimistic mutation bodies can run concurrently with related work.

Evidence:

- The lane map covers playback/device/`QueueAdd`, playlist add/remove/tracks/create, library save/unsave, and ops undo/redo only: `crates/spotuify-daemon/src/state.rs:826`.
- `QueueAddMany` is a queue mutation in the protocol, distinct from `QueueAdd`: `crates/spotuify-protocol/src/lib.rs:194`, `crates/spotuify-protocol/src/lib.rs:201`.
- `QueueAddMany` runs a multi-item queue mutation body via `spawn_optimistic_mutation`: `crates/spotuify-daemon/src/handlers/playback.rs:694`, `crates/spotuify-daemon/src/handlers/playback.rs:702`.
- `QueueAdd` and `QueueAddMany` both read the live queue for dedup before issuing per-item queue calls: `crates/spotuify-daemon/src/handlers/playback.rs:552`, `crates/spotuify-daemon/src/handlers/playback.rs:728`.
- `PlaylistUnfollow` and `PlaylistSetImage` are playlist mutations in the protocol: `crates/spotuify-protocol/src/lib.rs:258`, `crates/spotuify-protocol/src/lib.rs:268`, but they are not in the playlist lane map at `crates/spotuify-daemon/src/state.rs:831`.
- `ArtistFollow` and `ArtistUnfollow` are library mutations: `crates/spotuify-protocol/src/lib.rs:223`, `crates/spotuify-protocol/src/lib.rs:227`, but the library lane only includes save/unsave at `crates/spotuify-daemon/src/state.rs:835`.
- `NotificationAct::Queue` also mutates the queue directly with no lane coverage: `crates/spotuify-daemon/src/handlers/reminders.rs:108`.

Impact: concurrent queue mutations can both dedup against the same pre-mutation live queue, then interleave POSTs and optimistic cache writes. That can produce duplicates, misleading receipts, or stale queue snapshots. Concurrent playlist mutations can invalidate pre-state/reversal assumptions captured before the Spotify call. Artist follow/unfollow can race and leave cache/event ordering different from the final remote state.

Recommended action: treat `mutation_lane` as the complete mutating-resource taxonomy. Add `QueueAddMany` and queueing notification actions to the transport/queue lane; add playlist unfollow/set-image to `playlist_lane(playlist)`; add artist follow/unfollow to a library or per-artist lane. Add a protocol-level test that enumerates mutating requests and asserts they map to a lane or have a documented exemption.

Confidence: High.

Validation idea: create a fake Spotify client test that dispatches `QueueAddMany` and `QueueAdd` concurrently, pauses both after `live_queue_uris`, then releases both and asserts only one serialized live read/append sequence can occur after the fix.

## P2 - First-party bearer refresh is not single-flight

Issue: first-party bearer caching uses a short `parking_lot::Mutex` only around cache read/write. The expensive mint/refresh path is outside that lock, so a burst of concurrent clients after TTL expiry or forced refresh can all mint/refresh independently.

Evidence:

- The shared first-party bearer cache is `Arc<parking_lot::Mutex<Option<(String, Instant)>>>`: `crates/spotuify-daemon/src/state.rs:450`.
- Each `SpotifyClient` gets a new `FirstPartyBearerProvider` sharing that cache: `crates/spotuify-daemon/src/state.rs:1755`.
- `bearer(false)` checks `cached()`, then calls `mint().await`, then `store()`; no async single-flight guard spans the check and mint: `crates/spotuify-daemon/src/state.rs:1954`, `crates/spotuify-daemon/src/state.rs:1961`, `crates/spotuify-daemon/src/state.rs:1966`.
- `bearer(true)` clears the cache, then refreshes OAuth credentials and may persist a rotated refresh token: `crates/spotuify-daemon/src/state.rs:1956`, `crates/spotuify-daemon/src/state.rs:1976`, `crates/spotuify-daemon/src/state.rs:1984`.
- The dev-app path does have single-flight token acquisition around cache/file refresh: `crates/spotuify-spotify/src/auth.rs:215`, `crates/spotuify-spotify/src/auth.rs:217`, `crates/spotuify-spotify/src/auth.rs:238`.

Impact: startup sync, TUI seed, CLI requests, and background health can stampede the player actor for `WebApiToken`. On 401, multiple forced refreshes can concurrently use the same stored first-party refresh token; if Spotify rotates it, a later save can leave stale credentials or cause avoidable auth failures/rate limits.

Recommended action: add a daemon-wide async single-flight guard for first-party bearer acquisition. Re-check the cache after acquiring the guard, refresh/mint once, and make rotated refresh-token persistence compare-and-swap against the credential version that was loaded.

Confidence: High.

Validation idea: fake `WebApiBearerProvider`/player actor test with 20 concurrent `bearer(false)` calls after cache expiry; assert one mint. Repeat with `bearer(true)` and rotating refresh tokens; assert one persisted rotation and all callers receive the same fresh bearer.

## P2 - Reminder firing is fetch-then-insert without claiming the due row

Issue: the reminder loop selects due schedules, inserts a notification, emits `ReminderDue`, then advances/completes the schedule. None of those writes condition on the schedule still being active at the selected due time.

Evidence:

- The loop reads due reminders, then fires each returned row later: `crates/spotuify-daemon/src/reminders.rs:78`, `crates/spotuify-daemon/src/reminders.rs:82`.
- Firing inserts the notification and emits the event before advancing/completing the reminder: `crates/spotuify-daemon/src/reminders.rs:114`, `crates/spotuify-daemon/src/reminders.rs:118`, `crates/spotuify-daemon/src/reminders.rs:120`.
- `due_reminders` is a plain `SELECT` over active rows: `crates/spotuify-store/src/lib.rs:1638`.
- `advance_reminder` and `complete_reminder` update by `id` only, without `state = 'active'` or `next_due_at_ms = old_due`: `crates/spotuify-store/src/lib.rs:1650`, `crates/spotuify-store/src/lib.rs:1659`.
- `cancel_reminder` can concurrently mark the same row cancelled by id: `crates/spotuify-store/src/lib.rs:1629`.
- Notifications have no uniqueness constraint on `(reminder_id, due_at_ms)`: `crates/spotuify-store/src/lib.rs:3207`.

Impact: a reminder cancelled after `due_reminders` returns can still generate a notification. If two daemon instances or two loop tasks ever overlap during restart/pathological lifecycle, the same due occurrence can be inserted twice because the insert uses a new UUID and the schedule is claimed only after notification emission.

Recommended action: claim due reminders atomically before emitting. Use a transaction or conditional update like `UPDATE reminder_schedules SET state = 'firing' WHERE id = ? AND state = 'active' AND next_due_at_ms = ? RETURNING *`, then insert notification and advance/complete from that claimed state. Add a unique index on `(reminder_id, due_at_ms)` for idempotency.

Confidence: Medium-high.

Validation idea: store-level concurrency test with one task reading a due reminder while another cancels it before `insert_notification`; assert no notification is emitted after the fix. Add duplicate-fire test with two concurrent claim attempts for the same due row.

## P3 - Event log drops events whenever the ring lock is busy

Issue: daemon event broadcast is reliable enough for live subscribers, but the diagnostic event ring uses `try_lock()` and silently skips logging when the ring is locked.

Evidence:

- The event log is the recent-event ring used by doctor/diagnostics: `crates/spotuify-daemon/src/state.rs:432`.
- `emit_daemon_event` sanitizes the event and then attempts `event_log.try_lock()`: `crates/spotuify-daemon/src/state.rs:2558`, `crates/spotuify-daemon/src/state.rs:2565`.
- If the lock is unavailable, the code skips `log.push(...)` and still broadcasts the event: `crates/spotuify-daemon/src/state.rs:2565`, `crates/spotuify-daemon/src/state.rs:2577`.
- Doctor/report snapshots take the async mutex normally: `crates/spotuify-daemon/src/state.rs:1611`.

Impact: high-value diagnostics such as auth, rate-limit, schema compatibility, or player degradation can be missing from `doctor`/recent-events during bursts. This does not drop the live broadcast, but it weakens post-incident debugging precisely under load.

Recommended action: avoid lossy `try_lock` for the diagnostic ring. Options: move event-log appends to a small bounded mpsc worker, use a non-async parking-lot ring lock if appends remain synchronous, or record a dropped-event counter when `try_lock` fails so diagnostics can say the ring is incomplete.

Confidence: Medium.

Validation idea: hold `event_log_snapshot()`/the ring lock in a test while emitting `AuthError` and `RateLimited`; assert either events appear later through the worker or a dropped-count diagnostic increments.

## Notes

- Search indexing is serialized through a single blocking worker and bounded request/response timeouts: `crates/spotuify-search/src/lib.rs:112`, `crates/spotuify-search/src/lib.rs:160`.
- The dev-app token path is stronger than the first-party path: it holds the cache mutex and token-store lock across refresh decision/execution.
- Playback command stale-poll protection is intentionally designed around `mutation_seq`; this audit did not find a higher-confidence lost-update bug in that specific path.
