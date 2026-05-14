---
title: "spotuify ops"
description: "Inspect, undo, or redo recorded operations"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Inspect, undo, or redo recorded operations

## Examples

```bash
spotuify ops log
spotuify ops undo --dry-run
```

## Help

```text
Inspect, undo, or redo recorded operations

Usage: spotuify ops [OPTIONS] <COMMAND>

Commands:
  log   List recorded operations, newest first
  show  Inspect a single operation by id
  undo  Undo a recorded operation; defaults to the last reversible op
  redo  Redo a previously-undone operation
  help  Print this message or the help of the given subcommand(s)

Options:
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
