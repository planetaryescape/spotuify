---
title: "spotuify queue add"
description: "Add an item to the current queue"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Add an item to the current queue

## Examples

```bash
spotuify queue add spotify:track:...
spotuify queue add --search "never too much"
spotuify search "luther vandross" --format ids | spotuify queue add --format json
```

## Help

```text
Add an item to the current queue

Usage: spotuify queue add [OPTIONS] [URIS]...

Arguments:
  [URIS]...  Spotify URI(s) to queue

Options:
      --ids <FILE>               Read Spotify URI(s) from a file, or `-` for stdin
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
      --search <SEARCH>          Search for a track and queue the first result
      --format <FORMAT>          Output format for the mutation receipt [default: table] [possible values: table, json, jsonl, csv, ids]
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
