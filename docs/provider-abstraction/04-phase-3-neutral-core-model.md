# Phase 3 - Provider-Neutral Core Model

## Goal

Make `spotuify-core` the provider-agnostic language of the app (mxr's phrase,
from its blueprint: "provider quirks stay below the core mail model"). Rename
and re-type the fields where Spotify response semantics leak through core
types, so the canonical model describes *music concepts* and adapters map into
it.

This is the "internal semantic schema" phase: canonical model + provider
extensions, **not** lowest-common-denominator. Fields that earn their keep
(the playlist version token gating sync) stay — as neutral concepts with
Spotify as one producer of them.

## Standalone value

High. Typed instead of stringly (`repeat: String` → enum, raw date strings →
parsed), documented semantics instead of "whatever Spotify returned". Removes
a class of "what format is this field?" bugs regardless of providers.

## Evidence base

`crates/spotuify-core/src/lib.rs`:

- `MediaItem.release_date` (:293) — "Spotify's `YYYY-MM-DD` string" passed
  raw. Apple/Deezer use different precisions/formats.
- `MediaItem.album_group` (:298) — Spotify vocabulary
  (album|single|compilation|appears_on) as a bare string.
- `MediaItem.resume_position_ms`/`fully_played` (:287-290) — "from Spotify's
  `resume_point`", fine concepts, Spotify-named docs.
- `MediaItem.source` (:270) — free-form provenance ("spotify"/"mercury"/
  "local"), undocumented contract.
- `Playlist.snapshot_id` (:343) — "Spotify's playlist-version token"; gates
  sync refetch (`sync/lib.rs:165-177`).
- `Playback.provider_timestamp_ms` (:45), `PlaybackStateSource::WebApiPoll`
  (:61-76) — Web-API-named.
- `SpotifyClient::repeat(&str)` (client.rs:1219) — stringly; `RepeatMode`
  already exists in `spotuify-player` (lib.rs:89).
- `SavedTracksPage` (client.rs:51) — the one spotify-crate-owned public return
  type; it's just a page.
- mxr precedent for the version-token move: `SyncCursor` began as a tagged
  union the daemon matched on and was refactored to opaque bytes — their
  self-audit calls it the biggest lesson (`docs/msp/mxr-alignment.md:75-113`).

## Deliverables

1. **Version token:** `Playlist.snapshot_id` → `version_token: Option<String>`
   (opaque; adapters fill it or don't). Sync gate reworked to "refetch when
   token absent or changed" (`should_refetch_playlist_tracks` already
   fail-open — keep that). Serde alias `snapshot_id` retained for wire compat;
   the private SQLite `snapshot_id` column may retain its adapter-era name
   until playlist identity is generalized in phase 10.
2. **Release date:** `ReleaseDate { year, month: Option, day: Option }` parsed
   at the adapter boundary; core stores the struct. Display formatting via
   one impl, not per-client string slicing.
3. **Album grouping:** `AlbumGroup` enum (Album, Single, Compilation,
   AppearsOn, Other(String) for forward-compat) — adapter maps Spotify's
   strings.
4. **Provenance:** `MediaItem.source` documented + typed as
   `ItemSource { Provider(String), Mercury, Local }` or tightened doc-comment
   contract (decide at impl; wire compat via serde). Groundwork for phase 4's
   provider column — not the provider discriminator itself.
5. **Repeat/playback:** `RepeatMode` moves to core; `SpotifyClient::repeat`
   and the protocol take the enum. `PlaybackStateSource::WebApiPoll` renamed
   provider-neutral (`RemotePoll`) with serde alias.
6. **Pagination:** `SavedTracksPage` → generic `Page<T>` in core.
7. Doc-comment sweep in core + protocol: Spotify-specific field docs rewritten
   as neutral contract + "Spotify adapter: maps from X" notes.

## Wire compatibility

Every rename ships with `#[serde(alias = …)]` (read old) and — where clients
serialize — `rename` staging so old daemons/new clients coexist per mxr's
additive-evolution doctrine (`protocol/lib.rs:18-24` equivalent). Bump
`IPC_PROTOCOL_VERSION` only if a shape genuinely breaks; the goal is zero
bumps in this phase.

## Verification

```bash
scripts/cargo-test --workspace
scripts/smoke.sh
# wire-compat drill: run the RELEASED binary's TUI against the dev daemon
SPOTUIFY_ALLOW_PROD_INSTANCE_FROM_TARGET=1 spotuify status   # released CLI, dev daemon
./target/release/spotuify status --format json | jq .        # new fields present
./target/release/spotuify playlists --format json | jq '.[0]'
```

Analytics regression: `spotuify analytics top --format json` before/after must
match (field semantics unchanged, only names/types).

## Exit criteria

- `grep -ri "spotify" crates/spotuify-core/src/lib.rs` hits only adapter-note
  doc comments (target: <10, from 93).
- Old-client JSON (captured fixture) still deserializes.

## Dependencies

Phase 1 (URI module owns the identity half of the model first).
