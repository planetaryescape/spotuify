---
title: "spotuify playlist plan"
description: "Generate a playlist plan JSON scaffold from a brief"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Generate a playlist plan JSON scaffold from a brief

## Examples

```bash
spotuify playlist plan "exile and returning home" --format json > plan.json
```

## Help

```text
Generate a playlist plan JSON scaffold from a brief

Usage: spotuify playlist plan [OPTIONS] <BRIEF>

Arguments:
  <BRIEF>  Plain-language playlist brief

Options:
      --format <FORMAT>          Output format [default: json] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
