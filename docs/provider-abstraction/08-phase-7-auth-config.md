# Phase 7 - Per-Provider Auth & Config

## Goal

Move OAuth into the daemon as a pollable session, restructure `spotuify.toml`
so provider settings live under `[providers.<key>]`, and re-home the generic
config sections out of the spotify crate. After this phase, "add a provider"
means "add a config table variant + an auth session kind", and the CLI/TUI
stop linking Spotify auth code.

## Standalone value

Moderate-high independent of providers: daemon-side auth fixes a real
architectural wart — today the PKCE flow runs *in the client process* (TUI
app.rs:3176-3195, CLI commands.rs:1857-1867) and then pokes the daemon with
`ReloadAuth`. That means two codepaths own token files, and a headless daemon
can't drive its own re-auth. Instance-scoped credentials (mxr's trick) also
close the dev-daemon-reads-prod-tokens hazard the hard-won dev-build keychain
lessons circle around.

## Evidence base

- Client-side login sites: app.rs:3176-3195 (+ `LoginProgress` modal),
  commands.rs:1857-1867 (+ :1793). Both call
  `spotuify_spotify::auth::login(&config, progress)` in-process.
- `Config` (spotify/config.rs:20-34): flat top-level `client_id`/
  `client_secret`/`redirect_uri`; librespot settings in `[player]`
  (:404-424); generic sections (`[cache]`, `[analytics]`, `[notifications]`,
  `[discord]`, `[viz]`) mixed into the same Spotify-crate-owned struct.
  Legacy `[spotifyd]` section (:285).
- The clean seam that already exists: `WebApiBearerProvider` (client.rs:44)
  and the hybrid write-routing (`endpoint_needs_first_party`, client.rs:1804)
  — both stay *inside* the Spotify adapter; they are Spotify's private
  auth complexity, invisible above the trait. Bearer-scoped cooldowns
  (client.rs:271) likewise.
- mxr (verified): daemon-side OAuth as a session state machine
  (`Starting → WaitingForUser → Authorized/Failed`) the client polls
  (auth_sessions.rs:84-237 — Gmail loopback-redirect and Outlook device-code
  both fit it); per-account tagged-enum config
  (`SyncProviderConfig::Gmail{…}/Imap{…}/Fake`, config/types.rs:514-548);
  config holds only *references* to credentials, never secrets;
  instance-scoped credential service names so dev can't read prod
  (resolve.rs:99-118).

## Design

1. **Config layout** (with migration shim reading the old shape + a
   deprecation warning for one release cycle):

   ```toml
   [providers.spotify]            # tagged: type = "spotify"
   client_id = "…"
   redirect_uri = "…"
   # librespot/player settings fold in here too (bitrate, device_name, …)

   [cache] / [analytics] / [viz] / [notifications] / [discord]   # unchanged, generic
   ```

   Generic sections + the loader move to a home the clients can link without
   the vendor crate (`spotuify-core` or a small `spotuify-config` crate —
   decide at impl; core has no internal deps today per crate rule 1, which
   argues for the separate crate). The Spotify adapter owns deserialization of
   its own table (mxr's tagged-enum pattern).
2. **Daemon-side auth sessions.** New protocol surface:
   `AuthStart { provider } → AuthSessionData` (browser URL or device code),
   `AuthPoll { session_id }`, `AuthCancel`. The daemon runs the PKCE
   loopback listener (it already owns the token files and the redirect URI is
   loopback anyway); TUI's `LoginModal` and CLI `login` become pure
   render-and-poll over IPC. `spotuify_spotify::auth` stops being a client
   dependency. Existing `ReloadAuth` remains for the transition, then
   deprecates.
3. **Auth file layout per provider:** `<config_dir>/auth/<provider>/…`
   (Spotify keeps `token.json`/`first-party.json` semantics inside its dir;
   a shim migrates existing paths on first run). Mode selection logic
   (`SPOTUIFY_USE_FIRST_PARTY`, stored-credential fallback) is untouched —
   it's inside the adapter.
4. **Instance scoping:** credential/auth paths already key off
   `SPOTUIFY_INSTANCE`; add the mxr-style guard so a dev-instance daemon
   refuses to read prod auth dirs even when pointed at them explicitly.

## Deliverables

1. Config split + loader re-home + old-shape shim + `spotuify config`
   subcommand updates (`get/set` paths gain the `providers.spotify.` prefix,
   old keys aliased).
2. Auth session protocol requests + daemon implementation + CLI/TUI ports.
3. Per-provider auth dir layout + migration shim.
4. Boundary tightening: TUI and CLI drop `spotuify_spotify::auth`; the
   `spotify` edge from clients shrinks to `selection::normalize_spotify_target`
   (CLI) only — which phase 8 then re-homes, completing the "clients =
   core+protocol only" rule.

## Verification

```bash
scripts/cargo-test --workspace && scripts/smoke.sh
# real auth drill (dev instance, throwaway login):
./target/release/spotuify auth logout
./target/release/spotuify login          # must open browser via daemon session, complete, and
./target/release/spotuify auth bearer | head -c 16   # mint a live bearer
./target/release/spotuify doctor --format json | jq '.checks[] | select(.name|test("auth"))'
# config shim drill: run once with the OLD toml shape, assert warning + working load
```

## Exit criteria

- No client crate links `spotuify_spotify::auth`.
- Old `spotuify.toml` shape loads with a warning; new shape round-trips
  through `config get/set`.
- `spotuify login` works end-to-end with the daemon owning the flow.

## Dependencies

Phase 5 (the factory that consumes `[providers.*]`). Deliverable 2 (auth
sessions) is independently shippable earlier if wanted.
