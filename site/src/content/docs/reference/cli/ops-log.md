---
title: "spotuify ops log"
description: "List recorded operations, newest first"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

List recorded operations, newest first

## Examples

```bash
spotuify ops log --limit 20 --format json
```

## Help

```text
List recorded operations, newest first

Usage: spotuify ops log [OPTIONS]

Options:
      --limit <LIMIT>    Maximum rows to print [default: 20]
      --since <SINCE>    Relative time or ISO timestamp
      --source <SOURCE>  cli, tui, mcp, agent, or daemon-internal
      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
