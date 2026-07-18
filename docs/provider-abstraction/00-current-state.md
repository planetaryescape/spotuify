# Provider Abstraction - Current State Audit

Audited 2026-07-16 (three parallel codebase agents + two mxr agents + external
research). This is the evidence base for every phase doc in this directory.
File:line references were verified on this date; re-verify before relying on
them after significant refactors.

## Verdict

There is no provider abstraction today. There is exactly one real trait seam
(`PlayerBackend`) and it sits at the playback-transport layer, not the provider
layer. Everything else — catalog, library, playlists, sync, auth, config,
persistence identity — is concretely Spotify-shaped. The good news: the daemon
architecture, the IPC contract, and the clients are in *better* shape than the
folklore says (the TUI no longer talks to a live `SpotifyClient` — see §6), and
mxr has already solved nearly every design problem this plan needs to solve.

## 1. Catalog/client layer

- `SpotifyClient` (`crates/spotuify-spotify/src/client.rs:70`) is a 3,707-line
  concrete struct, 62 public methods, no trait. Called from **53 sites** in
  `crates/spotuify-daemon/src` (22 in `handler.rs` alone). Construction is
  centralized in `state.rs:1933` (`spotify_client()`), which is the future
  injection point.
- Method surface groups cleanly into capability clusters: catalog search (2),
  catalog lookup (6), library read (6), library write (5), playlist read (2),
  playlist write (8), playback control via Web API (13), devices (2), user (1).
  Nearly all return core types (`MediaItem`, `Playlist`, `Device`, `Playback`,
  `Queue`). Spotify-protocol leaks in signatures: `SavedTracksPage`
  (client.rs:51), `snapshot_id` params/returns, `repeat(&str)`,
  `set_playlist_image(base64)`.
- An intermediate layer already exists: `crates/spotuify-spotify/src/actions.rs`
  wraps the client in higher-level free functions plus a
  `execute(client, CommandKind) -> CommandResult` dispatcher (:250). This is a
  provider-neutral-ish command layer the trait design should absorb, not bypass.
- **The fake is not a seam.** `fake: bool` (client.rs:78) with **39
  `if self.fake` guards** (one per network-touching method) returning inline
  fixtures (client.rs:2887-3063). `SPOTUIFY_FAKE_SPOTIFY` flips this bool via
  `state.rs:1939`. HTTP-level tests use mockito + `with_api_base_for_tests`.
  Contrast: the player has a real trait-impl fake (`backends/mock.rs`).

## 2. Identity

- `crates/spotuify-core/src/ids.rs` newtypes (`TrackId` etc.) hardcode the
  `spotify` scheme (`:31` rejects others, `:52` formats it) — and are
  **near-dead code**: exactly one non-test caller (client.rs:594). Free to
  redesign.
- Real identity is bare `String` URIs (`MediaItem.uri`, core/lib.rs:262),
  manipulated by **~40 string-op sites across 12 crates**: the
  `media_kind_from_uri` dispatcher (selection.rs:35-58, 13 callers), ~15
  `rsplit(':')` bare-id extractions, ~15 `format!("spotify:…")` builders,
  plus `starts_with`/`strip_prefix` scatter (exhaustive list in the phase-1
  doc). The `<scheme>:<kind>:<id>` shape and the literal `spotify` scheme are
  assumed everywhere.

## 3. Core model Spotify semantics

`MediaItem` (core/lib.rs:259-317): `resume_position_ms`/`fully_played` (from
Spotify `resume_point`), `release_date` ("Spotify's YYYY-MM-DD string"),
`album_group` (Spotify vocabulary), `album_uri`/`ArtistRef.uri` (spotify URIs),
`source: Option<String>` (free-form provenance: "spotify"/"mercury"/"local").
`Playlist.snapshot_id` (core/lib.rs:343) is Spotify's version token and gates
sync refetch. `Playback.provider_timestamp_ms` and `PlaybackStateSource::WebApiPoll`
name Spotify Web API concepts. `BackendKind` has one variant (`Embedded`) — a
self-described forward-compat marker that selects nothing. Precedent that the
codebase already does provider enums: `LyricsProvider { SpotifyMercury, Lrclib }`
(core/lib.rs:349).

## 4. Persistence

