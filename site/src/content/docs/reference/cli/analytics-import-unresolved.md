---
title: "spotuify analytics import unresolved"
description: "List unresolved scrobbles for a run"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

List unresolved scrobbles for a run

## Examples

```bash
spotuify analytics import unresolved 018f... --format json
```

## Help

```text
List unresolved scrobbles for a run

Usage: spotuify analytics import unresolved [OPTIONS] <RUN_ID>

Arguments:
  <RUN_ID>

Options:
      --format <FORMAT>          [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
