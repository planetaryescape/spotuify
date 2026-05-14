---
title: "spotuify analytics top"
description: "Top-N most-played tracks / artists / albums / playlists"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Top-N most-played tracks / artists / albums / playlists

## Examples

```bash
spotuify analytics top --kind tracks --since 30d --limit 25
```

## Help

```text
Top-N most-played tracks / artists / albums / playlists

Usage: spotuify analytics top [OPTIONS]

Options:
      --kind <KIND>      tracks, artists, albums, or playlists [default: tracks]
      --since <SINCE>    Time window: 7d, 30d, 90d, 365d, or all [default: 30d]
      --limit <LIMIT>    Maximum rows to print [default: 25]
      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
