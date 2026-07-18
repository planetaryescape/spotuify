# Phase 2 - Derived-Data Robustness (mxr back-ports)

## Goal

Back-port three proven mxr store/search robustness patterns that spotuify's
copy skipped. None of them mention providers; all of them are prerequisites
for shipping the phase-4 schema changes safely.

## Standalone value

Full. These guard against migration bugs, index corruption, and duplicate
mutation application — failure modes spotuify is exposed to today with one
provider.

## Evidence base (mxr, verified 2026-07-16)

1. **Post-migration schema validation.** mxr `store/src/pool.rs:249-260`
   validates a `REQUIRED_COLUMNS` manifest (`pool.rs:810-975`) after
   migrations and refuses to open a malformed/partial DB with a precise
   `missing required column {table}.{column}` error, with a test that a
   hand-broken DB fails to open (`pool.rs:1012-1043`). spotuify has the
   append-only `MIGRATIONS` array (store/lib.rs:3177, version 20) and the
   `CACHE_VERSION` forward-guard (:2007) but nothing validates the *result* —
   an interrupted or buggy migration currently produces a silently wrong DB.
2. **Search index as disposable derived data.** mxr
   `search/src/index.rs:141-190` (`open_with_rebuild_status`): any open
   failure (schema change, tantivy format change, corruption) wipes and
   recreates the index, returning `rebuilt: bool`; the daemon logs the reason
   (`state.rs:1488-1538`) and distinguishes "schema/corruption → rebuild" from
   "lock contention → don't wipe". spotuify's reindex is manual/admin only
   (`Request::Reindex`, search/reindex.rs); a Tantivy schema change today
   requires a human to know to run it. Note spotuify's daemon already clears
   stale `.tantivy-*.lock` files at startup — the lock-vs-corruption
   distinction matters so auto-rebuild doesn't wipe a healthy index under
   contention.
3. **Mutation idempotency keys.** mxr's `apply_mutation(mutation_id, …)` +
   `mutation_dedup_log` with 24h TTL (`store/src/mutation_dedup.rs:15-70`,
   Stripe-style) makes remote mutations retry-safe. spotuify has receipts and
   the ops/undo log but no dedup: a retried playlist-add after a timeout can
   double-apply today (partially masked by queue-dedup semantics, not by
   design).

## Deliverables

1. `REQUIRED_COLUMNS`-style manifest + `validate_schema()` in
   `spotuify-store`, run after `run_migrations`; daemon startup surfaces the
   error verbatim and refuses to serve. Test: break a column, assert refusal.
2. `open_with_rebuild_status` semantics in `spotuify-search`: auto wipe+rebuild
   on open-mismatch, `rebuilt` flag consumed by the daemon and logged as a
   `sync_events`-style event with a reason (`schema_mismatch` /
   `startup_repair`); a guard distinguishing lock contention (existing
   preflight handles that path). Full reindex then repopulates from SQLite via
   the existing `reindex::reindex`.
3. `mutation_id` (UUIDv7 — spotuify already uses uuidv7 for receipts) threaded
   through daemon write-mutation handlers + a `mutation_dedup` table (24h TTL,
   pruned alongside ops-log pruning). Scope: playlist writes, library writes,
   queue batch ops — the surfaces with retry loops.
4. Migration(s) appended to `MIGRATIONS` (version 21+) for the new table.

## Non-goals

No Tantivy schema *changes* here (that's phase 4 — this phase builds the
machinery that makes phase 4's index change a non-event). No provider
anything.

## Verification

```bash
scripts/cargo-nextest -p spotuify-store
scripts/cargo-nextest -p spotuify-search
scripts/smoke.sh
# corrupt-index drill (dev instance):
./target/release/spotuify daemon stop
echo garbage > "$(./target/release/spotuify config path | xargs dirname)/../search_index/meta.json"  # adjust to real path
./target/release/spotuify daemon start   # must self-heal, log reason, serve search
./target/release/spotuify search "known cached term" --format json
```

## Exit criteria

- A deliberately broken DB refuses to open with a named-column error.
- A deliberately corrupted index self-heals on daemon start without human
  action, and search returns cached results afterward.
- A replayed supplied `mutation_id` is a no-op with the original terminal
  receipt/response. Current `IpcClient` callers mint UUIDv7 keys for every
  protected mutation; legacy wire clients that omit the additive field remain
  accepted for compatibility and do not receive replay suppression.

## Dependencies

None on phases 0-1 (parallelizable with them).
