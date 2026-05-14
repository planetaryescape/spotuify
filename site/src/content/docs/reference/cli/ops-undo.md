---
title: "spotuify ops undo"
description: "Undo a recorded operation; defaults to the last reversible op"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Undo a recorded operation; defaults to the last reversible op

## Examples

```bash
spotuify ops undo --dry-run
spotuify ops undo 018f... --yes
```

## Help

```text
Undo a recorded operation; defaults to the last reversible op

Usage: spotuify ops undo [OPTIONS] [ID]

Arguments:
  [ID]  Operation id. Omit to undo the last reversible op

Options:
      --dry-run          Predict the reversal without executing
      --yes              Skip confirmation prompts
      --force            Override snapshot-id conflict detection
      --since <SINCE>    Bulk-undo every reversible op newer than this
      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
