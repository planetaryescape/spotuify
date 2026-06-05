---
title: "Troubleshooting"
description: "Fix auth, daemon, device, cache, search, lyrics, and visualizer issues."
---

Start with `doctor`. It is the shortest path to useful failure data.

## Run doctor

```bash
spotuify doctor
spotuify doctor --format json
```

## Daemon unavailable

```bash
spotuify daemon status
spotuify daemon start
spotuify logs tail 200
```

If a script must fail instead of starting the daemon:

```bash
spotuify --no-daemon-start status
```

## Auth failure

If you see `not logged in; run spotuify login`, do exactly that:

```bash
spotuify login
spotuify doctor
```

`spotuify login` opens the browser, and the daemon mints a Web API token
from your stored OAuth credentials. Configure a Spotify Developer app
`client_id` first if the config does not have one yet.

### 403 on playlist writes

If playlist or library writes return `403`, your Spotify app is probably
still in Development Mode. Apply for Extended Quota Mode in the Spotify
dashboard.

```bash
spotuify auth status
spotuify doctor
```

## Permissions out of date

The TUI shows the banner *"Spotify permissions out of date. Quit,
run `spotuify logout && spotuify login`, then restart."* when a token was
issued before a scope that newer features require, like follow/unfollow
or playlist add. The fix is exactly what the banner says:

```bash
spotuify logout
spotuify login
```

## Auth file issues

`spotuify` stores OAuth credentials in the private auth directory:
`<config_dir>/auth/token.json` for the default dev-app flow and
`<config_dir>/auth/first-party.json` for first-party/keymaster auth.
Files are written with mode `0600` on Unix.

If auth looks wrong, inspect the current paths first:

```bash
spotuify config path
spotuify doctor
```

Reset the auth file by recreating the token:

```bash
spotuify daemon stop
spotuify logout
spotuify login
spotuify daemon start
```

For local development and tests, use fake mode when you do not want live
Spotify auth at all:

```bash
SPOTUIFY_FAKE_SPOTIFY=1 spotuify
```

## No active device

```bash
spotuify devices --format json
spotuify transfer spotuify-hume
spotuify play "imagine dragons"
```

The daemon should expose its embedded librespot device even when Spotify's
device registry lags. If the device list is empty, start the daemon and
reconnect:

```bash
spotuify daemon restart
spotuify reconnect
spotuify devices
```

### Can't transfer to an Echo / Alexa speaker

Amazon Echo and other Alexa-controlled speakers appear in `spotuify devices`,
but Spotify's Web API routinely refuses to *start* playback on them from a
third-party client, so `transfer` returns `404 Not found`. Wake the device via
Alexa (or the Spotify app) first, then transfer while it's in an active
session:

```bash
# Start anything on the Echo via Alexa, then:
spotuify transfer "Office Echo"
```

## Search looks empty

```bash
spotuify sync library
spotuify cache status --format json
spotuify reindex
spotuify search "test" --source local
```

## Cache looks broken

```bash
spotuify cache repair
spotuify cache status
```

Last resort:

```bash
spotuify cache reset --confirm
spotuify sync all
```

## Lyrics are missing

```bash
spotuify lyrics show
spotuify lyrics follow --lines 3
spotuify lyrics fetch spotify:track:...
spotuify refresh-media
spotuify lyrics offset spotify:track:... +50ms
```

Lyrics depend on configured providers and cache state. Spotify Web API itself does not guarantee lyrics.

`lyrics follow` requires an active track with synced lyric timestamps. If it
prints `synced lyrics unavailable; use spotuify lyrics show`, the track has
plain lyrics but no timing data for karaoke-style following.

In the TUI, press `U` to refetch the current track's cover art and lyrics.
The current display is not cleared while the new fetch is running.

## Visualizer is blank

```bash
spotuify viz status --format json
spotuify viz source auto
spotuify viz enable
```

On macOS loopback capture needs a virtual device such as BlackHole unless the embedded sink tap is active.

## Bug report

```bash
spotuify bug-report --log-lines 500 --output spotuify-report.tar.gz
```

The bundle is local. Inspect it before sharing.

## See Also

- [Player and Daemon](/guides/player-and-daemon/)
- [Config](/reference/config/)
- [IPC Protocol](/reference/ipc/)
