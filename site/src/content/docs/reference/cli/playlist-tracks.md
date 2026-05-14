---
title: "spotuify playlist tracks"
description: "Print playlist tracks"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Print playlist tracks

## Examples

```bash
spotuify playlist tracks "Quiet Storm" --format jsonl
```

## Help

```text
Print playlist tracks

Usage: spotuify playlist tracks [OPTIONS] <PLAYLIST>

Arguments:
  <PLAYLIST>  Playlist ID, URI, or exact name

Options:
      --format <FORMAT>          Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
