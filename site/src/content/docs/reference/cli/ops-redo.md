---
title: "spotuify ops redo"
description: "Redo a previously-undone operation"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Redo a previously-undone operation

## Examples

```bash
spotuify ops redo 018f...
```

## Help

```text
Redo a previously-undone operation

Usage: spotuify ops redo [OPTIONS] [ID]

Arguments:
  [ID]  Operation id. Omit to redo the last undone op

Options:
      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
