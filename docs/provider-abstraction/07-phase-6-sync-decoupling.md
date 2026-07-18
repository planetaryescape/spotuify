# Phase 6 - Sync Engine Decoupling

## Goal

Break `spotuify-sync`'s concrete dependency on `SpotifyClient` and its
Spotify-shaped freshness gates. The engine ends up holding only store+search
and taking `&dyn MusicProvider` per call, with provider freshness state as an
opaque token — mxr's exact architecture, which is the proven fix for exactly
this coupling.

## Standalone value

Moderate-high: the sync engine becomes testable against the FakeProvider
(today it needs the fake *client* bool or live network), and cursor/version
handling gets a principled home instead of per-domain special cases.

## Evidence base

- `SyncContext::spotify_client() -> anyhow::Result<SpotifyClient>`
  (sync/lib.rs:52) — the single biggest concrete coupling in the workspace.
- Spotify assumptions in the engine: `should_refetch_playlist_tracks` on
  `snapshot_id` (lib.rs:165-177), `should_refetch_saved_tracks` on Spotify's
  `(total, page-0 ids)` shape (:194-204), 403 handling by matching
  `endpoint.starts_with("GET /playlists/")` (sync_loop.rs:845-854).
- mxr (verified): engine method `sync_account_with_outcome(&self, provider:
  &dyn MailSyncProvider)` (engine.rs:229-232); `SyncCursor(pub Vec<u8>)`
  opaque, daemon persists blindly, adapter owns encoding + private versioning
  (core/types.rs:1834-1859; Gmail's versioned cursor with legacy-shape shim,
  provider-gmail/cursor.rs:23-171); centralized cursor-expiry recovery — reset
  once, re-run full (engine.rs:264-289); capabilities drive engine behavior
  instead of provider type-switches (skip local rethread when
  `native_threading`, engine.rs:475-478). Their self-audit names the
  tagged-union-cursor → opaque-bytes refactor as the design's biggest lesson
  (mxr-alignment.md:75-113).

## Design

1. **`SyncContext` loses `spotify_client()`.** The daemon passes
   `Arc<dyn MusicProvider>` into sync entry points; the trait keeps its
   provider-agnostic hooks (clock, snapshot writers, `index_media_items`).
2. **Freshness tokens go opaque.** The provider trait (phase 5) exposes:
   - `playlist_version(&Playlist) -> Option<&str>` — already neutral after
     phase 3's `version_token`.
   - `library_freshness_probe() -> FreshnessProbe` — an adapter-owned opaque
     value (Spotify: `(total, page-0 ids)` serialized; stored as bytes like
     mxr's cursor) with `fn changed(&prev) -> bool` semantics owned by the
     adapter. The engine's rule shrinks to: *no token or changed token →
     refetch* (fail-open, as today).
3. **Typed resync signal.** `ProviderError::SyncTokenExpired { reason }`;
   engine resets state and re-runs full sync once per cycle (mxr's guard).
   Kills the endpoint-string 403 matching — the *adapter* classifies its own
   errors.
4. **Capability-driven scheduling.** Cadence table keys off
   `ProviderCaps` (e.g. no `transport` caps → skip playback/queue/devices
   targets for that provider) instead of assuming every provider has all
   seven `SyncTargetData` domains.
5. **Per-provider task isolation** (mxr loops.rs:74-116,301-336): one sync
   task per provider with a detach timeout, so a wedged provider can't stall
   the rest. With one provider this is a no-op structurally — but it's the
   shape multi-provider needs and it hardens today's single loop against
   wedges.

## Deliverables

1. `SyncContext` trait rework + daemon wiring (state.rs impl).
2. Opaque `FreshnessProbe`/token plumbing; Spotify adapter encodes its
   existing snapshot/page-0 logic behind it; store keeps persisting via
   `sync_cursors` (domain-keyed table already exists, store/lib.rs:2682).
3. `SyncTokenExpired` + centralized recovery; delete endpoint-string
   matching.
4. Engine integration tests against FakeProvider: full-sync, delta-skip on
   unchanged token, expiry-recovery, capability-skipped domains.

## Verification

```bash
scripts/cargo-nextest -p spotuify-sync
scripts/smoke.sh
# live drill (dev daemon, real account):
./target/release/spotuify daemon stop && ./target/release/spotuify daemon start
sleep 90 && ./target/release/spotuify logs path | xargs grep -E 'sync' | tail -30 | grep -v tantivy
# expect: playlists synced with token-skip lines for unchanged playlists
./target/release/spotuify playlists --format json | jq length   # matches Spotify reality
```

## Exit criteria

- `spotuify-sync` has no `spotuify-spotify` dependency (boundary test
  tightened — this is the moment crate-dep rule 6 becomes true as documented).
- Engine tests run network-free against the fake.

## Dependencies

Phases 3 (version_token) and 5 (trait + FakeProvider).
