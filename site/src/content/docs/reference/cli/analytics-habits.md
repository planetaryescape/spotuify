---
title: "spotuify analytics habits"
description: "Habit metrics bucketed by day / week / month"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Habit metrics bucketed by day / week / month

## Examples

```bash
spotuify analytics habits --window week --format json
```

## Help

```text
Habit metrics bucketed by day / week / month

Usage: spotuify analytics habits [OPTIONS]

Options:
      --window <WINDOW>  day, week, or month [default: week]
      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
