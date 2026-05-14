---
title: "spotuify playlist create"
description: "Create a playlist from resolved candidates"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Create a playlist from resolved candidates

## Examples

```bash
spotuify playlist create "Exile and Return" --from candidates.jsonl --dry-run
spotuify playlist create "Exile and Return" --from candidates.jsonl --yes --format json
```

## Help

```text
Create a playlist from resolved candidates

Usage: spotuify playlist create [OPTIONS] --from <FROM> <NAME>

Arguments:
  <NAME>  New playlist name

Options:
      --from <FROM>              Resolved candidates JSONL file
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --dry-run                  Show the exact mutation without creating the playlist
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
      --yes                      Commit the playlist creation without an interactive prompt
      --format <FORMAT>          Output format [default: table] [possible values: table, json, jsonl, csv, ids]
  -h, --help                     Print help
```
