# Phase 9 - Player Trait Capability Split

## Goal

Remove the Spotify leaks from `PlayerBackend` and define what playback means
for a provider that has none. The trait is already the workspace's best seam
(real second impl, actor-injected, Spirc contained in one file) — this phase
finishes it rather than rebuilding it.

## Standalone value

Low standalone; this is abstraction-completion work. The exception:
`mercury_get` untangling gives lyrics/discovery a principled home, which pays
off whenever librespot/Mercury changes upstream (a live concern — the pinned
fork, the Mercury deprecation risk tracked in
`docs/maintenance/librespot-fork.md`).

## Evidence base

- The three trait leaks (player/lib.rs): `mercury_get` (:253 — Spotify's
  proprietary `hm://` bus; consumers: lyrics via handler.rs:529, discovery via
  handlers/library.rs:135,156), `web_api_token` (:240 — login5 minting,
  state.rs:170 notes it's currently mostly unused), `&str` URIs +
  `PlayContextRequest` Spirc-flavored comments (:129), `PlayerError::
  PremiumRequired`/`NoActiveDevice` variants (:140).
- `DeviceTransfer` runs over Web API + selection helpers
  (handlers/playback.rs:347-415), not the trait — transfer is a Spotify
  Connect concept with undo support (`ReversalPlan::TransferToPriorDevice`).
- Token flows both directions between daemon and backend
  (player_factory.rs:20-36 `DaemonTokenProvider`; embedded/mod.rs:228-292)
  — Spotify-shaped seams, fine *inside* the adapter pairing.
- Music Assistant (verified): content vs output fully decoupled — its Spotify
  music provider streams via bundled librespot while `spotify_connect` is a
  separate player provider. Strawberry: `spotify:` playback legitimately
  bypasses the generic URL-handler registry. OwnTone: NULL vtable slot =
  unsupported, benign degradation (missing seek returns "not seekable", not
  an error).
- The apple-music-feasibility study (D026): a future provider may have
  **no playback at all** — the design must make "metadata-only provider"
  a first-class capability state, not an error storm.

## Design

1. **`mercury_get` leaves `PlayerBackend`.** It becomes a Spotify-adapter
   extension: the lyrics and discovery consumers route through a
   `ProviderExtras`-style optional interface on the *provider* (or a
   downcast hook the Spotify adapter exposes), not the generic player trait.
   Lyrics already has the right shape to absorb this: `LyricsProvider {
   SpotifyMercury, Lrclib }` (core/lib.rs:349) — SpotifyMercury just becomes
   an adapter-supplied source, and LRCLIB (provider-independent) is the
   fallback every provider gets for free.
2. **`web_api_token` leaves the trait.** It exists for the first-party
   bearer bridge; that wiring stays private between the Spotify adapter and
   the embedded backend (they're a pair — MA's librespot-inside-the-Spotify-
   provider proves the pairing is legitimate). The
   daemon-visible surface is `WebApiBearerProvider`, which already abstracts
   it.
3. **Pairing, not universality:** a provider *optionally supplies* a
   `PlayerBackend` (Spotify → embedded librespot; Apple Music → none;
   future Deezer → none until someone writes one). `player_factory` consults
   the provider registry; `BackendKind` (the one-variant decoy enum) is
   subsumed by this and deleted. No playback capability →
   `TransportCaps: None` (phase 5) → daemon rejects transport requests for
   that provider with a clean capability error; queue/devices surfaces for it
   render empty-with-reason, not error toasts (OwnTone's benign-degradation
   rule).
4. **Device semantics stay Connect-scoped.** `DeviceTransfer`, device lists,
   and the watchdog remain Spotify-adapter behavior surfaced through
   `RemoteTransport` (phase 5). No pretense of a generic device model until a
   second provider with devices exists — inventing one now would be
   speculation (and AirPlay is an output transport, not a device protocol —
   see the feasibility study).
5. **Typed URIs in the trait:** `play_uri(&ResourceUri, …)` etc., closing
   phase 1's loop. `PlayerError::PremiumRequired` moves behind a generic
   `ProviderPolicy(String)` variant. The daemon emits a provider-tagged,
   redacted `DaemonEvent::ProviderPolicy`; the released
   `premium-required` wire event remains decode-only compatibility.

## Deliverables

1. Trait cleanup (remove `mercury_get`/`web_api_token`, typed URIs, error
   rename) + `MockPlayerBackend`/conformance updates.
2. Lyrics/discovery consumers re-routed through the adapter extras seam;
   Mercury cache (`backends/mercury_cache.rs`) moves with them.
3. `player_factory` → registry-driven optional backend per provider;
   `BackendKind` deleted (protocol serde shim for the status field it
   appears in).
4. Daemon transport-request gating on `TransportCaps` with clean errors +
   TUI empty-state rendering for transportless providers.
5. Provider-policy player events retain the installed provider identity across
   daemon, doctor, TUI, and macOS surfaces without exposing token-shaped text.

## Verification

```bash
scripts/cargo-nextest -p spotuify-player
scripts/cargo-test --workspace && scripts/smoke.sh
# the player-first gate — real playback must stay impeccable:
./target/release/spotuify daemon stop && ./target/release/spotuify daemon start
./target/release/spotuify play "<a song>" && sleep 3 && ./target/release/spotuify status --format json | jq .is_playing
./target/release/spotuify next && ./target/release/spotuify pause && ./target/release/spotuify resume
./target/release/spotuify lyrics show          # Mercury path through the new seam
./target/release/spotuify devices --format json
# watchdog regression: transfer to phone, confirm daemon does NOT yank it back (car-playback lesson)
```

## Exit criteria

- `grep -i mercury crates/spotuify-player/src/lib.rs` → nothing; trait is
  provider-neutral.
- Playback, lyrics, devices verified live (not just tests — player first).
- A registry entry with no backend produces capability errors, not crashes.

## Dependencies

Phase 5 (trait + caps). Sequenced late deliberately: player reliability is
non-negotiable, so the churn lands after the surrounding seams are stable.
