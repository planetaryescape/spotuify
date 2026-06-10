---
title: "spotuify notifications queue"
description: "Queue the media for a notification"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Queue the media for a notification

## Examples

```bash
spotuify notifications queue <notification-id>
```

## Help

```text
Queue the media for a notification

Usage: spotuify notifications queue [OPTIONS] <ID>

Arguments:
  <ID>  Notification id

Options:
      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
