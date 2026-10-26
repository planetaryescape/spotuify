The classified-403 path now surfaces `read playlist {uri} items ({reason:?})` as a typed Forbidden error plus a `tracks-inaccessible` event — so Spotify's cause and the playlist identity are preserved on the read surface, and unclassified 403s produce the new development-mode guidance. That closes the last open question in the error-handling scope.

---

# R1 Fresh Whole-Candidate Adversarial Review — Verdict: **PASS** (no accepted in-scope blockers)

## What I verified (read from the exact checkout, not the stale `target-cli` artifacts)

**Playback ownership / Connect handoff (the highest-risk area)**
- `crates/spotuify-player/src/backends/embedded/mod.rs:1325-1328,678-683,728-733` — librespot `SessionDisconnected` (handle_disconnect/handoff) → internal `Deactivated`; `SessionConnected` → `Activated`; only the Spirc task-exit wrapper emits the domain `SessionDisconnected` that still triggers recovery. The two meanings are genuinely separated.
- `crates/spotuify-daemon/src/state.rs:5482-5780` — `forward_player_events`: Deactivated bumps `mutation_seq` and clears `we_are_active` **before** awaiting the session tracker; `inactive_after_handoff` drops queued TrackChanged/Playing/PositionTick/pause/volume-as-clock events until `Activated`; late local volume still updates `own_device_volume` without overwriting a foreign clock device (`device_matches_own` gate); stale `SessionDisconnected/Failed` cannot schedule reconnects while inactive.
- `AutomaticReconnectGuard` (`state.rs:750-788`) is checked at all three automatic-reconnect sites (event-driven `state.rs:5731`, health loop `3651`, audio-stall `3785`), re-checked after backoff (`5849-5863`) and inside the actor before teardown, after shutdown, and after registration (`4881-4910`); manual `request_player_reconnect` bumps the generation (`2593-2598`) so user intent supersedes. `reconnect_in_flight` is reset on every path including timeout/send failure (`5886-5892`).
- `clock.rs:221-231` freezes on Deactivated (foreign-device case excluded in the forwarder); `session_tracker.rs:306-312` never qualifies a handoff-interrupted track. Mock backend mirrors the lifecycle (`mock.rs` Activated on play/resume; registration gate).

**Search / TUI selection**
- Single full-width active category (`ui.rs:3631-3748`), tab hits map to visible indices, hit map cleared per frame (`ui.rs:72`), row hits remapped through `group_indices` (`ui.rs:3781-3904`).
- `app.rs:2604-2630` keeps arrow/page/top/bottom inside the drawn kind's visible indices; `g`-prefix (`6283-6291`), Tab/`cycle_search_panel` (`8828-8862`) and mouse all operate on `visible_items()`; `selected_item()` indexes the filtered/sorted view (`2233-2235`). `truncate_media_row` (`ui.rs:3908-3930`) is cell-width/grapheme-safe.
- Open playlist Enter sends the highlighted track + playlist context (`app.rs:2341-2364`, `7278-7345`); the generic `selected_play_target_uris` is used only for capability gating, never dispatch. Queue hint "Queue playlist" when a playlist row is focused (`ui.rs:5196-5201`).

**CLI / MCP / daemon parity**
- `commands.rs:1848-1931` `play-uri --context`: kind/scheme/capability validation before mutation, Liked-Songs sentinel preserved; MCP `play_context_uri` (`bridge.rs:740-778`) + schema advertisement (`rpc.rs:647-655`) + pre-dispatch validation (`tools.rs:650-653`) match; daemon `resolve_play_context` (`handler.rs:3920-3967`) is the authoritative cross-provider/kind gate.
- `commands.rs:317-333` + `ipc_queue_add:3433-3512` accept album/playlist with collection-read capability gates; daemon expansion (`handler.rs:3583-3617, 3630-3695`) is authoritative and rejects artist/show. `daemon_playlist_target` (`commands.rs:3816-3833`) resolves explicit URIs/share links without saved membership and falls back to name lookup.

**Errors / doctor / logs**
- Unclassified playlist 403 stays typed with playlist identity + development-mode guidance (`spotify/provider.rs:590-608`); classified 403s surface `read playlist {uri} items ({reason:?})` (`playlists.rs:139-159`). Transfer: restricted devices fail before transport execution, plain 404 keeps the target in the hint with the provider cause preserved in `detail` (`playback.rs:530-544, 600-616`; `handler.rs:1399-1500` redaction targets only credential-shaped tokens).
- Doctor queries the daemon (`src/main.rs:3053-3078`), both finding-append sites recompute health last (`diagnostics.rs:313-316, 459-491`, `handlers/admin.rs:334-357`).
- `logging.rs:113-182` bounded 1 MiB + boundary-byte tail with explicit overflow error; UTF-8/grapheme and sparse-file tests present.

**Dependency / MSRV / CI / release seams**
- Tantivy 0.27 with zstd 0.14.3/safe 8.2/sys 2.1.2 in `Cargo.lock`; search crate uses no removed APIs (no stemmer usage); remediation record states Tantivy 0.27 declares Rust 1.88, and the failed 1.88 run names only the unchanged quantette/wide/safe_arch chain — so the new graph does not exceed the declared 1.90. `Cargo.toml` declares 1.90; CI filter sets `msrv` exactly on root/child manifests, lock, toolchain files, and the job pins `1.90.0`. Index fixture documents segment-ID churn with identical user-visible results — no universal-compatibility claim.
- `ci.yml` (filter, macos-smoke with trusted-main-only self-hosted routing, MSRV job), `release.yml` (runner substitution, isolated Rust home, ci-cargo queue, `sign_macos_cli.sh` with explicit keychain selection/restoration and cleanup trap), `release-lockfile.yml` no-reply identity, `release-please.yml` dispatch, `.cargo/config.toml` caps, and all four shell test harnesses match the documentation. No `rust-toolchain` pin contradicts 1.90; `ops log` is a real subcommand.

## Nonblocking notes
1. Hosted exact-Rust-1.90 CI, tag-driven release, and native build on the final head are controller-owned runtime gates still pending — the code does not prevent them; the evidence log for the dependency-updated suite (2,017 passed / 0 failed / 1 existing ignored, 82 groups) is retained in the checkout.
2. Live account/device gates S1–S3 remain cooldown-bound runtime checks; no code path blocks them.
3. Tantivy index evidence is fixture-level only (segment IDs churn); consistent with the documented claim.
4. Classified-403 reasons (private/region/subscription/temporary) are surfaced on the playlist read path but still collapse to the legacy "playlist is not accessible" text on the queue-expansion path — unchanged baseline behavior inside the documented repair boundary.
5. `.cargo/config.toml` caps every local build at 4 jobs (documented worker-cap policy); `docs/implementation/00-current-state.md` retains a stale pointer to the old `/tmp` clone.

## Review limitations
Read-only: no execution, no runtime/audio/network certification, no inspection of credentials or live account state; dependency-graph MSRV relied on the retained 1.88 check log plus the remediation record rather than a fresh 1.90 build; the `site/public/spotuify.skill` binary delta was not inspected byte-for-byte (site/skill gates are covered by CI and retained audit evidence, out of this scope).
