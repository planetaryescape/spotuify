# spotuify - Provider Abstraction Plan

Phased plan to make Spotify one adapter behind a provider abstraction, so a
second provider (Apple Music, Deezer, local files, …) is an adapter + config
table, not a rewrite. Planned 2026-07-16 from a five-agent audit (three
codebase mappers, two mxr mappers) plus source-verified external research
(Music Assistant, OwnTone, Strawberry, Navidrome, OpenDAL, termusic,
ListenBrainz). Evidence lives in [00-current-state.md](00-current-state.md).

**Ordering principle (deliberate):** phases 0-3 are worth shipping even if the
abstraction is abandoned; 4-9 build the abstraction against the existing
single provider; only phase 10 needs a second provider. D026 resolved that gate
as Spotify-only, so phase 10 stops after the dual-fake abstraction proof unless
a later product decision explicitly selects a real adapter.

**Status:** complete within the Spotify-only scope authorized by D026. The
completion record, verification evidence, compatibility limits, and deliberate
deferrals are recorded in
[D029](../blueprint/13-decision-log.md#d029-provider-abstraction-phases-complete-within-d026-scope-2026-07-17).

## Documents

| # | Document | One-line | Standalone value |
|---|---|---|---|
| 00 | [Current State](00-current-state.md) | Audit evidence: every seam, leak, and mxr/external precedent | — |
| 01 | [Phase 0 - Boundary Hygiene](01-phase-0-boundary-hygiene.md) | Dead deps, vendor-crate type imports, stale boundary docs | Full |
| 02 | [Phase 1 - One URI Module](02-phase-1-uri-module.md) | ~40 URI string-op sites → one typed core module | Full |
| 03 | [Phase 2 - Derived-Data Robustness](03-phase-2-derived-data-robustness.md) | mxr back-ports: schema validation, index auto-rebuild, mutation idempotency | Full |
| 04 | [Phase 3 - Neutral Core Model](04-phase-3-neutral-core-model.md) | The internal semantic schema: version tokens, typed dates/enums, neutral docs | High |
| 05 | [Phase 4 - Provider Identity in Persistence](05-phase-4-provider-identity-persistence.md) | `provider` column, `spotify_id` dropped, `source` overload untangled | Moderate |
| 06 | [Phase 5 - The Provider Trait](06-phase-5-provider-trait.md) | `MusicProvider` trait, registry, FakeProvider crate, conformance harness | High |
| 07 | [Phase 6 - Sync Decoupling](07-phase-6-sync-decoupling.md) | Engine takes `&dyn`, opaque freshness tokens, typed resync | Moderate-high |
| 08 | [Phase 7 - Auth & Config](08-phase-7-auth-config.md) | Daemon-side OAuth sessions, `[providers.*]` config, instance-scoped creds | Moderate-high |
| 09 | [Phase 8 - Wire & Clients](09-phase-8-wire-and-clients.md) | Provider scope on IPC, capability gating, boundary rule finally enforced | Low-moderate |
| 10 | [Phase 9 - Player Capabilities](10-phase-9-player-capabilities.md) | Mercury/token leaks out of `PlayerBackend`; playbackless providers first-class | Low |
| 11 | [Phase 10 - Second Adapter & Identity](11-phase-10-second-adapter.md) | Dual-fake proof complete; real adapter + identity work deferred by D026 | — |
| — | [Provider Adapter Author Guide](adapter-author-guide.md) | Executable contract for implementing and wiring an adapter | Reference |

## Design pillars (argued in the phase docs)

1. **Canonical model + provider extensions, not lowest-common-denominator**
   (mxr: "provider quirks stay below the core mail model"). Spotify's
   `snapshot_id` survives as an opaque `version_token`; Mercury survives as a
   Spotify-adapter extra.
2. **Split traits:** `MusicProvider` (catalog/library/playlists) /
   `RemoteTransport` (Connect-shaped, optional) / `PlayerBackend` (exists,
   optional per provider). Music Assistant's music-vs-player split at
   120-provider scale; Strawberry's precedent that Spotify playback
   legitimately bypasses generic resolution.
3. **`async_trait` + `Arc<dyn>` registry** — workspace idiom and mxr's
   pattern; the termusic-style enum was considered and rejected for
   consistency (boxing is noise against network latency).
4. **Capabilities as plain namespaced structs** (OpenDAL shape, mxr
   namespacing), semantic meaning, default-false, additive on the wire;
   daemon gates before dispatch, adapters also return typed `Unsupported`.
5. **Opaque tokens for anything provider-versioned** (cursors, freshness,
   playlist versions) — mxr's biggest recorded lesson.
6. **Identity: URIs already are `<provider>:<kind>:<id>`** — the existing
   `spotify:` scheme slots in unchanged; no PK migration ever.
7. **ISRC is a signal, never a key** (many-to-many both directions; MA gates
   it with ±8s duration). Entity resolution is quarantined in phase 10.
8. **Conformance harness over HTTP mocks** — every adapter (fake included)
   passes `run_provider_conformance`; the 39 `if self.fake` branches die.

## Rules for implementers

- Every phase keeps `scripts/smoke.sh` + the CLI drills in its doc green
  before merge; playback verification is live, not test-only (player first).
- Wire changes are additive with serde defaults; `IPC_PROTOCOL_VERSION` bumps
  only on true breaks (mxr doctrine).
- Copy from mxr `crates/provider-fake/` + `crates/core/src/provider.rs` —
  **never** from its stale `examples/adapter-skeleton/` or blueprint §03 code
  samples.
- Store migrations are append-only; pair every index-affecting change with
  the phase-2 auto-rebuild machinery.
- Record phase completion + any deviation in
  `docs/blueprint/13-decision-log.md`.

Reopening questions live at the end of [phase 10](11-phase-10-second-adapter.md).
