# Phase 10 - Second Adapter & Cross-Provider Identity

## Goal

The only phase that requires a second provider to exist. Two independent
tracks: (a) prove the abstraction with a real second adapter, (b) entity
resolution — "same song, two providers" — which is deliberately quarantined
here because it is the hardest problem and *nothing earlier depends on it*.

**Gate resolved:** D026 chose Spotify-only and explicitly rejected adding any
second music service. The real-adapter, entity-resolution, and aggregation
tracks therefore remain product-gated rather than implementation debt. Reopen
them only after a new product decision supersedes D026. The pre-gate proof
below is the complete authorized scope of this phase.

## Implemented pre-gate proof (2026-07-17)

Deliverable 1 and the provider-authoring documentation are implemented without
choosing or implying a real second service:

- `spotuify-config` accepts two named fake adapter tables with an explicit
  default while preserving each adapter's settings.
- The daemon factory constructs two isolated fake runtimes through the same
  config branch production adapters use. The resulting registry proves
  default selection, distinct URI namespaces, URI routing, and scoped search
  (including different datasets per configured instance).
- The sync integration uses two real `FakeProvider` allocations and proves
  library calls and persisted rows remain in their provider namespaces.
- [Provider Adapter Author Guide](adapter-author-guide.md) documents the
  capability/facet contracts, conformance suite, config/auth/URI rules,
  error/timeout behavior, client obligations, and verification commands.

This is an abstraction proof, not a second adapter. In accordance with D026,
no real provider, entity mappings, cross-provider aggregation, or canonicalized
analytics has been implemented or selected.

## Candidate adapters, ranked by what they prove

1. **FakeProvider #2 configuration** (zero cost, ships with phase 5): two
   fake instances registered simultaneously prove registry routing, scoped
   search, per-provider sync isolation — without any product decision.
2. **Deezer** (best real first target): open REST API, no playback
   (30s previews only), simple OAuth, `GET /track/isrc:{isrc}` lookup
   (verified live). Proves: real second auth flow, capability gating
   (no transport), entity resolution raw material.
3. **Apple Music** (per D026): REST catalog/library/playlist CRUD works;
   $99/yr developer token; user token bootstrapped via browser; no playback.
   Re-validation triggers listed in the study.
4. **Local files** (dark horse; MA treats local as just another provider,
   `is_streaming_provider = false`): no auth at all, full playback via a
   trivial `PlayerBackend`, exercises the one seam Deezer/Apple can't —
   a *second playback path*. Worth considering as the conformance-hardening
   adapter even if never shipped as a feature.

## Entity resolution (the hard 20%)

Adopt Music Assistant's verified schema shape, not a bespoke one:

1. **Tables** (new migrations):
   - `provider_mappings(canonical_uri, provider, provider_item_id, available,
     quality_hint, matched_via, added_at_ms, UNIQUE(provider,
     provider_item_id))` — `available` flips on dead IDs, never deleted;
     `matched_via ∈ {native, external_id, fuzzy, manual}` records confidence.
   - `external_ids(media_type, id_type, id_value COLLATE NOCASE,
     canonical_uri)` — **non-unique on id_value**: ISRC is many-to-many in
     both directions (IFPI; verified against MA's schema which encodes the
     same). UPC/EAN for albums with leading-zero normalization.
2. **Matching cascade** (MA's, verified in source): existing mapping →
   external-id hit *double-checked with metadata compare* → normalized
   name/artist compare. **ISRC counts only with duration within ±8s** (MA's
   gate). Fuzzy tier uses scored fields (beets precedent) so low-confidence
   matches land in a review queue (`matched_via = fuzzy`, surfaced via CLI)
   rather than silently merging.
3. **Analytics fix:** `listen_facts`/metrics keyed on canonical URI once
   mappings exist — the phase-4 "double-counting is a visible known
   limitation" note closes here.
4. **No external dependency:** Odesli public API sunsets 2026-07-31
   (verified); MusicBrainz coverage is ~15% of recordings — matching is
   local-first with provider `isrc` fields (Spotify `external_ids.isrc`,
   Deezer ISRC endpoint, Apple `filter[isrc]`) as the signal source.

## Aggregation rules (from MA, verified)

Multi-provider search fan-out: per-provider soft timeout (slow provider
contributes nothing this query; result cached for next), hard timeout,
provider errors logged and swallowed — the aggregate never fails because one
provider did. Mopidy's slowest-backend-blocks-search is the anti-pattern
(unverified detail, but the design rule stands on MA's implementation alone).

## Deliverables

1. Dual-fake registry configuration + integration tests (routing, scoped
   search, isolation) — ships early, with phase 5 if convenient.
2. One real adapter (product pick) passing `run_provider_conformance`,
   with `[providers.<key>]` config, auth session kind, capability set.
3. Entity-resolution tables + cascade + review-queue CLI
   (`spotuify mappings list/confirm/reject`, dry-run-able per the mutation
   rules).
4. Multi-provider search aggregation with the timeout/error rules.
5. Docs: adapter-author guide generated from the conformance suite (mxr's
   provider-fake doubles as reference implementation — copy that role).

## Verification

```bash
scripts/cargo-nextest -p spotuify-provider-fake -E 'test(dual)'
scripts/cargo-nextest -p spotuify-config -E 'test(dual_fake_config)'
scripts/cargo-nextest -p spotuify-daemon -E 'test(dual_fake_config)'
scripts/cargo-nextest -p spotuify-sync -E 'test(dual_real_fake_sync)'
scripts/cargo-test --workspace && scripts/smoke.sh
# real-adapter drill (whichever ships):
./target/release/spotuify providers list --format json          # two entries + caps
./target/release/spotuify search "track" --provider <new> --format json
./target/release/spotuify playlist create "x" --from candidates.jsonl --provider <new> --dry-run
./target/release/spotuify mappings list --format json           # cascade results with matched_via
./target/release/spotuify analytics top --format json           # no double-counting across mapped pairs
```

## Exit criteria

- Pre-gate: two configured fake instances route and sync independently; the
  adapter-author guide is anchored to the executable conformance suite.

Deferred by D026 (not current exit criteria):

- Second adapter passes conformance; capability gating verified end-to-end
  (transport requests cleanly refused for playbackless provider).
- A track known on both providers resolves to one canonical row with two
  mappings; analytics counts it once.

## Dependencies

Everything: phases 5-9 complete. Gate on the product decision recorded in the
decision log before starting deliverables 2-4.

## Reopening questions

These are intentionally unresolved while D026 remains in force:

1. Second adapter pick: Deezer vs Apple Music vs local-files vs none-for-now?
   (Product call; local-files is the engineering-cheapest playback-proving
   option.)
2. Entity resolution: scored fuzzy tier (beets-style, review queue) accepted
   as scope, or ISRC+duration only for v1?
