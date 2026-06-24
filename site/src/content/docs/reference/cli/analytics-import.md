---
title: "spotuify analytics import"
description: "Import historical scrobbles"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Import historical scrobbles

## Examples

```bash
spotuify analytics import lastfm --user your-lastfm-user --from 2024-01-01 --format json
spotuify analytics import status 018f...
spotuify analytics import undo 018f... --dry-run
```

## Help

```text
Import historical scrobbles

Usage: spotuify analytics import [OPTIONS] [COMMAND]

Commands:
  lastfm      Preview/apply Last.fm historical scrobble import
  status      Show import run status
  unresolved  List unresolved scrobbles for a run
  undo        Undo promoted analytics effects while preserving raw scrobbles
  help        Print this message or the help of the given subcommand(s)

Options:
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --target <TARGET>          Compatibility alias: `analytics import --target lastfm`
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
      --format <FORMAT>          [default: table] [possible values: table, json, jsonl, csv, ids]
  -h, --help                     Print help
```
