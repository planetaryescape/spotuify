---
title: "spotuify ops show"
description: "Inspect a single operation by id"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Inspect a single operation by id

## Examples

```bash
spotuify ops show 018f... --diff
```

## Help

```text
Inspect a single operation by id

Usage: spotuify ops show [OPTIONS] <ID>

Arguments:
  <ID>  Operation id

Options:
      --diff             Render a human-readable diff of what undo would do
      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
