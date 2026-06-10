---
title: "spotuify notifications dismiss"
description: "Dismiss a notification without playing"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Dismiss a notification without playing

## Examples

```bash
spotuify notifications dismiss <notification-id>
```

## Help

```text
Dismiss a notification without playing

Usage: spotuify notifications dismiss [OPTIONS] <ID>

Arguments:
  <ID>  Notification id

Options:
      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
