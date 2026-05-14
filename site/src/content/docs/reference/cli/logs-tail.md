---
title: "spotuify logs tail"
description: "Print recent log lines"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Print recent log lines

## Examples

```bash
spotuify logs tail 200
spotuify logs tail --follow --format jsonl
```

## Help

```text
Print recent log lines

Usage: spotuify logs tail [OPTIONS] [LINES]

Arguments:
  [LINES]  Number of lines to print [default: 80]

Options:
      --follow                   Phase 13 (P13-C) - keep printing as new lines arrive (poll the log file every 500ms; Ctrl-C to exit)
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --format <FORMAT>          Output format: text (default), json/jsonl (pass-through) [default: text] [possible values: text, json, jsonl]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
