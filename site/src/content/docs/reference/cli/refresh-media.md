---
title: "spotuify refresh-media"
description: "Refresh current track cover art and lyrics"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Refresh current track cover art and lyrics

## Examples

```bash
spotuify refresh-media
spotuify refresh-media --format json
```

## Help

```text
Refresh current track cover art and lyrics

Usage: spotuify refresh-media [OPTIONS]

Options:
      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
