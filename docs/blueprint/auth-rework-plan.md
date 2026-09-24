# Auth rework: first-party librespot login (drop the user dev app)

> Status: superseded. The staged rework was built far enough to keep
> first-party/keymaster auth as an opt-in experiment, but the default was
> reverted on 2026-05-26. Current default auth is user dev-app PKCE with
> `client_id` in config or `SPOTUIFY_CLIENT_ID`. A fresh first-party login uses
> `SPOTUIFY_USE_FIRST_PARTY=1`; later restarts follow the stored credential mode
> when the variable is unset. See D016 in `docs/blueprint/13-decision-log.md`.

> Staged plan to replace spotuify's "register your own Spotify app" Web API auth
> with librespot's first-party OAuth (keymaster client id) + `login5`, like
> spotify-player. Fixes the Development-Mode 403 on playlist writes and removes
> the worst onboarding step. Validated 2026-05-24: dev-app token -> 403 (policy
> blocked); first-party keymaster token -> 429 (authorized, only rate-limited).

## Why

Each spotuify user registers their own Spotify Developer app and pastes the
client_id. That app is in Development Mode, and Spotify (Feb 2026) refuses
playlist writes for dev-mode apps (`POST /users/{id}/playlists` and
`POST /playlists/{id}/tracks` both 403), even for an allow-listed owner on
Premium. Re-login + allow-listing did not help (verified). The only fix used by
working terminal clients (spotify-player, ncspot) is to mint the Web API token
from librespot's first-party client, which is never in Development Mode.

## New architecture (inverted token flow)

Today: dev-app Web API token -> bootstraps the librespot session.
After: one browser login (`librespot-oauth`, keymaster client id
`65b708073fc0480ea92a077233ca87bd`) -> native librespot credentials ->
`Session::login5().auth_token()` mints the full-scope Web API bearer for ALL
Web API calls (reads and writes). No user dev app.

- Web API bearer = `login5().auth_token()` (full scope, re-mintable from the live
  session without a browser; survives keymaster-OAuth-endpoint outages, which is
  how spotify-player recovered after Spotify broke keymaster in Aug 2025). NOT
  the raw librespot-oauth token (scope-constrained).
- Playback creds = `Credentials::with_access_token(oauth.access_token)` on first
  connect; librespot then persists reusable native creds to its cache, so later
  daemon starts need no token for playback.
- Note: `login5` `Token.scopes` is always empty, so the scope-drift banner
  (`missing_required_scopes`, `emit_scope_reauth_event_if_needed`) must be
  retired in the cutover or it fires a permanent false "run spotuify login".

## Token lifecycle

- Refresh: re-mint via `login5().auth_token()` from the live session (primary).
  Fallback: `librespot-oauth` `refresh_token_async` to re-bootstrap the session.
- Activate the already-built-but-unused `WebApiTokenSource`/`TokenBridge`
  scaffold in `crates/spotuify-player/src/backends/token_bridge.rs` (5s timeout,
  60s headroom refresh, graceful cache fallback) as the bearer source.
- The private auth file stores the librespot-oauth refresh token (one secret)
  instead of the dev-app token pair; reusable native creds live in librespot's
  own cache; the Web API bearer is never persisted (always minted live).

## Onboarding / config

- `config.rs`: historical target was to make `client_id` optional and use the
  keymaster id; current code did not keep this default. Keep
  `SPOTUIFY_CLIENT_ID` as a power-user override. Drop the required client_secret.
  Remove the dashboard instructions from the config template and
  `ensure_config_exists` bail.
- `src/main.rs` `onboard`/`needs_onboarding`: delete the dev-app credential
  prompts; new flow is "open browser to log in". Keep `--no-daemon-start`,
  `login`/`logout`/`onboard` names, and the CLI contract.

## Migration

Existing users hold a dev-app token. Tag the stored credential with an
`auth_kind`; on load, classify legacy dev-app creds as "needs re-login" and
surface the existing AuthRequired banner ("spotuify changed how it logs in, run
`spotuify login`"). Don't crash a running daemon; don't auto-delete the legacy
token until a successful first-party login overwrites it.

## Stages (dependency order, each independently testable)

1. **Additive token-minting module** (cannot regress). `librespot-oauth` login +
   `login5` mint + a serializable `FirstPartyCredentials` (shared in
   `spotuify-spotify`; the librespot calls in `spotuify-player`). Unit-test the
   pure parts; live-verify reads (`GET /me`) now; writes wait for the 24h quota
   reset (429, not 403, already proves authorization).
2. **Activate `TokenBridge`** with a real `WebApiTokenSource` over the session
   (still not the default). Test with scripted-login5 fakes.
3. **New auth-file schema + legacy detection** (additive).
4. **RISKY CUTOVER**: daemon + `client.rs` take the bearer from the bridge;
   embedded backend bootstraps from first-party creds; retire scope-drift.
5. **Onboarding/config UX + migration prompt** (user-facing).
6. **Cleanup**: delete dead dev-app PKCE code.

## Verification

Per-crate `scripts/cargo-test`; clippy gates; fake provider for smoke; NO
live-API tests. Verifiable now: unit/serde/bridge tests + live reads with a
minted bearer. Verifiable after ~24h: live playlist write returning 200 (not
429). The 403->429 transition already proves the write path is authorized.

## Risks

- ToS-gray first-party impersonation: same posture already taken by embedding
  librespot for playback; keep `SPOTUIFY_CLIENT_ID` override as an opt-out.
- Breakage-prone: anchor steady state on `login5` (re-mintable) so an
  OAuth-endpoint outage only blocks the one-time login, not running daemons.
- Premium required (already true for embedded playback).
- Graceful failure: the `TokenBridge` timeout + cache fallback prevents a hung
  mint from blocking the daemon; hard failure surfaces the re-login banner.

## Critical files

`crates/spotuify-spotify/src/{auth.rs,config.rs}`,
`crates/spotuify-player/src/backends/{token_bridge.rs,embedded/mod.rs}`,
`crates/spotuify-daemon/src/{state.rs,player_factory.rs}`, `src/main.rs`.
