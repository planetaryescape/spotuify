# The listening failures cross selection, access and recovery boundaries

Reviewed on 2026-10-26. Code baseline: spotuify `origin/main` at `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e`. The installed binary was Homebrew `0.1.109`, daemon PID 34068, on macOS ARM64. Proposed and uncommitted corrections are recorded separately from baseline observations.

## The operation log preserves both reported failures

```sh
spotuify ops log --limit 15 --format json
spotuify ops show 01a177c6-6746-73e0-8ee7-7108c3b92c80 --format json
spotuify devices --format json
spotuify doctor --format json
```

Observed results:

- TUI queue operation `01a177c5-de13-7530-8b3a-f6bc2e6e94c3` targeted `spotify:playlist:0uZ4faHZ6X7ctwUG4pcxr1` and failed with “playlist is not accessible”. Daemon log2026-10-26T10:33:38 shows GET `/playlists/<id>/items` returning403 Forbidden using the dev-app bucket.
- TUI transfer `01a177c6-6746-73e0-8ee7-7108c3b92c80` failed with `provider upstream error (HTTP 404): PUT /me/player: Not found.` Its pre-state retains the previous device but not the selected target. That response does not establish an Echo policy or a specific account cause.
- The device list contained 12 devices, all inactive. Playback was paused with `device:null`, source `recent-fallback`. These observations alone do not diagnose a failure.
- Doctor reported `healthy:true`, `health_class:healthy`, and `device_diagnostics:null`. The provider check said `ok:true`, “skipped: provider runtime unavailable outside the daemon”. It did not test provider health.
- Repeated auto-reconnect/resume decisions at 10:34:31,10:35:57,10:36:04 and 10:36:18 used `we_are_active=true`, while librespot logs identified a remote active device between attempts.

A direct CLI reproduction, `spotuify playlist tracks spotify:playlist:0uZ4faHZ6X7ctwUG4pcxr1 --format json`, stopped before the provider with `no playlist matching`. Two subsequent read-only upstream probes, metadata and one-item playlist contents, returned 429; the latter included `QUOTA_EXCEEDED`. Probes stopped. No token was printed or saved in the evidence. The later 429 does not prove the cause of the earlier 403/404.

## Source explains the defects without guessing upstream policy

| Finding | Source at baseline SHA | Correction boundary |
|---|---|---|
| Narrow search still tiles categories | `crates/spotuify-tui/src/ui.rs:3668-3701` | One primary list and category navigation |
| Artist detail loses width to long context | `crates/spotuify-tui/src/ui.rs:3879-3891` | Bound secondary context width |
| Kind jump uses raw indices | `crates/spotuify-tui/src/app.rs:6212-6218`; visible order2036-2105 | Use visible filtered/sorted order |
| Playlist Enter ignores cursor | `crates/spotuify-tui/src/app.rs:7244-7254` | Selected-track URI plus playlist context |
| CLI explicit playlist lookup requires saved list | `crates/spotuify-cli/src/commands.rs:3726-3744` | Direct URI/share-link dispatch for read/play |
| Unclassified403 becomes private | `crates/spotuify-spotify/src/provider.rs:587-603` | Keep typed permission error |
| Queue collapses unavailable reason | `crates/spotuify-daemon/src/handler.rs:3618-3625` | Preserve upstream evidence and action context |
| Transfer hint misses generic404 | `crates/spotuify-daemon/src/handlers/playback.rs:576-596` | Target-named guidance with preserved cause |
| Handoff maps to connection loss | `crates/spotuify-player/src/backends/embedded/mod.rs:1322-1325` | Internal Deactivated event |
| Recovery consumes stale ownership | `crates/spotuify-daemon/src/state.rs:5547-5551,5584-5628` | Clear on deactivation; recheck after backoff |
| CLI doctor builds local-only report | `src/main.rs:3049-3056` | Existing GetDoctorReport IPC |
| Findings append after health election | `crates/spotuify-daemon/src/diagnostics.rs:309-315`; `handlers/admin.rs:337-358` | Recompute aggregate health last |

The librespot dependency is pinned to `f76335856e978d1cd6dd1352f2fdecd3a2a224f5`. At `connect/src/spirc.rs:1151-1157`, a different active device triggers `handle_disconnect`; that function emits librespot `SessionDisconnected` at 1441-1452. Spotuify's wrapper separately emits its domain `SessionDisconnected` when the actual Spirc task exits (`embedded/mod.rs:718-730`). This establishes why those meanings must be separated.

The introducing history supports keeping true-loss recovery. Spotuify commit `fadc2c4d01af7b72324a7407e0af4a38078e94a3` added task-loss recovery on 2026-06-14. `5442e659717cca8a63be00b0a5d39367c4dc7648` recognised device deactivation for the activation latch but left the domain failure mapping unchanged.

## Current primary sources constrain the correction

