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

## Shell hooks

Set a hook command in config:

```bash
spotuify config set analytics.hook_command "/Users/me/bin/spotuify-listen-hook"
```

The hook can scrobble to ListenBrainz, post a now-playing notification, or feed your own logs. Keep it fast; hooks have timeouts so playback is not held hostage.

Hook commands are executed by the shell exactly as configured. Track data is passed through `SPOTUIFY_*` environment variables; it is not interpolated into the command string.

## Export and import status

```bash
spotuify analytics export --help
spotuify analytics import --help
```

These commands are reserved for a future provider bridge and currently return a clear follow-up error. Use shell hooks for live ListenBrainz, Last.fm, Discord, or custom integrations.

## See Also

- [JSON Output](/reference/json-output/)
- [Analytics CLI](/reference/cli/analytics/)
- [Config](/reference/config/)
