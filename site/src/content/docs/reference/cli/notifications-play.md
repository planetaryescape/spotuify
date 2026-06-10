---
title: "spotuify notifications play"
description: "Play the media for a notification"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Play the media for a notification

## Examples

```bash
spotuify notifications play <notification-id>
```

## Help

```text
Play the media for a notification

Usage: spotuify notifications play [OPTIONS] <ID>

Arguments:
  <ID>  Notification id

Options:
      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