[Spotify's available-device reference](https://developer.spotify.com/documentation/web-api/reference/get-a-users-available-devices) says IDs are not guaranteed persistent and restricted devices reject Web API control. [Transfer playback](https://developer.spotify.com/documentation/web-api/reference/transfer-a-users-playback) accepts one device ID and does not guarantee ordering with other Player endpoints. Refresh identity and verify sustained ownership; do not infer successful audible transfer from HTTP acceptance.

[The February migration guide](https://developer.spotify.com/documentation/web-api/tutorials/february-2026-migration-guide) describes playlist `/items`, restricted content access and search pagination. [The announcement's March 9 update](https://developer.spotify.com/blog/2026-02-06-update-on-developer-access-and-platform-security) postpones endpoint access changes for existing integrations. These sources describe rollout variation; this account's 403 is the evidence for this case. Do not claim one access model applies to all accounts.

[spotify-player's documented interactions](https://github.com/aome510/spotify-player) distinguish selection actions, queueing, search and device choice. Adopt context-sensitive naming. Its multipane geometry is not an answer to this application's observed width failure. [ncspot](https://github.com/hrkfdn/ncspot) provides a resource-oriented terminal music reference and IPC; it does not prove another client's current reliability. [Ratatui's tabs example](https://ratatui.rs/examples/widgets/tabs/) demonstrates a selected tab and content region. Spotuify needs a small wrapping adaptation for terminal cells, without introducing a new UI framework.

[GitHub's runner documentation](https://docs.github.com/en/actions/concepts/runners/self-hosted-runners) establishes that repository runners belong to one repository; [runner registration guidance](https://docs.github.com/en/enterprise-cloud%40latest/actions/how-tos/manage-runners/self-hosted-runners/add-runners) also explains the exposure of persistent machines to public pull-request code. Keep PRs hosted and use dedicated trusted local routing.

## Further verified gaps remain separate work

- Nonblocking playlist refresh drops metadata resolution errors at `crates/spotuify-daemon/src/handlers/playlists.rs:1740-1751` and only debug-logs item failures at 1804-1807. A first load can return an empty success while a background failure never settles visible loading state. A complete loading/empty/error contract needs protocol and client work.
- Device transfer uses a fixed 1500 ms registration sleep (`handlers/playback.rs:537` at baseline) and can reassert a contextless current track after immediate readback (`605-664`). Endpoint ordering is not guaranteed; prove a bounded readiness/verification state before changing this path.
- Search categories with no results have no selectable tab, even when a category failed. Loading/error placeholders need a defined state model.
- The player occupies substantial vertical space on small screens. The direct search correction does not redesign global chrome.
- `app.rs` contains 15,087 lines and `ui.rs` 7,652 at baseline. A future structural task can split search, playlist and device interactions/rendering by job. Move code verbatim first, preserve public entry points, and separate behaviour commits; do not mix it into this repair.
- Log rotation limits daily files, not total bytes, and `logging.rs:121-138` reads the whole active file for a tail. The correction now uses a bounded 1 MiB reader with explicit overflow; byte-limited storage remains tracked.

The [correction handover](../implementation/2026-10-26-recovery-handover.md) records exact validation and remaining blockers. Findings here are scoped to the examined journeys and evidence, not an exhaustive claim of correctness.

## The fake CLI gate exposed additional parity gaps

The corrected debug binary completed fake search, play, playlist URI read and equivalent share-link read. The next command, `queue add spotify:playlist:quiet-storm --wait --format json`, failed locally with exit2: “resolved target has kind playlist, which is not allowed”. The daemon and TUI already expand playlist contexts; CLI input validation accepted only individual items. This is an application parity defect, independent of Spotify's403. The queue accepted-kind correction and optional contextual track-play argument are part of the final repair wave.

The first complete daemon test run passed 387 cases and failed one existing wall-clock test: a 100 ms sleep resumed after 245 ms and correctly produced 10490 ms of progress at 2x. The test's upper bound assumed 200 ms maximum scheduler delay. The correction supplies exact elapsed time to the existing clock-state method and preserves public speed assertions; no playback-clock production behaviour was changed for that failure.

Independent review of the replacement renderer identified a navigation regression before release: global movement could choose a hidden type under Name sort. D-054 records the reversal; the final correction projects movement through the rendered type while retaining the global visible index for actions.

## Rust 1.99 Clippy rejects attributes generated by async-trait

`cargo clippy --workspace --all-targets -- -D warnings` on Rust 1.99.1 produced 21 `double_must_use` errors in unchanged core trait definitions. The locked `async-trait` 0.1.89 macro unconditionally injects `#[must_use]` at its `src/expand.rs:69`. [Upstream's report](https://github.com/rust-lang/rust-clippy/issues/17529) reproduces the same macro diagnostic; [PR 17833](https://github.com/rust-lang/rust-clippy/pull/17833) addresses macro-injected attributes.

The compatibility correction scopes an allowance to the eight async-trait definitions, with the cause and upstream reference beside it. It does not disable workspace warnings or change runtime logic. A separate existing test uses `AtomicUsize::fetch_update`, deprecated on this compiler; its method has a scoped allowance to retain the declared Rust 1.88 API compatibility. Revisit these allowances when the upstream fix reaches the supported compiler.
