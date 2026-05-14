---
title: "spotuify analytics prune"
description: "Apply retention prune to raw events and progress samples"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Apply retention prune to raw events and progress samples

## Examples

```bash
spotuify analytics prune
spotuify analytics prune --apply
```

## Help

```text
Apply retention prune to raw events and progress samples

Usage: spotuify analytics prune [OPTIONS]

Options:
      --apply            Actually delete rows. Without this flag, print a dry-run report
      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
