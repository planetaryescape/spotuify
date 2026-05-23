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
| `player.backend` | enum | `embedded` | only `embedded` (in-process librespot); Spotifyd/Connect-only backends were removed |
| `player.bitrate` | number | `320` | `96`, `160`, or `320` |
| `player.device_name` | string | none | preferred embedded/connect device name |
| `player.audio_output_device` | string | system default | local audio output the embedded player renders to; match a name from `spotuify audio-outputs` |
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

:::note[Legacy `[spotifyd]` migration]
Old configs with `[spotifyd] device_name = "..."` are still honored as a
fallback for `player.device_name`, so an upgrade won't lose your device name.
Use `player.device_name` going forward.
:::

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
enabled = true
source = "auto"
target_fps = 30
smoothing = 0.5
noise_gate = 0.005
color_scheme = "spotify-green"
```

The visualizer ships on by default. Set `enabled = false` to opt out.
It animates from the embedded librespot sink tap; when no audio is
playing the spectrum draws a flat baseline. Toggle it off if you want
the player to use that vertical space for queue items instead.

## Environment variables

```bash
SPOTUIFY_CLIENT_ID=... spotuify login
SPOTUIFY_CLIENT_SECRET=... spotuify login
SPOTUIFY_REDIRECT_URI=http://127.0.0.1:8888/callback spotuify login
```

For local development and tests:

```bash
# Skip the proactive scope-drift keychain read at daemon startup
# (still reads the token on the first real API call). Cuts macOS
# Keychain prompts in half for fresh builds.
SPOTUIFY_SKIP_KEYCHAIN_ON_START=1 spotuify daemon start

# Run the whole stack against fake Spotify data; never touches the
# keychain. Honored by the CLI, daemon, and TUI uniformly.
SPOTUIFY_FAKE_SPOTIFY=1 spotuify
```

## One-shot overrides

```bash
spotuify -o player.bitrate=160 play "ambient"
spotuify -o player.normalization=true play "ambient"
```

Overrides apply only to that command.

## See Also

- [Install](/getting-started/install/)
- [CLI Concepts](/reference/cli/concepts/)
- [Troubleshooting](/reference/troubleshooting/)
