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
spotuify config set player.event_hook "/Users/me/bin/spotuify-listen-hook"
```

The hook can scrobble to ListenBrainz, post a now-playing notification, or feed your own logs. Keep it fast; hooks have timeouts so playback is not held hostage.

## Export and import

```bash
spotuify analytics export --target listenbrainz --since 2026-01-01
spotuify analytics import --target lastfm
```

## See Also

- [JSON Output](/reference/json-output/)
- [Analytics CLI](/reference/cli/analytics/)
- [Config](/reference/config/)
