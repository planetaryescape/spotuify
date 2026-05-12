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

[spotifyd]
autostart = true
config_path = "~/.dotfiles/.config/spotifyd/spotifyd.conf"
device_name = "spotuify-hume"

[daemon]
autostart = true

[search]
engine = "tantivy"
cache_remote_results = true
```

## Secrets

- Access and refresh tokens live in system keyring.
- Client secret should not be required for PKCE.
- If a secret is stored for compatibility, `config show` must redact it.
- Bug reports must never include secrets.

## OAuth

Use Spotify OAuth PKCE.

Commands:

```text
spotuify login
spotuify logout
spotuify auth status
spotuify auth refresh
spotuify auth reauth
```

## Keychain timeouts

Every keychain call must be bounded. A hung keychain must degrade to a clear auth error, not freeze doctor, CLI, daemon, or TUI.

## spotifyd config

spotifyd uses its own OAuth flow in current versions. The spotuify config points to the spotifyd config file and preferred device name, but spotuify should not store spotifyd credentials.

`spotuify doctor` should detect invalid TOML in spotifyd config and stale device-name mismatches.
