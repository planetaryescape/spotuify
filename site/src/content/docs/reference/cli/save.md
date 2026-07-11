---
title: "spotuify save"
description: "Save a Spotify URI or the current now-playing item"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Save a Spotify URI or the current now-playing item

## Examples

```bash
spotuify save current
spotuify save spotify:album:...
```

## Help

```text
Save a Spotify URI or the current now-playing item

Usage: spotuify save [OPTIONS] <TARGET>

Arguments:
  <TARGET>  Spotify URI or `current`

Options:
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --wait                     Block until the daemon confirms the save with Spotify (non-zero exit if it fails). Default is fire-and-forget
      --format <FORMAT>          Output format for the mutation receipt [default: table] [possible values: table, json, jsonl, csv, ids]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
