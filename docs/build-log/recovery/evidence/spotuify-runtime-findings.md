# Device hand-offs were being treated as dead connections

Baseline: freshly fetched `origin/main` at `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e`. Worktree: `/tmp/spotuify-recovery-clones/spotuify`. Changes below are uncommitted. Runtime evidence supplied by the root agent came from installed `spotuify 0.1.109`, Homebrew daemon PID 34068, on 2026-10-26.

## A Connect hand-off could trigger recovery that reclaimed playback

Root's log review observed repeated `we_are_active=true resume=true reconnect=true` at 10:34:31, 10:35:57, 10:36:04, 10:36:18, with the embedded fork reporting a remote active device between reconnects. This is evidence of the path being exercised; the source establishes why it can happen.

The pinned librespot source is `f76335856e978d1cd6dd1352f2fdecd3a2a224f5` (confirmed from the dependency checkout and Cargo.toml). `connect/src/spirc.rs:1151-1157` detects a different active device and invokes `handle_disconnect`. That function emits librespot's `SessionDisconnected` at `1441-1452`, even though the connection is still available. In spotuify baseline, `crates/spotuify-player/src/backends/embedded/mod.rs:1322-1325` translated it to the domain event meaning the session died. `crates/spotuify-daemon/src/state.rs:5547-5551,5584-5628` treated that event as an automatic reconnect trigger using the prior local playback snapshot.

There is already a separate reliable true connection-loss signal: the embedded Spirc task wrapper emits the domain `SessionDisconnected` when the task exits (`embedded/mod.rs:718-730`). It must remain a recovery trigger.

A focused archaeology pass found two June 14 decisions that explain the conflation. Commit `fadc2c4d01af7b72324a7407e0af4a38078e94a3` added the task-wrapper signal and resume-on-reconnect for real silent AP loss. Commit `5442e659717cca8a63be00b0a5d39367c4dc7648` explicitly recognized librespot's event as device deactivation and cleared its activation latch, but retained the existing domain connection-loss mapping. We should keep true-loss recovery and separate deactivation.

Implemented: `PlayerEvent::Deactivated`; adapter mapping for Connect hand-off; immediate removal of local recovery intent; pause local clock without pausing an already-known foreign-device snapshot; normal daemon PlaybackChanged event; stop the local listening session. A delayed reconnect checks current intent and playback after backoff and cancels after hand-off. A real closed Spirc task still emits SessionDisconnected and retains recovery.

Added tests: `connect_handoff_is_deactivation_instead_of_connection_loss`, `device_handoff_pauses_local_clock_without_reconnecting`, `delayed_reconnect_is_cancelled_after_handoff`.

## Device-transfer errors lost the device-specific recovery hint

Root retrieved failed operation `01a177c6-6746-73e0-8ee7-7108c3b92c80`: `provider upstream error (HTTP 404): PUT /me/player: Not found.` The baseline transfer handler only provided a user-facing hint for `ProviderError::NoActiveDevice` (`handlers/playback.rs:576-596`). Generic 404s remained raw upstream errors. A 404 proves the attempted operation failed; it does not establish that the device is offline or that every Echo device is unsupported.

Implemented: recognize typed 404/NotFound within this transfer handler; preserve the target name and suggest opening Spotify on that target and refreshing/retrying. Echo wording is conditional rather than an unsupported absolute. A device explicitly marked restricted is rejected before sending transfer.

Added test: `plain_transfer_404_receives_target_recovery_guidance` (also preserves 429/auth distinctions).

