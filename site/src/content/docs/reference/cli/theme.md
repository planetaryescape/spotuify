---
title: "spotuify theme"
description: "Show, list, or switch the terminal colour theme"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Show, list, or switch the terminal colour theme

## Examples

```bash
spotuify theme
spotuify theme list --format ids
spotuify theme winamp
spotuify theme path
```

## Help

```text
Show, list, or switch the terminal colour theme

Usage: spotuify theme [OPTIONS] [NAME]

Arguments:
  [NAME]  Theme name to apply, `list` to list them all, or `path` to print the user themes directory. Omit to show the active theme

Options:
      --format <FORMAT>          Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
