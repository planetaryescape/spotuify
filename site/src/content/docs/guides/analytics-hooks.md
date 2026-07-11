---
title: "Analytics and Hooks"
description: "Inspect local listening analytics and connect shell hooks."
---

Analytics are local. `spotuify` records playback/search/action events into SQLite, then derives useful listening facts from them.

## Recent events

```bash
spotuify analytics events --limit 50
spotuify analytics events --limit 50 --format jsonl
```

## Top listens

```bash
spotuify analytics top --kind tracks --since 30d --limit 25
spotuify analytics top --kind artists --since all --format json
```

## Habits

```bash
spotuify analytics habits --window week --format json
```

## Rediscovery

```bash
spotuify analytics rediscovery --gap 90d
```

What you get: tracks you listened to before and have not heard recently.

## Rebuild derived facts

```bash
spotuify analytics rebuild
```

Use `--since` for a smaller rebuild:

```bash
spotuify analytics rebuild --since 2026-05-01T00:00:00Z
```

## Import Last.fm history

Use historical import when your Last.fm account already has listening history and you want it in local analytics.

Preview first:

```bash
spotuify analytics import lastfm \
  --user your-lastfm-user \
  --from 2024-01-01 \
  --format json
```

Apply only after the dry-run counts make sense:

```bash
spotuify analytics import lastfm \
  --user your-lastfm-user \
  --from 2024-01-01 \
  --apply \
  --format json
```

Imported scrobbles are marked as `lastfm_scrobble_import`. They count as qualified listens, but their audible time is estimated because Last.fm stores a listen timestamp, not the full playback timeline.

Use the dedicated guide for status, unresolved rows, and undo:

- [Import Last.fm History](/guides/import-lastfm-history/)

## Shell hooks

Set a hook command in config:

```bash
spotuify config set analytics.hook_command "/Users/me/bin/spotuify-listen-hook"
```

The hook can scrobble to ListenBrainz, post a now-playing notification, or feed your own logs. Keep it fast; hooks have timeouts so playback is not held hostage.

The hook is invoked once per event with positional args (and matching
`SPOTUIFY_*` env vars):
## Compatibility alias and export status

```text
<cmd> track-change    <uri> <track> <artist> <album> <duration_ms>
<cmd> playback-paused  <uri> <position_ms>
<cmd> playback-resumed <uri> <position_ms>
<cmd> track-finished   <uri> <reason>
<cmd> listen-qualified <uri> <duration_ms>
```bash
spotuify analytics import --target lastfm
```

Hook commands are executed by the shell exactly as configured. Track data is passed through `SPOTUIFY_*` environment variables; it is not interpolated into the command string.

## Scrobbling to external services

Spotuify does not ship an in-tree provider export/import bridge - it would mean
storing third-party credentials and tracking provider API drift. Instead, the
shell-hook above is the supported path for live ListenBrainz, Last.fm, Discord,
or custom integrations. Ready-to-use scripts live in `docs/recipes/`
(`scrobble-listenbrainz.sh`, `scrobble-lastfm.sh`, `notify-discord-listening.sh`).
`analytics import --target lastfm` is a compatibility alias for dry-run Last.fm import. It reads the Last.fm username and API key from config or environment. New scripts should prefer `analytics import lastfm`.

`analytics export` is still a placeholder. Use shell-hook recipes for live ListenBrainz or Last.fm scrobbling.
## See Also

- [JSON Output](/reference/json-output/)
- [Analytics CLI](/reference/cli/analytics/)
- [Import Last.fm History](/guides/import-lastfm-history/)
- [Config](/reference/config/)
