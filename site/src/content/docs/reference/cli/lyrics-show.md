---
title: "spotuify lyrics show"
description: "Print lyrics for the current or specified track"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Print lyrics for the current or specified track

## Examples

```bash
spotuify lyrics show
spotuify lyrics show --track spotify:track:... --format json
```

## Help

```text
Print lyrics for the current or specified track

Usage: spotuify lyrics show [OPTIONS]

Options:
      --track <TRACK>    Spotify track URI. Defaults to the current now-playing track
      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
