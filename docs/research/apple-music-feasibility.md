# Apple Music support: feasibility study

**Captured 2026-07-16.** Answers the recurring question "can spotuify support
Apple Music?" so it does not need re-litigating from scratch. The decision this
fed is [D026](../blueprint/13-decision-log.md).

**Short answer: no, and the blocker is not our code.** There is no librespot for
Apple Music and there never has been. That removes core principle #1 (*player
first*, daemon owns playback) on Linux entirely, and leaves only awkward
GUI-session-bound options on macOS.

Two independent findings, either of which is disqualifying on its own:

1. **No legal, distributable way to stream Apple Music audio from a Rust daemon.**
2. **spotuify has no provider abstraction to slot a second provider into.** The
   seam most people assume exists (`SPOTUIFY_FAKE_SPOTIFY`) is not one.

## 1. Playback: why there is no librespot for Apple Music

librespot is a **clean-room reimplementation** of Spotify's protocol: it streams
and decrypts audio in-process, no Spotify app involved. Nothing equivalent exists
for Apple Music, and the reason is structural rather than incidental — FairPlay's
key delivery lives in obfuscated native blobs that every working tool *reuses*
rather than reimplements.

Do not be misled by search results for "FairPlay": **FairPlay-the-binary-encryption**
(`mremap_encrypted`, App Store `.ipa` protection — what `UnFairPlay` and `unfair`
target) is a different thing from **FairPlay Streaming** (content DRM). Only the
latter is relevant.

### What actually exists

