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
      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
