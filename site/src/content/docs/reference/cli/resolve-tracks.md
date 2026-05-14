---
title: "spotuify resolve-tracks"
description: "Resolve playlist-plan track candidates"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Resolve playlist-plan track candidates

## Examples

```bash
spotuify resolve-tracks --from plan.json --format jsonl > candidates.jsonl
```

## Help

```text
Resolve playlist-plan track candidates

Usage: spotuify resolve-tracks [OPTIONS] --from <FROM>

Options:
      --from <FROM>              Playlist plan JSON file
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --format <FORMAT>          Output format [default: jsonl] [possible values: table, json, jsonl, csv, ids]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