- **Store** (`crates/spotuify-store/src/lib.rs`): URI strings are load-bearing
  PKs/FKs in ~15 tables (`media_items.uri` PK :2572, `library_items`,
  `lyrics_cache`, `*_metrics`, `playlist_items.item_uri` FK, …). Two hardcoded
  Spotify spots: `media_items.spotify_id` (:2574) and `source TEXT DEFAULT
  'spotify'` (:2581) — **`source` is a trap**: it's search provenance
  (`Local|Spotify|Hybrid`), not a provider column. Last.fm import has a column
  literally named `resolved_spotify_uri` (:3125).
- **Migration machinery exists and is proven**: append-only `MIGRATIONS` array
  at version 20 (lib.rs:3177), typed `MigrationKind` (Sql / AddColumns /
  AddColumnsThenSql / bespoke rebuild), `schema_migrations` stamps,
  `CACHE_VERSION` forward-incompat guard (:2007). Shipping a provider column is
  routine, not novel.
- **Search** (`crates/spotuify-search/src/lib.rs:396-425`): keys on `uri`
  (delete-term upsert), one Spotify-named field (`spotify_id`, :400). Full
  reindex is manual/admin (`reindex.rs`, `Request::Reindex`); steady-state
  freshness is incremental via sync. No auto-rebuild-on-schema-mismatch.

## 5. Sync

`crates/spotuify-sync`: the deepest concrete coupling —
`SyncContext::spotify_client() -> anyhow::Result<SpotifyClient>` (lib.rs:52)
returns the concrete type. Spotify assumptions: `snapshot_id` refetch gates
(lib.rs:165-204), 403 handling by matching endpoint strings
(`sync_loop.rs:845-854`), playlist URI building (:945-948). Cadences and the
target dispatcher (`SyncTargetData`) are provider-agnostic already.

## 6. Clients — better than the folklore

**The TUI holds no `SpotifyClient`, no Store, no Search, no Sync.** All data
flows over IPC (~44 `Request::*` variants, all already in the protocol). The
comment in `tests/workspace_boundaries.rs:119-123` claiming otherwise is stale,
and `store/search/sync/player` in `crates/spotuify-tui/Cargo.toml:12-16` are
dead edges. Residual real coupling, none of it data:

