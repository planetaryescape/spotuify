---
title: "spotuify reminder create"
description: "Schedule a listening reminder for any media URI"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Schedule a listening reminder for any media URI

## Examples

```bash
spotuify reminder create spotify:album:... --at +3d --message "come back to this"
```

## Help

```text
Schedule a listening reminder for any media URI

Usage: spotuify reminder create [OPTIONS] <URI>

Arguments:
  <URI>  Spotify URI to be reminded about

Options:
      --at <AT>          When to fire: +2h, +30m, +3d, +1w, tomorrow, or an ISO-8601 datetime
      --repeat <REPEAT>  Repeat cadence [default: none]
      --message <TEXT>   Optional note shown with the reminder
      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
