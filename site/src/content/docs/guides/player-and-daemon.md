---
title: "Player and Daemon"
description: "Understand playback, devices, daemon lifecycle, and recovery commands."
---

The player is the product. If `next`, `pause`, device transfer, or search-to-play is flaky, nothing else matters.

## Device model

`spotuify` controls Spotify Connect devices. The preferred local device on this machine is `spotuify-hume`.

```bash
spotuify devices
spotuify transfer spotuify-hume
spotuify status
```

## Choose the local audio output

`spotuify-hume` is the embedded player running on this machine. It renders to your system's default audio output unless you pick one. List the outputs and select one:

```bash
spotuify audio-outputs                          # list local outputs
spotuify audio-output "MacBook Pro Speakers"    # set it + restart the player
spotuify audio-output default                   # follow the system default again
```

What you get: the choice persisted as `player.audio_output_device` and the player restarted so audio routes there. In the TUI, press `O` for the same picker.

This is the *local* output (which speaker on this Mac), not the Connect target. To play on another Connect device (a phone, an Echo), use `spotuify transfer` instead.

## Daemon lifecycle

```bash
spotuify daemon start
spotuify daemon status
spotuify daemon stop
```

Install the user service:

```bash
spotuify daemon install-service
```

Remove it:

```bash
spotuify daemon uninstall-service
```

`install-service` registers a user-level service for the current platform: launchd on macOS, systemd user units on Linux, and a Task Scheduler logon trigger on Windows.

## Upgrading the daemon

The daemon is long-lived, so a freshly-installed binary doesn't take effect until it restarts. Any CLI command (or launching the TUI) detects a version mismatch and restarts the stale daemon for you. The exception is mid-playback: it leaves the running daemon alone so your audio isn't cut, and prints a note to restart when ready:

```bash
brew update
brew upgrade planetaryescape/spotuify/spotuify
spotuify daemon restart
```

If a TUI is already open when you upgrade, it shows an `Update installed` banner; press `R` to restart the daemon onto the new build without quitting.

## Playback continues after the TUI exits

```bash
spotuify
```

Start music, quit with `q`, then check:

```bash
spotuify status
```

## Playback controls are hot

Play/pause, next, previous, seek, and volume are treated as hot-path actions.
The daemon updates its playback clock optimistically for subscribers, then tries
the embedded player's local transport path before waiting on Spotify Web API
reconciliation. That is why pressing Space in the TUI should feel immediate
even when Spotify's API response is slow.

## Opening the TUI is not a refresh

The TUI seeds Home from cached daemon state. It does not fetch playback,
devices, queue, playlists, or library data from Spotify just because a window
opened. That keeps provider budget available for the action you actually take,
like play, transfer, queue, or search.

Force fresh data when you need it:

```bash
spotuify sync library
spotuify playlists --format json
spotuify doctor
```

## Recover a stale session

```bash
spotuify reconnect
spotuify devices
spotuify play "imagine dragons"
```

Use this after sleep/wake, VPN changes, or a Spotify session that stopped responding.

## Reload config

```bash
spotuify config set player.bitrate 320
spotuify reload
```

## Failure rule

No raw Spotify error should be the final user experience. If playback fails, run:

```bash
spotuify doctor
spotuify daemon status --format json
spotuify logs tail 200
```

## See Also

- [Architecture](/guides/architecture/)
- [Troubleshooting](/reference/troubleshooting/)
- [Daemon CLI](/reference/cli/daemon/)