1. Type imports through the vendor crate: `app.rs:33` imports
   `Device/MediaItem/MediaKind/Playback/Playlist/Queue` via
   `spotuify_spotify::client` re-exports (they're core types, client.rs:24).
2. In-process OAuth login (`app.rs:3176-3195`; CLI same at
   `commands.rs:1857-1867`) — the PKCE flow runs client-side, then
   `Request::ReloadAuth`.
3. Direct `Config::load()` reads for viz/audio-output defaults
   (app.rs:840,3193,3252).
4. Daemon lifecycle helpers via `spotuify_daemon::server` (mostly re-exports
   from `spotuify_launcher`).

CLI: IPC-only except OAuth login; `normalize_spotify_target`
(selection.rs:67) is its Spotify-format input parser. MCP: genuinely
protocol-only; Spotify references are description strings plus the functional
`SearchSourceData::Spotify` tag.

## 7. Protocol

`IPC_PROTOCOL_VERSION = 7` (protocol/lib.rs:56). URIs cross the wire as opaque
strings (good). Provider leaks: `SearchSourceData { Local, Spotify, Hybrid }`
(:917-923) is a wire-level provider tag; some `IpcErrorKind` variants are
Spotify-shaped; doc comments assume Spotify throughout.

## 8. Player

`PlayerBackend` (player/lib.rs:177) is the model trait: async_trait,
`Send + Sync`, default impls returning `Unsupported` for optional capabilities,
built by `player_factory` as `Box<dyn>`, driven via an actor
(`run_player_actor`, state.rs:2202). Spotify leaks in the trait:
`mercury_get` (:253 — Mercury is Spotify-proprietary; consumers: lyrics
handler.rs:529, discovery handlers/library.rs:135,156), `web_api_token` (:240 —
login5 minting, currently mostly unused), `&str` URIs, `PlayContextRequest`
comments, `PlayerError::PremiumRequired`. `DeviceTransfer` is handled via Web
API + selection helpers (handlers/playback.rs:347), not the trait — transfer is
a Connect concept with no trait surface.

## 9. Auth & config

Auth is file-based under `<config_dir>/auth/` with three paths: dev-app PKCE
(default), first-party/login5 (opt-in), hybrid (writes → first-party) routed by
`endpoint_needs_first_party` (client.rs:1804). The only auth abstraction is
`WebApiBearerProvider` (client.rs:44) — a clean seam. Cooldowns are
bearer-scoped (client.rs:271). `Config` (spotify/config.rs:20) is one
Spotify-shaped struct: flat OAuth creds top-level, librespot settings in
`[player]`, generic sections (`[cache]`, `[analytics]`, `[viz]`, …) mixed in.
No `[providers.*]` concept. Rate-limit machinery
(`rate_limit.rs`: RateLimitedClient, BackoffState, Priority) is structurally
generic but lives in the spotify crate and classifies Spotify errors.

## 10. The mxr precedent (copy-first source)

mxr is genuinely multi-provider (Gmail, IMAP, SMTP, Outlook ×2, Fake) and its
blueprint declared the provider-agnostic core from day one (vision.md; decision
D-CAL-007). What to copy, with sources:

| Pattern | mxr location | Copies to |
|---|---|---|
| Split traits, not one god-trait (sync vs send) | `core/src/provider.rs:8-156`; rejection rationale `docs/blueprint/03-providers.md:7-13` | catalog/library/playlist vs playback-control split |
| Registry = `HashMap<AccountId, Arc<dyn Trait>>` in daemon state + factory from config | `daemon/src/state.rs:19-29,578-871` | provider registry in DaemonState |
| Deterministic UUIDv5 IDs scoped `(account, provider, native-id)` | `core/src/id.rs:35-47` | optional; spotuify may keep string URIs with provider prefix |
| `UNIQUE(account_id, provider_id)` upsert key | `store/migrations/001_initial.sql:38-59` | provider-scoped store identity |
| **Opaque `SyncCursor(Vec<u8>)`** — daemon persists blindly, adapter owns encoding; was a tagged union first, refactored (their biggest lesson) | `core/src/types.rs:1834-1859`; `docs/msp/mxr-alignment.md:75-113` | sync cursors + snapshot_id generalization |
| Engine takes `&dyn Provider` per call; holds only store+search | `sync/src/engine.rs:229-232` | fixes `SyncContext::spotify_client()` |
| Cursor-expiry recovery centralized, typed error | `engine.rs:264-289` | `SyncCursorExpired`-style variant |
| Namespaced capability flags, default-false, serde-additive (were "flat boolean soup" first) | `core/src/types.rs:2010-2073`; alignment.md:343-368 | provider capabilities |
| Capability meaning is semantic not syntactic ("labels=false ≠ no folders") | `03-providers.md:106` | e.g. queue semantics per provider |
| One `apply_mutation(mutation_id, &Mutation)` + 24h dedup log (was 4 methods) | `provider.rs:54-65`; `store/src/mutation_dedup.rs` | idempotent playlist/library writes |
| **`provider-fake` crate + `run_conformance()` every adapter must pass** | `crates/provider-fake/src/conformance.rs` | replaces the 39 `if self.fake` branches |
| Daemon-side OAuth as pollable session state machine | `daemon/src/handler/auth_sessions.rs:84-237` | moves client-side PKCE into daemon |
| Per-account tagged-enum config (`[accounts.<key>]`, sync/send independent) | `config/src/types.rs:503-580` | `[providers.<key>]` tables |
| Credential scoping by runtime instance (dev can't read prod secrets) | `config/src/resolve.rs:99-118` | dev/prod auth isolation |
| Post-migration `validate_schema()` REQUIRED_COLUMNS guard | `store/src/pool.rs:249-260,810-975` | store robustness (phase 2) |
| Tantivy = disposable derived data; auto wipe+rebuild on open mismatch | `search/src/index.rs:141-190`; daemon `state.rs:1488-1538` | search robustness (phase 2) |
| Optional `account_id` on requests via serde defaults; version bumps only on breaks | `protocol/src/lib.rs:18-35`, `types.rs:424-434` | wire evolution (phase 8) |
| Per-account sync task isolation + detach timeout | `daemon/src/loops.rs:74-116,301-336` | multi-provider sync loops |

**Traps found in mxr:** the adapter-skeleton example
(`examples/adapter-skeleton/`) and blueprint §03 code samples are stale
pre-refactor shapes — copy from `crates/provider-fake` + the real trait, never
the skeleton. `provider_meta` (the per-item raw-provider sidecar table) exists
but is dormant — copy the idea, know it's unproven. mxr has no explicit
rate-limiter; spotuify is ahead there and keeps its own.

## 11. External research (state of the art, verified from source 2026-07-16)

- **Music Assistant** (the modern multi-provider reference; ~120 providers)
  [verified from source]: `MusicProvider` (catalog/library/streams) vs
  `PlayerProvider` (playback) is the load-bearing split — its Spotify *music*
  provider streams via a bundled librespot binary while `spotify_connect` is a
  separate player provider, i.e. exactly spotuify's implicit Web-API/Spirc
  split made explicit. Identity: canonical item rows + `provider_mappings`
  junction (`UNIQUE(media_type, provider_instance, provider_item_id)`,
  `available` flag flipped not deleted) + non-unique `external_id_lookup`.
  Matching cascade: provider-mapping hit → external-id hit (double-checked with
  metadata compare) → normalized name compare; **ISRC only counts with duration
  within ±8s**. Search fan-out: per-provider soft+hard timeouts, provider
  errors never fail the aggregate.
- **OwnTone / Strawberry / Navidrome** [verified from source]: mature native
  apps make capability *structural* — NULL vtable slots (OwnTone), separate
  scheme-keyed URL-handler registries (Strawberry), single-method capability
  interfaces + runtime assertion (Navidrome) — with booleans reserved for
  runtime/account quirks. Strawberry is precedent that one provider's playback
  (Spotify via GStreamer/librespot) may legitimately bypass the generic
  resolution path.
- **Rust idioms 2026**: AFIT is stable but still not dyn-compatible;
  `async_trait` remains maintained; `dynosaur` 0.3 generates the dyn boundary
  from plain AFIT traits. OpenDAL is the best prior art: plain `Capability`
  struct of named bools + `Option<usize>` limits (not bitflags), one wide
  trait where unsupported ops return `ErrorKind::Unsupported`. For a closed
  compiled-in provider set, a hand-rolled `#[non_exhaustive]` enum with
  cfg-gated variants (termusic ships this) dodges dyn-async entirely.
  `enum_dispatch` does not support AFIT. spotifyd/ncspot/psst are the
  cautionary tales: one provider's session type hardwired through the app is
  what makes provider #2 a rewrite.
- **ISRC is many-to-many in both directions** (IFPI handbook; ~15% of
  MusicBrainz recordings carry one) — never a primary key, always a match
  signal feeding a mappings table with a duration gate. Albums match on
  UPC/EAN (needs leading-zero normalization). Even ListenBrainz's mbid-mapper
  falls back to normalized artist+title strings — identity cannot be
  outsourced to MusicBrainz.
- **Odesli/Songlink public API sunsets 2026-07-31** — off the table as a
  dependency.
- **Mopidy** [NOT source-verified this session — leads only]: URI-scheme
  routing to backend actors carried a ~100-extension ecosystem for 15 years;
  known pain: aggregate search blocks on slowest backend, no cross-backend
  dedup.

## Consolidated leak inventory

| Leak | Location | Fix phase |
|---|---|---|
| Dead TUI Cargo deps (store/search/sync/player) | tui/Cargo.toml:12-16 | 0 |
| Types imported via `spotuify_spotify::client` re-export | tui app.rs:33 | 0 |
| Stale boundary comment + permissive ALLOWED_DEPS | tests/workspace_boundaries.rs:55,119-137 | 0 (comment), 8 (tighten) |
| ~40 URI string-op sites; `ids.rs` scheme hardcode | 12 crates | 1 |
| No post-migration schema validation; no index auto-rebuild | store, search | 2 |
| `snapshot_id`, `album_group`, `release_date`, `SavedTracksPage`, `repeat(&str)` semantics in core | core/lib.rs, client.rs | 3 |
| No provider discriminator in store/index; `spotify_id` columns; `source` overload | store lib.rs:2574-2581, search lib.rs:400 | 4 |
| Concrete `SpotifyClient`, 53 call sites, 39 fake branches | spotify crate, daemon | 5 |
| `SyncContext::spotify_client()`; snapshot gates; endpoint-string 403s | sync crate | 6 |
| Client-side OAuth; Spotify-shaped `Config`; flat creds | spotify/{auth,config}.rs, tui, cli | 7 |
| `SearchSourceData::Spotify`; Spotify `IpcErrorKind` variants; MCP/CLI strings | protocol, mcp, cli | 8 |
| `mercury_get` / `web_api_token` / `PlayContextRequest` / DeviceTransfer semantics | player, daemon | 9 |
| No second adapter; no cross-provider identity | — | 10 |