Primary references reviewed 2026-10-26: [Spotify transfer playback](https://developer.spotify.com/documentation/web-api/reference/transfer-a-users-playback), [available devices](https://developer.spotify.com/documentation/web-api/reference/get-a-users-available-devices). The transfer reference requires Premium and does not guarantee ordering across Player endpoints. Do not infer that a returned device list guarantees a successful transfer.

## Doctor skipped the provider and still said healthy

Observed by root: the installed `doctor --format json` returned `healthy:true`, `provider api` with `ok:true` and `skipped: provider runtime unavailable outside the daemon`, and `device_diagnostics:null`.

Source: `src/main.rs:3049-3056` always built a local report even with a running daemon. The reusable `GetDoctorReport` request already exists and is served by `handlers/admin.rs:318-360` with the actual provider registry and event history. Two additional stale-rollup defects existed: diagnostic event findings were appended after health classification (`diagnostics.rs:309-315`), and daemon player warnings were appended without recalculating health (`handlers/admin.rs:337-358`).

Implemented: CLI doctor asks the running daemon for its existing report, with a bounded 75-second request budget covering the six existing 10-second API check budgets plus auth. Offline doctor retains local config/status diagnosis. Appended event and player findings now update health_class/healthy without replacing the findings.

Added test: `appended_player_findings_update_the_health_rollup`. Root will prove live IPC use through the fake CLI workflow, not through the rate-limited real account.

## A generic playlist access denial was falsely called private

Root's captured failed QueueAdd `01a177c5-de13-7530-8b3a-f6bc2e6e94c3` selected `spotify:playlist:0uZ4faHZ6X7ctwUG4pcxr1`; the logged GET `/playlists/{id}/items` returned 403. Later raw metadata/items checks each returned 429, so this run cannot establish whether the original 403 came from app mode, playlist permissions, ownership, or another Spotify restriction. No further live probes are needed during the cooldown.

Source: `spotuify-spotify/src/provider.rs:587-603` classified any 403 without region/subscription/temporary keywords as `AccessUnavailable::Private`, including public playlist client-access denials. The queue expansion collapsed this to `playlist is not accessible` at `daemon/src/handler.rs:3618-3625`.

Implemented: explicit private-denial wording keeps Private; otherwise generic 403 becomes typed Forbidden containing playlist URI, HTTP status and an instruction to check app permissions and development-mode restrictions. It does not guess that the playlist is private and does not attempt another auth mode to bypass access controls.

Added test: `playlist_client_access_denial_is_not_mislabelled_as_private`; updated the existing private-outcome fixture to actually say private.

## Further defects require separate bounded slices

The playlist background refresh silently drops a metadata-resolution failure (`handlers/playlists.rs:1740-1751`) and only writes an items error at debug level (`1804-1807`). The nonblocking caller receives an empty successful list, so a first-load read error can look like an empty playlist without an event that settles the loading state. This is a separate error/result-state contract change across protocol and clients; document and implement a complete loading/error state, rather than treating an empty list as all three.

The transfer's fixed 1500 ms registration sleep (`handlers/playback.rs:537`) should eventually become a bounded check for the selected embedded device. Provider execution also treats immediate transfer readback as enough evidence to replay a contextless current track (`605-664`), while Spotify says Player endpoint ordering is not guaranteed. A delayed readback/verification state is preferable to guessing on the first read. These were not changed without a deterministic regression of the failure.

The current correction does not prove acoustic output or real speaker hand-offs. Root owns final build/test gates and the real CLI fake-provider journey; live playback must still verify the same track and advancing PCM after the watchdog window when the real API cooldown permits it.

Validation at report write: focused files formatted with rustfmt; `git diff --check` clean. Combined compile/tests pending root orchestration. No commits, release, install, or live playback mutations performed by this worker.

## Request-level transfer coverage and diagnostic context were added

A follow-up review added two full daemon request-to-final-receipt regressions with an injected remote transport: `restricted_device_transfer_fails_without_executing_transport` asserts the forbidden target never reaches execute; `plain_transfer_404_receipt_keeps_target_and_provider_error_detail` asserts one execute attempt, a failed receipt naming the selected speaker and recovery action, Provider classification/provider ID, and the original HTTP 404 plus endpoint in detail. Transfer recovery hints now wrap the typed error with anyhow context rather than replacing it. Provider error details preserve a redacted anyhow chain. A new trace immediately before execution records operation ID, provider, target name/ID and play intent.

The delayed reconnect guard now also rejects any known foreign device even if the tracked local-active flag is stale true; `delayed_reconnect_respects_foreign_snapshot_even_with_stale_active_flag` covers it. All three production scheduling paths (health probe, event-driven reconnect, audio-stall recovery) receive the current ownership flag and playback clock.

Remaining race: a reconnect already sent to the player actor can overlap a later hand-off while registration awaits. Closing that window requires cancellation/generation checks inside the actor and backend resume boundary. This patch cancels during backoff and separates the observed takeover event from true connection loss; it does not claim to eliminate every concurrent ownership race. No live hand-off or PCM proof was possible under the observed API cooldown in this slice.

Follow-up formatting was limited to touched handler/playback/state files; `git diff --check` remains clean. Root owns the running canonical compilation and tests; this worker did not start another Cargo process.

## The actor now checks recovery authority across registration

The previously recorded actor-registration race is addressed in the same correction. `AutomaticReconnectGuard` carries the captured mutation sequence and current ownership/clock state into the actor. It is checked before teardown, after teardown, and after registration before playback resumes. A newer command invalidates the generation. Explicit reconnects supersede pending automatic work and remain authoritative (`recovery: None`). Cancellation returns an internal superseded outcome to the scheduler and logs informationally, rather than emitting a provider failure.

Pinned shutdown semantics were checked before choosing these boundaries: librespot Spirc Shutdown calls handle_disconnect, but spotuify's embedded shutdown aborts and awaits both its player-event task and Spirc monitor before teardown (`embedded/mod.rs:982-1016` at baseline). Its intentional teardown therefore does not supply a valid deactivation signal that would cancel its own recovery.

A separate mock registration gate now lets a test hold the actor inside register_device, deliver a hand-off event through the normal player stream, then release registration. `automatic_reconnect_cancelled_when_handoff_arrives_during_registration` asserts the superseded outcome and that no PlayUri call occurred. `automatic_reconnect_superseded_in_actor_queue_never_shuts_down_player` checks a stale generation is rejected before teardown, leaving the connected player intact. These are in addition to backoff cancellation and request-to-receipt transfer coverage.

The canonical first compile exposed missing ResourceUri test imports and a missing IpcPayload::Event wrapper in the new event test; those are corrected. All source is frozen for the root's canonical rerun. The runtime correction still requires live playback/PCM confirmation after the API cooldown; fake tests establish state/control behavior, not the audible result on real speakers.

## The canonical run exposed a wall-clock test flake

The canonical daemon suite passed 387 tests and failed only `clock_advances_at_podcast_speed_for_episodes_only`: the episode assertion expected roughly 10,200 ms at 2x but observed 10,490 ms. The test sleeps for 100 ms and assumes it will be scheduled again within 200 ms; 10,490 ms corresponds to 245 ms elapsed. Running the same compiled test alone passed in 0.21 seconds. The clock itself correctly accounts for elapsed time; this test's upper wall-clock bound was invalid under load.

The test now supplies `base_instant + 100 ms` to the existing `ClockState::current_progress_ms(now)` method and asserts 10,200 ms for episodes and 10,100 ms for music. Public playback-speed and URI-only episode-stub assertions remain. No production timing behavior or new test hook was added. The edit is ready for the canonical rerun.

The same completed canonical daemon run passed the new deactivation, backoff cancellation, registration cancellation, stale-generation rejection, request-level restricted transfer, request-level 404 receipt, doctor rollup, and helper classification regressions. Root retains final workspace and CLI journey verification.

## MCP now exposes the existing collection playback context

The `play` and `play_uri` bridge tools accept optional `context_uri` and forward it through the existing `PlaybackCommand::PlayUri` request. Their RPC input schemas advertise the optional string. When supplied, the target must be a track or episode; context must be album, artist, playlist, or show on the same provider URI scheme, or the exact protocol Liked Songs sentinel. Malformed/wrong-type/cross-provider/leaf context and nested collection targets fail before daemon discovery or mutation. Omitted context preserves existing playback behavior and accepted target kinds. Provider transport-play capability checks remain in force.

Added bridge positive/negative coverage and RPC schema/provider capability rejection coverage in the MCP crate. The manifest snapshot records tool names/descriptions but not schemas, so this additive input-schema change requires no snapshot churn. Source was frozen and handed to root for the final combined workspace run; no separate Cargo process was started.
