---
title: "spotuify analytics rediscovery"
description: "Tracks worth re-discovering"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Tracks worth re-discovering

## Examples

```bash
spotuify analytics rediscovery --gap 90d
```

## Help

```text
Tracks worth re-discovering

Usage: spotuify analytics rediscovery [OPTIONS]

Options:
      --gap <GAP>        Rediscovery gap: 30d, 90d, or 365d [default: 90d]
      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
