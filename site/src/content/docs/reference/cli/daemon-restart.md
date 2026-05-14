---
title: "spotuify daemon restart"
description: "Restart the daemon with the current binary"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Restart the daemon with the current binary

## Examples

```bash
spotuify daemon restart
```

## Help

```text
Restart the daemon with the current binary

Usage: spotuify daemon restart [OPTIONS]

Options:
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
