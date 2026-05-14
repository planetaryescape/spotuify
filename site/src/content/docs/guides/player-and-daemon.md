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

## Playback continues after the TUI exits

```bash
spotuify
```

Start music, quit with `q`, then check:

```bash
spotuify status
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
