---
title: "spotuify notifications snooze"
description: "Snooze a notification"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Snooze a notification

## Examples

```bash
spotuify notifications snooze <notification-id> --for 1h
```

## Help

```text
Snooze a notification

Usage: spotuify notifications snooze [OPTIONS] <ID>

Arguments:
  <ID>  Notification id

Options:
      --for <DURATION>   Snooze duration: 15m, 1h, 4h, or 1d [default: 1h]
      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
