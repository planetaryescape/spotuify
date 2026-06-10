---
title: "spotuify notifications"
description: "View and act on reminder notifications"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

View and act on reminder notifications

## Examples

```bash
spotuify notifications list
```

## Help

```text
View and act on reminder notifications

Usage: spotuify notifications [OPTIONS] <COMMAND>

Commands:
  list     List inbox notifications
  play     Play the media for a notification
  queue    Queue the media for a notification
  snooze   Snooze a notification
  dismiss  Dismiss a notification without playing
  help     Print this message or the help of the given subcommand(s)

Options:
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
