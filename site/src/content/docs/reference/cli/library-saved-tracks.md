---
title: "spotuify library saved-tracks"
description: "Print liked songs (live `/me/tracks`, with date added)"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Print liked songs (live `/me/tracks`, with date added)

## Examples

```bash
spotuify library saved-tracks --limit 50 --format json
```

## Help

```text
Print liked songs (live `/me/tracks`, with date added)

Usage: spotuify library saved-tracks [OPTIONS]

Options:
      --limit <LIMIT>            [default: 50]
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
      --offset <OFFSET>          [default: 0]
      --format <FORMAT>          [default: table] [possible values: table, json, jsonl, csv, ids]
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
