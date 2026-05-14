---
title: "spotuify analytics search"
description: "Search history with raw or normalized query mode"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Search history with raw or normalized query mode

## Examples

```bash
spotuify analytics search --mode normalized --limit 20
```

## Help

```text
Search history with raw or normalized query mode

Usage: spotuify analytics search [OPTIONS]

Options:
      --mode <MODE>      raw or normalized [default: raw]
      --limit <LIMIT>    Maximum rows to print [default: 50]
      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
