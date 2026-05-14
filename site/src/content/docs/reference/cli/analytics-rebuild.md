---
title: "spotuify analytics rebuild"
description: "Recompute derived listen facts from analytics_events"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Recompute derived listen facts from analytics_events

## Examples

```bash
spotuify analytics rebuild
spotuify analytics rebuild --since 2026-05-01T00:00:00Z
```

## Help

```text
Recompute derived listen facts from analytics_events

Usage: spotuify analytics rebuild [OPTIONS]

Options:
      --since <SINCE>    ISO timestamp to rebuild from; omit for full rebuild
      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
