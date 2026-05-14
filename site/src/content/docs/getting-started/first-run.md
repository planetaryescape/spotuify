---
title: "First Run"
description: "Understand onboarding, config, OAuth, daemon startup, and doctor."
---

The first run should either open the app or tell you exactly what is missing. No blank screens, no silent auth failure.

## Let onboarding drive

```bash
spotuify onboard
```

What you get: config creation, Spotify OAuth, and the first sync path in one flow.

## Inspect the config path

```bash
spotuify config path
spotuify config get redirect_uri
```

By default the config lives under the platform config directory as `spotuify/spotuify.toml`. You can point one invocation at another file:

```bash
SPOTUIFY_CONFIG=/tmp/spotuify.toml spotuify config path
```

## Run doctor before debugging the TUI

```bash
spotuify doctor --format json
```

What you get: a structured health report. Use this first for auth, daemon, device, Spotify API, cache, and log-path problems.

## Verify device control

```bash
spotuify devices
spotuify transfer spotuify-hume
spotuify status
```

## Verify local cache

```bash
spotuify sync library --format json
spotuify cache status --format json
spotuify search "liked" --source local --format jsonl
```

## Open the TUI

```bash
spotuify
```

Quit the TUI with `q`. The daemon and playback continue.

## See Also

- [Install](/getting-started/install/)
- [Player and Daemon](/guides/player-and-daemon/)
- [Troubleshooting](/reference/troubleshooting/)
