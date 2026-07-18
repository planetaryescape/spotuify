# Phase 5 - The Provider Trait

## Goal

Extract a `MusicProvider` trait from `SpotifyClient`, make the Spotify client
its first adapter, replace the 39 `if self.fake` branches with a real
`FakeProvider` implementation, and ship a conformance harness every adapter
must pass. This is the heart of the abstraction.

## Standalone value

High even without provider #2: the fake becomes an honest trait impl (mxr
proved this is strictly better than bool-flag fixtures — theirs backs demo
mode, smoke tests, and serves as the reference implementation for adapter
authors), and the daemon's 53 concrete call sites become injectable, which
unlocks real integration tests without network.

## Design decisions (with rationale)

1. **Split traits, not one god-trait.** mxr rejected the unified trait
   explicitly ("forces SMTP to implement sync methods it can't support" —
   blueprint 03-providers.md:7-13); Music Assistant's MusicProvider vs
   PlayerProvider split is the same call at ~120-provider scale. For spotuify:
   - `MusicProvider` — catalog search/lookup, library read/write, playlist
     read/write. (One trait, capability-gated internally: unlike mail
     sync-vs-send, these share auth/session state.)
   - **Playback stays on `PlayerBackend`** — it already exists, already has a
     mock, already actor-injected. Strawberry is precedent that one provider's
     playback legitimately bypasses generic resolution (its `spotify:` URIs go
     straight to GStreamer/librespot below the abstraction, exactly like our
     Spirc path). Remote transport control (the Web-API playback methods on
     SpotifyClient) becomes a third, optional trait: `RemoteTransport`
     (play/pause/seek/volume/shuffle/repeat/queue/devices/transfer) — this is
     Spotify-Connect-shaped and a provider without it (Apple Music) simply
     doesn't implement it.
2. **`#[async_trait]` + `Arc<dyn>`, not the enum.** Research says a
   hand-rolled enum (termusic-style) is the 2026-optimal answer for a closed
   set — but async_trait + trait objects is the established workspace idiom
   (`PlayerBackend`, `AnalyticsSink`, `WebApiBearerProvider`,
   `SyncContext`), it's what mxr ships, and boxing overhead is noise against
   network calls. Consistency and copy-mxr both point one way. Revisit only if
   a middleware layer ever needs zero-cost static dispatch (OpenDAL's
   mirror-trait trick or `dynosaur` are the escape hatches; `enum_dispatch`
   does not support AFIT).
3. **Capabilities = plain struct of named bools + `Option<usize>` limits,
   namespaced.** OpenDAL's shape (not bitflags — allows limits like
   `playlist_add_max_batch: Option<usize>`, diffs well in logs), mxr's
   namespacing (they refactored away from flat "boolean soup"), MA's
   per-media-type granularity:

   ```rust
   pub struct ProviderCaps {
       pub search: SearchCaps,      // remote: bool, kinds: …
       pub library: LibraryCaps,    // read: bool, write: bool, per-kind …
       pub playlists: PlaylistCaps, // read, create, edit, image, version_token
       pub transport: Option<TransportCaps>, // None = no remote transport
   }
   ```

   All fields default-false / serde-additive. Capability meaning is
   *semantic*, mxr's rule: `playlists.edit == false` does not mean "playlists
   don't exist" — it means callers must not assume mutable playlist
   semantics.
4. **Unsupported = typed error, checked at the daemon.** Trait methods get
   default impls returning `ProviderError::Unsupported` (the `PlayerBackend`
   pattern, player/lib.rs:216-234); the daemon *also* gates on caps before
   dispatch so clients get "provider X can't do Y" rather than a deep error
   (mxr does both, layered).
5. **Registry:** `HashMap<ProviderId, Arc<dyn MusicProvider>>` +
   `default_provider`, built by a factory from config (mxr
   state.rs:19-29,578-871). With one provider it's a one-entry map — the
   point is that `state.spotify_client()`'s 53 callers change to
   `state.provider(scope)?` once, now, while there's only one answer.
