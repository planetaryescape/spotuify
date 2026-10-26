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

## Installed binary and daemon differ

Check both version surfaces after an upgrade:

```bash
spotuify --version
spotuify daemon status --format json
```

`--no-daemon-start` also refuses a stale running daemon. Restart when you are ready
to interrupt that instance's playback, then retry the read:

```bash
spotuify daemon restart
spotuify --no-daemon-start status --format json
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

Read the error's reason before changing credentials. `403` alone does not tell
you whether the cause is a missing OAuth scope, app/user access, resource
permissions, or a device restriction. `spotuify` distinguishes named missing
scopes from other provider denials; a generic write hint is not a confirmed cause.

If the error names a missing scope, sign in again with the required permission.
For app access, check the account allowlist and endpoint availability in the
Spotify dashboard. Extended quota access has separate eligibility requirements;
it is not an automatic repair for a denied write. Check Spotify's
[current quota-mode rules](https://developer.spotify.com/documentation/web-api/concepts/quota-modes).

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
`<config_dir>/auth/spotify/token.json` for the dev-app flow and
`<config_dir>/auth/spotify/first-party.json` for first-party/keymaster auth.
Older unscoped auth files are migration inputs, not the current storage location.
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
SPOTUIFY_FAKE_SPOTIFY=1 SPOTUIFY_CLIENT_ID=fake-client-id spotuify
```

### Login redirect fails with `localhost`

Spotify's November 2025 OAuth migration only accepts the literal
`http://127.0.0.1:<port>/callback` loopback redirect - `localhost` and
`::1` are rejected. The default config already uses `127.0.0.1`; if you
overrode `redirect_uri`, fix it and update the redirect URI registered
in your Spotify Developer app to match:

```bash
spotuify config set redirect_uri http://127.0.0.1:8888/callback
spotuify login
```

`spotuify doctor` flags a `localhost`/`::1` redirect host as a warning.

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

## Music shows playing but is silent on macOS

Check whether the embedded session is connected and whether decoded PCM samples
are still moving:

```bash
spotuify daemon status --format json | jq .audio_health
spotuify logs tail 200
```

`connected: true` with `is_playing: true` and `samples_advancing: false` after
6 seconds means the Spotify session is alive but the local audio sink is not
making progress.

Isolate macOS audio from `spotuify` before changing player code:

```bash
spotuify daemon status --format json   # record this instance's daemon_pid
spotuify daemon stop
spotuify daemon status --format json   # running and socket_reachable should be false
/usr/bin/afplay -t 1 /System/Library/Sounds/Glass.aiff
```

Confirm the recorded PID exited before restarting. Other instances, including
`spotuify-dev`, can have their own daemons; an empty global process search is not
the stop condition for this instance.

If `afplay` also hangs, the system audio probe has failed independently of `spotuify`. Reset the system
audio service, then prove `afplay` works before restarting the daemon:

```bash
sudo killall coreaudiod
/usr/bin/afplay -t 1 /System/Library/Sounds/Glass.aiff
spotuify daemon start
spotuify play "imagine dragons"
sleep 8
spotuify daemon status --format json | jq .audio_health
```

The final report should show `connected: true`, `is_playing: true`, and
`samples_advancing: true`.

Since v0.1.102, a native player teardown that takes longer than 2 seconds blocks
replacement audio sinks until a clean daemon restart. If `spotuify reconnect`
says teardown is still blocked, do not loop reconnects. Restore system audio,
then restart the daemon.

## Search looks empty

```bash
spotuify sync library
spotuify cache status --format json
spotuify reindex
spotuify search "test" --source local
```

### Search index reports `LockBusy`

The daemon clears stale Tantivy lock files (`.tantivy-*.lock`) automatically
during startup preflight, so a restart fixes a stuck index lock:

```bash
spotuify daemon restart
```

Manual lock removal under the instance's `search_index` directory is only
needed if the daemon itself refuses to start - and only after stopping that
instance's daemon.

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

## Cover art doesn't render in the terminal

`spotuify doctor` (run in an interactive terminal) prints a `terminal:` line
showing your `TERM`, the cover-art image protocol it expects to use (kitty
graphics, iterm2 inline images, sixel, or unicode half-blocks), and whether
truecolor is available. If it says "unicode half-blocks", your terminal
doesn't advertise an inline-image protocol - switch to kitty, WezTerm, or
iTerm2 for real album art. The line is omitted when output is piped.

## Windows notes

Windows x64 release zips use the same daemon protocol over named pipes instead of Unix sockets. If the daemon looks unavailable, start with the same commands:

```powershell
spotuify daemon status
spotuify daemon restart
spotuify logs tail 200
```

`spotuify daemon install-service` registers a user-level Task Scheduler logon trigger. Global media keys (SMTC) work headlessly: the daemon spawns a hidden message-only window to host the System Media Transport Controls, so play/pause/next from the keyboard and the Windows volume flyout reach spotuify even with no TUI open.

## Bug report

```bash
spotuify bug-report --log-lines 500 --output spotuify-report.tar.gz
```

The bundle is local. Inspect it before sharing.

## See Also

- [Player and Daemon](/guides/player-and-daemon/)
- [Config](/reference/config/)
- [IPC Protocol](/reference/ipc/)
