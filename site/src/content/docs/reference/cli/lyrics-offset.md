---
title: "spotuify lyrics offset"
description: "Save a per-track lyrics timing offset"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Save a per-track lyrics timing offset

## Examples

```bash
spotuify lyrics offset spotify:track:... +50ms
```

## Help

```text
Save a per-track lyrics timing offset

Usage: spotuify lyrics offset [OPTIONS] <TRACK_URI> <OFFSET>

Arguments:
  <TRACK_URI>  Spotify track URI
  <OFFSET>     Offset in milliseconds, with optional ms suffix

Options:
      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
