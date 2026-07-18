# Phase 1 - One URI Module

## Goal

Replace ~40 scattered URI string operations across 12 crates with a single
typed URI module in `spotuify-core`. Still Spotify-only semantics — but one
place owns parsing, formatting, and kind dispatch, so the provider
discriminator later lands in one file instead of forty.

## Standalone value

Full. This kills an entire bug class (inconsistent parsing: some sites accept
`spotify:user:…:playlist:…` legacy shapes, some don't; some strip `?si=`
tracking params, most don't) and gives every future URI-format decision a
single home. Worth doing even if no second provider ever ships.

## Evidence base

The audit found the `<scheme>:<kind>:<id>` shape hand-parsed at:

- **The de-facto dispatcher:** `media_kind_from_uri`
  (`crates/spotuify-spotify/src/selection.rs:35-58`) — six `starts_with`
  branches; 13 callers across spotify/daemon/cli crates (actions.rs:152,
  client.rs:2819, selection.rs:89,115,166,184, handler.rs:1106,1248,3259,3265,
  handlers/playlists.rs:228, handlers/playback.rs:608, commands.rs:1320).
- **`rsplit(':')` bare-id extraction** (~15 sites): store/lib.rs:4114,
  client.rs:2814, mercury.rs:104, handler.rs:3260,3266,3295,3920,
  queue_warm.rs:324, state.rs:3093, handlers/library.rs:178,
  commands.rs:1979, ui.rs:5069, app.rs:6412,8548, agent_playlists.rs:496.
- **`format!("spotify:…")` builders** (~15 sites): mercury.rs:48,150,
  client.rs:822,1026,2664, selection.rs:88,114,122, store/lib.rs:2519,
  sync_loop.rs:948, commands.rs:1328, output.rs:2878, bridge.rs:290,
  app.rs:619,1248,4996,5009, ids.rs:52.
- **`starts_with`/`strip_prefix` scatter:** client.rs:3071-3075,
  store/lib.rs:2516, sync_loop.rs:945, embedded/mod.rs:759-762,
  mercury.rs:41,101,145, notifications.rs:239, queue_warm.rs:184,
  handlers/library.rs:149, bridge.rs:287, spotify_provider.rs:7,
  handler.rs:1113,1121, client.rs:928,934,970,1016,1040.
- **`ids.rs` is near-dead:** the four typed newtypes have exactly one non-test
  caller (client.rs:594). Redesign freely.

## Design

New module `spotuify-core/src/uri.rs`:

```rust
/// Canonical resource identifier. Today the only scheme is "spotify";
/// the parsed shape is deliberately provider-ready: <scheme>:<kind>:<id>.
pub struct ResourceUri {
    scheme: UriScheme,   // enum, today: Spotify (non_exhaustive)
    kind: MediaKind,
    id: String,          // bare id, no colons
}

impl ResourceUri {
    pub fn parse(s: &str) -> Result<Self, UriError>;   // strict
    pub fn kind(&self) -> MediaKind;
    pub fn bare_id(&self) -> &str;
    pub fn as_uri(&self) -> String;                    // canonical form
}
```

Notes:

- The existing `spotify:track:X` shape **is already** `<provider>:<kind>:<id>`
  — parsing is unchanged, no stored data migrates, PKs stay stable. This is
  the lucky break that makes phase 4 cheap.
- `MediaKind` already exists in core; `media_kind_from_uri` becomes a thin
  wrapper over `ResourceUri::parse` and then gets inlined away at call sites.
- `normalize_spotify_target` (selection.rs:67 — `open.spotify.com` URLs,
  legacy `spotify:user:…` shapes, `?si=` stripping) **stays in the spotify
  crate**: it is provider-specific *input* normalization. It changes to emit a
  `ResourceUri`. Every provider later gets its own input normalizer; the core
  type is what they normalize *to*.
- The four `ids.rs` newtypes are reimplemented on top of `ResourceUri` (or
  deleted in favor of it — decide at implementation time based on the single
  caller's needs).
- Mercury gid conversion (`artist_gid_from_uri`) stays in mercury.rs — it's
  Spotify-wire-format, below the seam.

## Deliverables

1. `spotuify-core/src/uri.rs` with exhaustive unit tests (all six kinds,
   reject garbage, reject empty id, round-trip).
2. Mechanical migration of every site listed above to the typed API. One PR
   per crate-cluster is fine; each keeps tests green independently.
3. A regression gate: a workspace test (alongside `workspace_boundaries.rs`)
   that greps non-core crates for `rsplit(':')`-on-uri and
   `format!("spotify:` patterns and fails on new occurrences outside an
   allowlist (mercury.rs, normalize_spotify_target, fixtures).
4. `ids.rs` folded into the new module; its one caller updated.

## Verification

```bash
scripts/cargo-nextest -p spotuify-core -E 'test(uri)'
scripts/cargo-test --workspace
scripts/smoke.sh
# behavior spot-checks through the real CLI:
./target/release/spotuify search "test" --format ids
./target/release/spotuify play "spotify:track:<id>"
./target/release/spotuify playlist play "https://open.spotify.com/playlist/<id>?si=junk"
```

The URL-with-tracking-param case is the canary: it exercises
normalize → ResourceUri → daemon → playback end to end.

## Exit criteria

- Grep gate passes: no URI string-ops outside `core/src/uri.rs` + allowlist.
- All 13 `media_kind_from_uri` call sites use the typed API.

## Dependencies

Phase 0 (so TUI sites import core types directly).