| Approach | Mechanism | Why it fails for us |
|---|---|---|
| [WorldObservationLog/wrapper](https://github.com/WorldObservationLog/wrapper) (was `zhaarey/wrapper`) → [apple-music-downloader](https://github.com/zhaarey/apple-music-downloader) | Loads Apple's **Android** binaries (`libCoreFP.so`, `libCoreLSKD.so`, `libCoreADI.so`) on Linux via `linker64`, hooks them with Dobby, calls Apple's mangled C++ symbols (`storeservicescore::RequestContext::setFairPlayDirectoryPath`) | Requires redistributing Apple's proprietary `.so` files. Linux x86_64/arm64 only. DMCA §1201 circumvention. Not shippable in a public repo with a Homebrew tap. |
| [Manzana](https://github.com/dropcreations/Manzana-Apple-Music-Downloader) | Apple Music's web/AAC streams are **Widevine**-protected; uses `pywidevine` + a provisioned CDM blob | Still not clean-room — needs a Widevine device blob. Manzana's own README: Atmos/ALAC unsupported *"because those are not protected with Widevine. They are protected with the FairPlay."* |
| [Cider](https://cider.sh/), [Sidra](https://github.com/wimpysworld/sidra) | MusicKit JS inside **castLabs Electron** (the `wvcus` Widevine build; stock Electron lacks the EME CDM) | Legitimate and it genuinely works on Linux — but it is a Chromium process, not a daemon. No aarch64 Widevine CDM, so no ARM build. |

### The legitimate macOS paths

- **`ApplicationMusicPlayer`** (macOS 14+; note `SystemMusicPlayer` has *no* macOS
  availability at all). MusicKit requires the restricted
  `com.apple.application-identifier` entitlement. Apple DTS is explicit that a
  plain CLI cannot hold one ([forum 711950](https://developer.apple.com/forums/thread/711950)):

  > "MusicKit requires that code be signed with the `com.apple.application-identifier`
  > entitlement... a restricted entitlement... You're building a command-line tool,
  > which has no place to store a profile."

  The sanctioned workaround is wrapping the binary in an `.app` carrying
  `embedded.provisionprofile` and running `Tool.app/Contents/MacOS/Tool`
  ([Signing a Daemon with a Restricted Entitlement](https://developer.apple.com/documentation/xcode/signing-a-daemon-with-a-restricted-entitlement)).
  Also needs `NSAppleMusicUsageDescription` + `MusicAuthorization.request()` —
  **an interactive consent prompt**, which is a real first-run problem for a
  detached daemon.

  ⚠️ **Unverified:** whether an `.app`-wrapped LaunchDaemon renders audio with no
  logged-in Aqua session. Untested; no authoritative source found. Hypothesis: a
  LaunchAgent in a GUI session works, a root LaunchDaemon does not. Worth one
  empirical test if this is ever revisited.

- **AppleScript Music.app** — strictly worse than what spotuify already does.
  Read from `/System/Applications/Music.app/Contents/Resources/com.apple.Music.sdef`:
  there is **no queue class and no enqueue command**, and `search` is
  **library-only** (`type="playlist"` — "search a playlist for tracks matching the
  search string"), not catalog. Needs the app running in a GUI session.

  ⚠️ Claims that `osascript` over SSH works "against headless Macs with no GUI
  session" appear to conflate this with Remote Apple Events. Treat as false absent
  a test.

### Not a solution: AirPlay / DACP

- **AirPlay** is an *output transport*, not a content source. Senders are
  reverse-engineered ([pyatv](https://pyatv.dev/), [airplay2-rs](https://github.com/lmcgartland/airplay2-rs)),
  so you can push audio to a HomePod — but only PCM/ALAC **you already hold in the
  clear**. Gets you zero closer to Apple Music bits. (Music.app's sdef does expose
  a settable `current AirPlay devices`, so you can route *its* output.)
- **"Apple Music Connect"** is a red herring — a defunct artist-social feature
  (2015–2018), never a remote-control protocol. The real analog is **DACP** (port
  3689 + Bonjour, what the old Remote app used): reverse-engineered, no public
  spec, implementations ([dacp-net](https://github.com/melloware/dacp-net),
  [OwnTone](https://owntone.github.io/owntone-server/)) date to the iTunes era and
  break whenever Apple changes anything.

## 2. What the REST API *can* do

Everything except play audio. Confirmed against Apple's doc JSON (the rendered
pages are JS-only):

- Catalog search, library read, **full playlist CRUD** —
  [Create a New Library Playlist](https://developer.apple.com/documentation/applemusicapi/create-a-new-library-playlist)
  is real (`POST`, 201, optional `tracks` relationship and `parent` folder).
  Caveat verbatim: *"There may be a delay before a new resource appears in a
  user's library."*
- No audio. Metadata and previews only.

### Auth cost

- **Developer token: paid Apple Developer Program, $99/yr.** Non-negotiable —
  Certificates/Identifiers/Profiles is unavailable to free accounts, so there is
  no MusicKit key without paying. ES256/P-256, `kid` from a
  [media identifier and private key](https://developer.apple.com/help/account/capabilities/create-a-media-identifier-and-private-key/),
  `iss` = Team ID, `exp` ≤ 15777000s (6 months). Rate-limited `429` per token.
- **Music User Token: no Linux/Rust path.** Only three sanctioned sources
  ([User Authentication for MusicKit](https://developer.apple.com/documentation/applemusicapi/user-authentication-for-musickit)):
  MusicKit for Swift, MusicKit JS, or the Android SDK (*"Automatic Music User Token
  management is not available for Android"*). The token **is** portable once
  obtained — Apple's own docs show `curl` + a `Music-User-Token` header — so
  bootstrapping via browser and then driving the API from Rust works. That is
  exactly how Manzana et al. operate.

## 3. Our side: there is no provider seam

Audited 2026-07-16. Adding a provider is **not** "write a second impl" — it is a
data-model change reaching core, store, search, protocol, and the TUI.

### Genuinely abstracted

- **`PlayerBackend`** (`crates/spotuify-player/src/lib.rs:177`) — real trait, ~18
  methods, real second impl (`backends/mock.rs`). Spirc is properly contained in
  `backends/embedded/mod.rs`; the `spirc_activated` latch does not leak outside the
  player crate. **This is the easy 10%.** It still leaks Spotify: `mercury_get()`
  (Mercury is a Spotify-proprietary bus), `web_api_token()`, and `&str` URIs.
- **`AnalyticsSink`** (`crates/spotuify-core/src/analytics.rs:130`).
- **`spotuify-mcp`** — `core` + `protocol` only; vendor references confined to
  tool description strings. Cheapest client to adapt.

### Merely *looks* abstracted

- **`SPOTUIFY_FAKE_SPOTIFY` is not a second provider.** It flips a `fake: bool`
  field on the concrete client (`crates/spotuify-spotify/src/client.rs:82`) with
  ~30 inline `if self.fake` branches returning canned fixtures that hardcode
  `spotify:track:…` URIs. This is **anti-evidence**: we needed a test double and
  routed around the absent trait instead of introducing one.
- **The catalog layer has no trait at all.** `SpotifyClient`
  (`crates/spotuify-spotify/src/client.rs:70`) is a 3,707-line concrete struct with
  ~63 public methods, called from **53 sites** across `crates/spotuify-daemon/src`
  (22 in `handler.rs` alone), every one returning the concrete type. Nowhere to
  inject.
- **`BackendKind`** (`crates/spotuify-core/src/lib.rs:105`) — one variant
  (`Embedded`); its own doc calls it "a forward-compat marker".
  `player_factory.rs:39-45` unconditionally builds embedded. Selects nothing.
- **`media_items.source`** — looks like a provider column, is search provenance
  (`SearchSourceData { Local, Spotify, Hybrid }`; `Local` = local *cache*, `Hybrid`
  isn't a provider). Overloading it would corrupt search-source semantics.
  `crates/spotuify-store/src/lib.rs:2507` already hardcodes `source: Some("spotify")`.
- **`TrackId`/`AlbumId`/…** (`crates/spotuify-core/src/ids.rs`) — provider-neutral
  by name only. `:31` rejects non-Spotify URIs outright
  (`if parts.next()? != "spotify" { return None; }`) and `:52` can only emit
  `format!("spotify:{kind}:{id}")`. They are bypassed anyway — real identity is the
  bare `MediaItem.uri: String` (`crates/spotuify-core/src/lib.rs:262`),
  prefix-matched in ~15 sites across 8 crates.
- **`tests/workspace_boundaries.rs`** — states the client/backend rule at `:54`,
  then waives it in `ALLOWED_DEPS` (`:120-128`). The TUI imports core types
  *through* `spotuify_spotify` (`crates/spotuify-tui/src/app.rs:33`) and holds a
  live `SpotifyClient`. 331 spotify references in the TUI crate.

Store/search are keyed on the Spotify URI string as PK, with literal `spotify_id`
columns in both SQLite (`INITIAL_SCHEMA`, `crates/spotuify-store/src/lib.rs:2572`)
and the Tantivy schema (`crates/spotuify-search/src/lib.rs:400`). Rows would not
*collide* — an Apple URI is a different PK — which is the danger: they would
silently coexist and cross-contaminate. Nothing can filter by provider, so search
and all analytics (`track_metrics`/`artist_metrics`/`album_metrics`) would blend
both catalogs and double-count the same song under two URIs. Deduping needs
cross-provider identity resolution (ISRC), which does not exist here.

**No multi-provider intent exists in the repo.** Grepping every `.md` for "apple
music", "multi-provider", "provider-agnostic", "tidal", "youtube music" returns
zero hits. This was never a design goal.

## 4. The one variant worth considering

If the pull is ever strong enough, the coherent narrow version is **an Apple Music
provider that does everything except play audio**. REST gives catalog search,
library read, and playlist CRUD — precisely spotuify's agent-facing surface.
`spotuify playlist create --dry-run` and `ops undo` would work against Apple Music;
`spotuify play` would not.

That is a *different product* ("agents curate your Apple Music library, the Music
app plays it") and it breaks the promise the README leads with. **Resolve the
product question before touching `ids.rs`** — including what `register_device` and
`DeviceTransfer` even mean for a service with no Connect equivalent. That is a
product call, not an engineering one.

Rough shape if it ever happens, in dependency order:

1. Introduce provider identity into `MediaItem.uri` / `ids.rs`; propagate through
   ~15 prefix-matching sites in 8 crates.
2. Store migration + full Tantivy reindex (machinery exists —
   `crates/spotuify-search/src/reindex.rs`): add a real provider discriminator,
   drop `spotify_id`.
3. Extract a catalog trait behind the 53 concrete `spotify_client()` call sites.
   Delete the `if self.fake` branches in favour of a real fake impl — worth doing
   **on its own merits** regardless of Apple Music.
4. Untangle the TUI's live `SpotifyClient`; enforce `workspace_boundaries.rs`
   instead of waiving it.
5. Only then: an Apple Music provider impl.

Steps 1–4 are worth their own conversation as an architecture-hygiene project.
Step 5 is the part that cannot deliver playback.

## Re-validation triggers

Revisit only if one of these becomes true:

- A clean-room FairPlay Streaming client ships (i.e. an actual librespot analog).
  This is the only thing that changes the answer for Linux.
- Apple ships a MusicKit path that works headless without a GUI session.
- We decide the metadata/playlist-only product in §4 is worth being a different
  product.

Otherwise the answer stays no. Sources are linked inline; all claims captured
2026-07-16 against docs and source current on that date — re-verify before relying
on any of them.
