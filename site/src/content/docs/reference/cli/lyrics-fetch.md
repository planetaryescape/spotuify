---
title: "spotuify lyrics fetch"
description: "Force-refresh cached lyrics for a Spotify track URI"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Force-refresh cached lyrics for a Spotify track URI

## Examples

```bash
spotuify lyrics fetch spotify:track:...
```

## Help

```text
Force-refresh cached lyrics for a Spotify track URI

Usage: spotuify lyrics fetch [OPTIONS] <TRACK_URI>

Arguments:
  <TRACK_URI>  Spotify track URI

Options:
      --format <FORMAT>          Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
