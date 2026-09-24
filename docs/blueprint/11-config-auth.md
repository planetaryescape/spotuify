# spotuify - Config and Auth

## Config goals

Config should be readable, small, and safe to share after secrets are redacted.

## Paths

Current macOS path uses `dirs::config_dir`, which resolves to Application Support on macOS.

Target behavior should be explicit in docs and CLI:

```text
spotuify config path
spotuify config show --redacted
spotuify config doctor
```

## Config shape

```toml
client_id = "..."
redirect_uri = "http://127.0.0.1:8888/callback"

[player]
backend = "embedded"
device_name = "spotuify-hume"
bitrate = 320
normalization = false
audio_cache_mib = 0

[daemon]
autostart = true

[search]
engine = "tantivy"
cache_remote_results = true
```

## Secrets

- Default dev-app PKCE credentials live in `<config_dir>/auth/token.json` with mode `0600` on Unix.
- `<config_dir>/auth/token.lock` serializes login, logout, refresh, and revocation purge across daemon/CLI processes.
- First-party/keymaster credentials store only refresh token + scopes in
  `<config_dir>/auth/first-party.json`. Fresh setup opts in with
  `SPOTUIFY_USE_FIRST_PARTY=1`.
- Client secret is optional for PKCE.
- If a secret is stored for compatibility, `config show` must redact it.
- Bug reports must never include secrets.

## OAuth

Default: Spotify OAuth PKCE with a user-provided Spotify Developer app `client_id`.

Experimental: first-party/keymaster auth via librespot login5.

`SPOTUIFY_USE_FIRST_PARTY` is an explicit override in either direction. When it
is unset, stored credentials decide the runtime mode:

- a dev-app token selects dev-app reads;
- a dev-app token plus first-party credentials enables hybrid dev-app reads and
  first-party writes on the supported mutation endpoints;
- first-party credentials without a dev-app token select first-party mode;
- no credentials selects dev-app onboarding, not an unusable credential mode.

`spotuify login --dev-app` migrates a first-party-only setup back to dev-app
credentials.

Commands:

```text
spotuify login
spotuify logout
spotuify auth status
spotuify auth bearer --reveal-secret
```

## Auth file failure behavior

Auth file reads and writes must fail clearly and never freeze doctor, CLI, daemon, or TUI.

Corrupt current auth files are source-of-truth errors. Legacy `<data_dir>/auth/*.json` files are migration inputs only: read once, copied into `<config_dir>/auth/`, then ignored if unreadable.

## Refresh-token revocation

Refresh tokens are mutable shared state, not static config. Refresh paths must:

- hold the token-store lock,
- reload persisted credentials under the lock before refreshing stale memory,
- persist replacement refresh tokens when Spotify returns one,
- keep the old refresh token when Spotify omits one,
- purge memory + disk on `invalid_grant` only after re-checking that the failed refresh token is still the persisted token,
- fail fast through the daemon auth latch until re-auth or a successful health probe clears it.

## Legacy player config

Current playback is embedded librespot only. The supported config surface is `[player]`, and `player.backend` currently accepts only `embedded`.

Old configs with `[spotifyd] device_name = "..."` are still honored as a read-only migration shim when `[player] device_name` is absent. Other spotifyd fields are ignored; spotuify does not manage a spotifyd subprocess.

`spotuify doctor` should report the effective player device name and whether the embedded device is visible/connected.
