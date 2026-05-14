---
title: "spotuify library"
description: "Cached library operations"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Cached library operations

## Examples

```bash
spotuify library tracks
```

## Help

```text
Cached library operations

Usage: spotuify library [OPTIONS] <COMMAND>

Commands:
  tracks  Print cached saved tracks and albums
  help    Print this message or the help of the given subcommand(s)

Options:
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
