---
title: "spotuify volume"
description: "Set playback volume percent"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Set playback volume percent

## Examples

```bash
spotuify volume 70
```

## Help

```text
Set playback volume percent

Usage: spotuify volume [OPTIONS] <PERCENT>

Arguments:
  <PERCENT>  Volume percent, clamped to 0..100

Options:
      --format <FORMAT>          Output format for the mutation receipt [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
