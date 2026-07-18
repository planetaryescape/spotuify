# Phase 0 - Boundary Hygiene

## Goal

Make the workspace's declared boundaries true. Delete dead dependency edges,
re-home type imports that go through the vendor crate, and fix documentation
that describes coupling which no longer exists. Pure cleanup — no behavior
change, no new abstractions.

## Standalone value (if the provider abstraction never happens)

Full. Dead deps slow builds and mislead readers; stale comments send the next
contributor (or agent) chasing coupling that isn't there — this audit itself
initially reported the TUI as holding a live `SpotifyClient` because the
in-repo comments say so.

## Evidence base

- `crates/spotuify-tui/Cargo.toml:12,13,15,16` declares `spotuify-store`,
  `spotuify-search`, `spotuify-sync`, `spotuify-player` — zero source
  references to any of them. Dead edges.
- `crates/spotuify-tui/src/app.rs:33` imports `Device, MediaItem, MediaKind,
  Playback, Playlist, Queue` via `spotuify_spotify::client` — these are
  re-exports of `spotuify_core` types (client.rs:24). The TUI links the vendor
  crate for types it could get from core.
- `tests/workspace_boundaries.rs:119-123` claims "app.rs talks to the live
  SpotifyClient + Store + Search + Sync" — false as of this audit; all TUI data
  flows over IPC (~44 `Request::*` variants).
- `docs/research/apple-music-feasibility.md` §3 repeats the same stale claim
  (corrected alongside this phase).

## Deliverables

1. Remove the four dead deps from `crates/spotuify-tui/Cargo.toml` and shrink
   the `ALLOWED_DEPS` entry in `tests/workspace_boundaries.rs` to match
   (`core, protocol, spotify, cli, daemon, launcher` remain — `spotify` for
   auth/config/action compatibility, `cli` for action types, `daemon` for
   audio-output enumeration, and `launcher` for lifecycle helpers; those edges
   are removed in later phases, not this one).
2. Re-point core-type imports in `app.rs` and `ui.rs` at `spotuify_core`
   directly, including test-only fully-qualified references.
3. Rewrite the `ALLOWED_DEPS` comments to describe the *actual* remaining
   edges (domain types/auth/config/actions/audio enumeration/lifecycle), so the
   table reads as a truthful debt register instead of fiction.
4. Correct `docs/research/apple-music-feasibility.md` §3: the TUI coupling is
   type-import paths + in-process OAuth + config reads, not a live client.
5. TUI lifecycle helpers: switch `ensure_daemon_running`/`restart_daemon`
   imports from `spotuify_daemon::server` re-exports to `spotuify_launcher`
   directly. `list_audio_outputs` stays on the daemon edge for now (it wraps
   `spotuify_player::list_audio_outputs`; routed over IPC in phase 8).

## Non-goals

Do not move `Config` or `LoginProgress` (phase 7). Do not touch CLI deps. Do
not tighten boundaries beyond what compiles today — enforcement ratchets in
later phases as edges are actually removed.

## Verification

```bash
scripts/cargo-nextest -p spotuify-tui
scripts/cargo-test --workspace   # workspace_boundaries must pass with the shrunk table
cargo build --locked --release
scripts/smoke.sh
```

Manual: open the TUI, confirm search/play/queue render (no behavior should
change; this catches accidental type-mismatch fallout).

## Exit criteria

- `spotuify-tui` builds without store/search/sync/player in its dep graph.
- `workspace_boundaries.rs` comments describe only edges that exist.
- No TUI source file imports core types through `spotuify_spotify::client`.

## Dependencies

None. Can ship today.
