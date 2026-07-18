# Phase 4 - Provider Identity in Persistence

## Goal

Give the store and search index a real provider discriminator. Minimal shape:
one `provider` column derived from the URI scheme, `spotify_id` generalized,
the `source`-column overload untangled. **Not** the Music-Assistant
`provider_mappings` schema — that's entity resolution, deliberately deferred
to phase 10.

## Standalone value

Moderate. Untangling `source` (search provenance) from provider identity and
dropping the redundant `spotify_id` column are cleanups on their own; the
`provider` column itself is only useful if the abstraction proceeds. This is
the first phase whose main payoff is provider-specific — it sits here because
phases 2's machinery (schema validation, index auto-rebuild) makes it cheap
and safe, and phase 1 made URIs parseable in one place.

## Evidence base

- URIs are PKs/FKs in ~15 tables (`media_items.uri` PK store/lib.rs:2572;
  `playlist_items.item_uri` FK :2626; `library_items` :2643; `lyrics_cache`
  :2862; `track/artist/album_metrics` :2753-2773; `listen_facts` :2724 …).
  **Because `spotify:track:X` already parses as `<provider>:<kind>:<id>`
  (phase 1), no key changes** — rows keep their PKs; the provider column is
  derivable metadata.
- `media_items.spotify_id` (:2574) — redundant vendor column (bare id,
  recoverable from the URI); mirrored in the Tantivy schema
  (search/lib.rs:400).
- `media_items.source TEXT DEFAULT 'spotify'` (:2581) is **search-route
  provenance** (`SearchSourceData { Local, Spotify, Hybrid }`), not provider
  identity; store/lib.rs:2507 hardcodes it. Overloading it would corrupt
  search semantics — it needs a rename, not reuse.
- Two-provider coexistence danger from the audit: `media_items` rows do not
  collide (a different-scheme URI is a different PK) — they'd silently
  cross-contaminate search and analytics, double-counting one song under two
  URIs. The provider column enables scoped queries; *dedup* stays out of scope
  until phase 10. The legacy `playlists.id` bare-ID PK is an exception and
  remains Spotify-only until phase 10's identity migration.
- mxr precedent: `UNIQUE(account_id, provider_id)` as the multi-tenant upsert
  invariant (`store/migrations/001_initial.sql:38-59`); Music Assistant keeps
  region/availability on the mapping row, not the canonical row.

## Deliverables

1. **Migrations (append to `MIGRATIONS`, version N+):**
   - `media_items.provider TEXT NOT NULL DEFAULT 'spotify'` + index; backfill
     `UPDATE media_items SET provider = substr(uri, 1, instr(uri,':')-1)`.
   - Rename `source` → `search_origin` (rebuild or add-and-migrate; SQLite
     rename-column is fine at our floor version). All read/write sites updated
     (`upsert_media_items`, store/lib.rs:188; `search_runs.source` :2656 keeps
     its name — it genuinely is search provenance).
   - Drop `spotify_id` from `media_items` (recoverable; `row_to_media_item`
     :2335 derives `MediaItem.id` from the URI via phase-1 parsing).
   - `lastfm_import`'s `resolved_spotify_uri` (:3125) → `resolved_uri`.
2. **Tantivy schema:** replace `spotify_id` field with `provider` STRING and
   rename search provenance `source` to `search_origin`
   (search/lib.rs:400, populated at :299). This is a breaking index change —
   phase 2's auto-rebuild makes it a silent startup reindex instead of a
   support incident. Verify doc counts post-rebuild (mxr checks
   `num_docs()` after reindex — copy that assertion).
3. **Store API:** query functions gain optional provider scoping
   (`list_media_for_index`, library/metrics queries) — default `None` = all,
   preserving current behavior exactly.
4. **Analytics guard:** metrics tables keep URI keys (provider is derivable);
   add a doc-comment + test asserting per-provider partitioning of
   `analytics top` output so cross-provider double-counting is a *visible*
   known limitation until phase 10, not a silent one.

## Non-goals

No `provider_mappings`, no `external_ids`, no ISRC anything (phase 10). No
protocol changes (`SearchSourceData` rename is phase 8 — the store column
rename here is internal). No playlist primary-key migration: playlist cache
identity remains Spotify-only until phase 10, when a real second adapter makes
the correct canonical/mapping shape testable.

## Verification

```bash
scripts/cargo-nextest -p spotuify-store
scripts/cargo-nextest -p spotuify-search
scripts/smoke.sh
# live migration drill on the dev instance (real cache, not fixtures):
./target/release/spotuify daemon stop && ./target/release/spotuify daemon start
./target/release/spotuify logs path | xargs tail -50 | grep -v tantivy   # expect one auto-reindex, reason logged
./target/release/spotuify search "known term" --format json              # results identical to pre-migration capture
./target/release/spotuify analytics top --format json                    # identical to pre-migration capture
sqlite3 <cache.db> "SELECT provider, COUNT(*) FROM media_items GROUP BY 1"  # all 'spotify'
```

## Exit criteria

- `media_items` has `provider`; no table or index field is vendor-named.
- Pre/post search + analytics outputs byte-identical (modulo run metadata).
- Fresh-install path (no existing DB) produces the same schema as
  migrated path — assert with the phase-2 schema validator.

## Dependencies

Phases 1 (URI parsing) and 2 (schema validation + index auto-rebuild).