6. **Errors:** `ProviderError` in core — `Unsupported`, `RateLimited
   { retry_after }`, `AuthExpired`, `NotFound`, `VersionConflict` (playlist
   token mismatch), `Provider(String)`. `SpotifyError` maps into it inside the
   adapter. The rate-limit machinery (`rate_limit.rs` — structurally generic
   per the audit) moves to a shared crate the adapter uses; each adapter
   supplies its own scope-naming and error classification.

## The mutation surface

Adopt mxr's consolidated mutation shape for writes:
`apply_mutation(mutation_id: Uuid, &Mutation) -> Result<MutationReceipt>`
where `Mutation` is an enum (PlaylistAdd/Remove/Reorder, LibrarySave/Unsave,
Follow/Unfollow, PlaylistCreate, …). Rationale: mxr collapsed four write
methods into this and gained one choke point for durable daemon-side dedup,
ops-log, and undo recording. The daemon/store mutation claim keyed by
`mutation_id` is the idempotency authority. Adapters receive that ID only for
correlation and best-effort replay suppression; a remote provider cannot
guarantee exactly-once behavior after an ambiguous timeout. Reads stay as
individual methods — they don't need receipts.

## FakeProvider + conformance (the payoff)

- New crate `crates/spotuify-provider-fake`: implements `MusicProvider` (+
  `RemoteTransport`) over in-memory fixtures. Fixture selection via env
  (`SPOTUIFY_FAKE_DATASET`, mxr's `MXR_FAKE_DATASET` pattern) — the existing
  `SPOTUIFY_FAKE_SPOTIFY=1` env contract keeps working: the daemon factory
  maps it to the fake provider. The 39 `if self.fake` branches and the inline
  fixtures (client.rs:2887-3063) are **deleted**.
- `run_provider_conformance<P: MusicProvider>(&P)` in the fake crate (mxr
  conformance.rs pattern): search returns items with parseable URIs,
  library round-trips, playlist create→add→remove→version-token advances,
  capability-gated ops only asserted when declared, every mutation is
  observable and returns the supplied receipt correlation ID. Generic provider
  conformance does not replay writes: durable replay behavior belongs to daemon
  conformance, while adapters may only offer best-effort suppression. Run by:
  fake (self), Spotify adapter (mockito-backed), and every future adapter.
- **Copy from mxr's `crates/provider-fake/` + `core/src/provider.rs` — not
  from its `examples/adapter-skeleton/` or blueprint code samples, both of
  which are stale pre-refactor shapes** (audit finding).

## Migration path for the 53 call sites

Mechanical once the trait exists, staged to keep every commit green:

1. Land trait + caps + errors in core; implement on `SpotifyClient` (methods
   already return core types — the audit confirms the surface maps 1:1 onto
   the trait minus the leaks handled in phases 3/9).
2. `state.spotify_client()` keeps its signature but gains a sibling
   `state.provider()` returning `Arc<dyn MusicProvider>`; port handler files
   one at a time (handler.rs's 22 sites last), including the `actions.rs`
   dispatcher — `execute(client, CommandKind)` becomes
   `execute(provider, CommandKind)` and moves out of the spotify crate into
   the daemon or a new `spotuify-provider` crate.
3. Delete `state.spotify_client()` when zero callers remain; the concrete
   type stops being nameable outside its crate + factory.

## Verification

```bash
scripts/cargo-nextest -p spotuify-provider-fake      # conformance vs fake
scripts/cargo-nextest -p spotuify-spotify            # conformance vs mockito-backed adapter
scripts/cargo-test --workspace
scripts/smoke.sh                                     # fake provider path — now a real impl
SPOTUIFY_LIVE_API=1 scripts/smoke.sh                 # opt-in live check, once, pre-merge
# real-daemon spot checks (dev instance):
./target/release/spotuify search "x" --format json
./target/release/spotuify playlist create "trait-test" --dry-run
./target/release/spotuify ops undo --dry-run
```

## Exit criteria

- `SpotifyClient` is not nameable outside `spotuify-spotify` + the daemon
  factory (boundary test tightened).
- Zero `if self.fake` branches; `scripts/smoke.sh` green on the fake crate.
- Conformance suite passes for fake + Spotify adapter.

## Dependencies

Phases 1 (URIs), 2 (dedup log for mutation_id), 3 (neutral types in
signatures). Phase 4 can land before or after.
