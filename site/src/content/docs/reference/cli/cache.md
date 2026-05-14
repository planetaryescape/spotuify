---
title: "spotuify cache"
description: "Inspect local cache state"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Inspect local cache state

## Examples

```bash
spotuify cache status
spotuify cache repair
```

## Help

```text
Inspect local cache state

Usage: spotuify cache [OPTIONS] <COMMAND>

Commands:
  status  Show local cache row counts and freshness
  reset   Delete local SQLite cache and search index. Requires --confirm
  repair  Replay cache migrations and rebuild the local search index
  help    Print this message or the help of the given subcommand(s)

Options:
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
