---
title: "spotuify update"
description: "Check whether a newer spotuify release is available and how to upgrade"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Check whether a newer spotuify release is available and how to upgrade

## Examples

```bash
spotuify update
spotuify update --force --format json
```

## Help

```text
Check whether a newer spotuify release is available and how to upgrade

Usage: spotuify update [OPTIONS]

Options:
      --force                    Force a fresh check now instead of returning the cached result
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --format <FORMAT>          Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
