---
title: "First Run"
description: "Understand onboarding, the browser login, config, daemon startup, and doctor."
---

The first run should either open the app or tell you exactly what is missing. No blank screens, no silent auth failure.

## Let onboarding drive

```bash
spotuify onboard
```

What you get: config creation, a browser login, and the first sync path in one flow.

There is no Spotify Developer app to register and no Client ID to paste. spotuify logs in with Spotify's first-party flow and mints a full-access Web API token from your session, so creating playlists and saving tracks work out of the box. Premium is required for playback. (Power users can point spotuify at their own Spotify app with `SPOTUIFY_CLIENT_ID`; see [Install](/getting-started/install/).)

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

The first screen is Home: saved music, podcasts, recent plays, and a queue
panel when a Spotify session is active. If nothing is playing, Space starts the
selected Home item. Quit the TUI with `q`; the daemon and playback continue.

## See Also

- [Install](/getting-started/install/)
- [Player and Daemon](/guides/player-and-daemon/)
- [Troubleshooting](/reference/troubleshooting/)
