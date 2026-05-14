---
title: "spotuify analytics import"
description: "Import historical scrobbles from ListenBrainz or Last.fm"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Import historical scrobbles from ListenBrainz or Last.fm

## Examples

```bash
spotuify analytics import --target lastfm
```

## Help

```text
Import historical scrobbles from ListenBrainz or Last.fm

Usage: spotuify analytics import [OPTIONS]

Options:
      --target <TARGET>  Import target [possible values: listenbrainz, lastfm]
      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
