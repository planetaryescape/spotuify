# librespot fork — maintenance & upstream tracking

> **spotuify pins a forked, patched build of librespot instead of the
> crates.io release.** This document is the authoritative record of *why*,
> *what*, and — most importantly — *how and when to drop the fork*. Read it
> before any dependency bump or release that touches playback.

## TL;DR

- **Pinned in:** the root [`Cargo.toml`](../../Cargo.toml) `[patch.crates-io]` block.
- **Fork:** <https://github.com/planetaryescape/librespot>
- **Branch:** `spotuify-session-recovery`
- **Rev (immutable pin):** `303026bba2af4c31e710afefc3aad4a89e38c812`
- **Equals:** upstream `dev` @ `33bf3a7` (still version `0.8.0`) + the 7 commits of upstream **PR #1692**.
- **Drop the fork when:** a librespot release **> 0.8.0** ships that includes the session-recovery work (PR #1692). Then delete the `[patch.crates-io]` block, bump the crates.io versions, and remove the now-redundant daemon reconnect shims (see [Removing the fork](#removing-the-fork)).

## Why we forked

librespot `0.8.0` (the latest crates.io release, tagged 2025-11-10) drops the
Spotify access-point session / dealer websocket roughly every **7–15 minutes**
and never re-establishes it. In `librespot-core` the keepalive `DispatchTask`
marks the session invalid on a missed pong and contains a literal
`// TODO: Optionally reconnect`. The symptom for spotuify users is **"music
just stops after ~15 minutes and pause/play won't bring it back"** — the
embedded Connect device goes silent and only a daemon restart recovers it.

spotuify already shipped daemon-side mitigations (auto-reconnect on detected
drop, an audio-flow watchdog, reconnect backoff — see
[`project_silent_session_drop`] context and the decision log). Those *recover*
after a drop but cannot prevent the gap. The actual cure lives in librespot:
upstream **PR #1692 — "dealer reconnect and session loss recovery without
playback interruption"** — which is **open and unmerged**, and therefore in no
released version.

Reimplementing librespot's session layer ourselves was explicitly rejected
(rodio + CoreAudio SIGSEGVs on AirPods disconnect, which is why spotuify uses
portaudio on macOS — see `docs/implementation/12-phase-9-librespot-embed.md`).
Forking and pinning the upstream fix, then dropping the fork once it lands,
is the bounded option.

## What the fork contains

The fork branch is upstream's `dev` branch (the base PR #1692 targets) with the
PR's commits on top. We based the branch on the PR's exact tested tree rather
than cherry-picking onto the `v0.8.0` tag because the PR depends on a `dev`
commit absent from the tag (the `emit_set_queue_events` config from #1677), and
hand-resolving the core reconnect logic onto a different base risked subtly
breaking the very fix we want.

Key facts that make this safe:

- The fork's crate versions are still **`0.8.0`** (dev has not bumped), so the
  `[patch.crates-io]` versions match and `version = "0.8"` in
  `[workspace.dependencies]` is satisfied.
- **No public-API removals** vs the `v0.8.0` tag. The only drift spotuify had
  to adapt to (see below) is additive.

### PR #1692 commits (the fix)

| Commit | Summary |
| --- | --- |
| `34d2fd9` | dealer websocket reconnect leaving spirc hung on stale channels |
| `812c972` | handle dealer reconnect in-place without restarting spirc |
| `18eb5be` | skip server cleanup on session loss to keep playback alive |
| `f69778d` | save and restore playback state across session reconnects |
| `2ac494b`, `48adba0`, `303026b` | `fixup!` commits squashing into the above |

The `dev` base also brings benign 0.8.0-line fixes we inherit for free
(integer-overflow fix #1678, try-all-resolved-socket-addrs #1651,
credential-file-permission fix #1650, `Emit SetQueue event` #1677, the
vergen 9.0.6 pin #1683).

## spotuify changes required by the fork's API drift

`dev` is additive vs `v0.8.0`, but two changes touched code spotuify compiles
against (`crates/spotuify-player/src/backends/embedded/mod.rs`):

1. **`SpotifyUri::to_uri()` became infallible** — it now returns `String`
   instead of `Result<String, _>`. `spotify_uri_string` was simplified to
   `uri.to_uri()` (the old `.unwrap_or_else(|_| uri.to_string())` fallback is
   gone).
2. **New `PlayerEvent::SetQueue` variant** (from #1677). The exhaustive match
   in `translate_librespot_player_event` gained a `SetQueue { .. } => None`
   arm — spotuify's daemon owns the queue, so this Connect-state notification
   is ignored like the other Connect-state events.

**If you re-fork / rebase and the build breaks, start here** — these two
adaptations may need revisiting against newer upstream.

## How to reproduce / rebuild the fork

```bash
# 1. Fork exists at planetaryescape/librespot. Clone it.
git clone git@github.com:planetaryescape/librespot.git
cd librespot
git remote add upstream https://github.com/librespot-org/librespot.git

# 2. Fetch upstream PR #1692 and recreate the branch from its tip.
git fetch upstream pull/1692/head:pr1692
git checkout -B spotuify-session-recovery pr1692
git push -u origin spotuify-session-recovery

# 3. Note the tip rev and pin it in spotuify's Cargo.toml [patch.crates-io].
git rev-parse HEAD
```

Constraints to preserve in spotuify's `Cargo.toml` when re-pinning:

- All six `librespot-*` crates point at the **same** git rev.
- `librespot-playback` keeps `default-features = false` in
  `[workspace.dependencies]` (audio-backend selection lives in
  `spotuify-player`'s own features; the `[patch]` only swaps the source).
- The `vergen = "=9.0.6"` pin stays (librespot-core 0.8's `build.rs` needs it).

## Upstream tracking — check on every dependency review and before each release

Watch these and re-evaluate whenever any change state:

- **PR #1692** — the fix. <https://github.com/librespot-org/librespot/pull/1692>
  - `gh pr view 1692 --repo librespot-org/librespot --json state,mergedAt`
- **PR #1690** — session recovery with automatic playback resume (complementary).
- **PR #1716** — keep spirc running after a transient connection-id update failure.
- **Issues** #1419 (broken-pipe/dealer drop), #1407 (spirc can't reconnect),
  #1627 (network-switch session loss).
- **Releases/tags:** `gh release list --repo librespot-org/librespot` — watch for
  anything **> v0.8.0**.

Help land #1692 upstream (comment with spotuify's soak evidence) so the fork
can be retired.

## Removing the fork

When a librespot **release > 0.8.0** ships that includes PR #1692 (or the fix is
otherwise on a tagged release):

1. Delete the entire `[patch.crates-io]` block from the root `Cargo.toml`.
2. Bump the `librespot-*` versions in `[workspace.dependencies]` to the fixed
   release. Re-check the `vergen` pin note.
3. Rebuild and re-test the two API-drift adaptations above (they may have
   shipped identically upstream, in which case no change is needed).
4. Re-evaluate the daemon-side reconnect shims that exist only because
   librespot didn't self-recover, and remove the now-redundant ones:
   - the spirc-task-end `SessionDisconnected` emit and `spirc_activated` latch
     reset in `crates/spotuify-player/src/backends/embedded/mod.rs`;
   - the audio-flow watchdog / auto-reconnect / backoff in
     `crates/spotuify-daemon/src/{server.rs,state.rs}` — keep what still earns
     its place as defence-in-depth; delete what only duplicates the upstream fix.
5. Delete the `spotuify-session-recovery` branch from the fork (optionally the
   whole fork) once nothing pins it.
6. Update this doc + the decision log + the `project_librespot_fork_maintenance`
   agent memory to record the fork's retirement.

## Verification

The fork must clear the same gates as any change, plus a real soak:

- `cargo build --release` (compiles all of librespot from git + spotuify).
- The CLI integration loop and `scripts/smoke.sh` (fake provider) stay green.
- **Soak (human, the real proof):** play to a real device (AirPods on macOS,
  portaudio backend) for **> 15 minutes** and confirm the logs show in-place
  dealer reconnects / preserved playback and **no** unrecovered
  `ERROR librespot_core::session: Connection to server closed.` gaps. This is
  the only test that proves the fork actually fixes the user-visible drop.

[`project_silent_session_drop`]: ../../README.md
