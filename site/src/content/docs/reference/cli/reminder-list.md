---
title: "spotuify reminder list"
description: "List reminder schedules"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

List reminder schedules

## Examples

```bash
spotuify reminder list
spotuify reminder list --all --format json
```

## Help

```text
List reminder schedules

Usage: spotuify reminder list [OPTIONS]

Options:
      --all              Include inactive reminders
      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
