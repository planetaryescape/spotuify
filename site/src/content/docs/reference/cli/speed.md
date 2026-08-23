---
title: "spotuify speed"
description: "Show or set the podcast playback speed (0.5x-3.5x; music always plays at 1x)"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Show or set the podcast playback speed (0.5x-3.5x; music always plays at 1x)

## Examples

```bash
spotuify speed
spotuify speed 1.5x
spotuify speed +
spotuify speed --format json
```

## Help

```text
Show or set the podcast playback speed (0.5x-3.5x; music always plays at 1x)

Usage: spotuify speed [OPTIONS] [RATE]

Arguments:
  [RATE]  New speed, e.g. `1.5`, `1.5x`, `150%`, `+` (one notch faster), or `-`

Options:
      --format <FORMAT>          [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
