---
title: "spotuify library shows"
description: "Print subscribed podcasts (saved shows)"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Print subscribed podcasts (saved shows)

## Examples

```bash
spotuify library shows --limit 50 --format json
```

## Help

```text
Print subscribed podcasts (saved shows)

Usage: spotuify library shows [OPTIONS]

Options:
      --limit <LIMIT>            [default: 200]
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
      --provider <PROVIDER>      Provider to target (defaults to the daemon's default provider)
      --format <FORMAT>          [default: table] [possible values: table, json, jsonl, csv, ids]
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
