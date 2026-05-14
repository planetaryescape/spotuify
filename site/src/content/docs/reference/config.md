---
title: "Config"
description: "Document config paths, keys, defaults, env vars, and one-shot overrides."
---

Config is TOML. Secrets belong in the OS credential store when possible.

## Paths

```bash
spotuify config path
SPOTUIFY_CONFIG=/tmp/spotuify.toml spotuify config path
```

## Managed keys

These keys are accepted by `spotuify config get` and `spotuify config set`.

| Key | Type | Default | Notes |
| --- | --- | --- | --- |
| `client_id` | string | none | Spotify app client id |
| `client_secret` | string | none | optional for PKCE-first setups |
| `redirect_uri` | string | `http://127.0.0.1:8888/callback` | must match Spotify app |
| `spotifyd.config_path` | path | platform default | spotifyd config path |
| `spotifyd.device_name` | string | none | preferred device, e.g. `spotuify-hume` |
| `spotifyd.autostart` | bool | `true` | start supervised spotifyd when needed |
| `player.backend` | enum | crate default | parsed by `BackendKind` |
| `player.bitrate` | number | `320` | `96`, `160`, or `320` |
| `player.device_name` | string | none | preferred embedded/connect device name |
| `player.normalization` | bool | `false` | player normalization |
| `player.audio_cache_mib` | number | `0` | embedded playback cache size |
| `player.pulse_props` | bool | `true` | Linux Pulse/PipeWire app props |
| `player.event_hook` | string | none | shell hook command |
| `cache.cover_cache_mb` | number | `200` | cover-art cache cap |
| `cache.cover_cache_ttl_days` | number | `30` | cover-art TTL |

```bash
spotuify config get player.bitrate
spotuify config set player.bitrate 320
```

## File-only sections

Some config is loaded from TOML but not yet wired through `config set`.

```toml
[analytics]
store_raw_queries = true
retention_progress_days = 90
retention_events_days = 365
retention_operations_days = 90
daily_rollup_hour = 3
hook_command = "/Users/me/bin/spotuify-listen-hook"
hook_timeout_ms = 5000
allow_file_credentials = false

[viz]
enabled = false
source = "auto"
target_fps = 30
smoothing = 0.5
noise_gate = 0.005
color_scheme = "spotify-green"
```

## Environment variables

```bash
SPOTUIFY_CLIENT_ID=... spotuify login
SPOTUIFY_CLIENT_SECRET=... spotuify login
SPOTUIFY_REDIRECT_URI=http://127.0.0.1:8888/callback spotuify login
```

## One-shot overrides

```bash
spotuify -o player.bitrate=160 play "ambient"
spotuify -o spotifyd.autostart=false doctor
```

Overrides apply only to that command.

## See Also

- [Install](/getting-started/install/)
- [CLI Concepts](/reference/cli/concepts/)
- [Troubleshooting](/reference/troubleshooting/)
