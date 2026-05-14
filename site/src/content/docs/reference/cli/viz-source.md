---
title: "spotuify viz source"
description: "Select the audio source used by the visualizer"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Select the audio source used by the visualizer

## Examples

```bash
spotuify viz source auto
spotuify viz source loopback
```

## Help

```text
Select the audio source used by the visualizer

Usage: spotuify viz source [OPTIONS] <KIND>

Arguments:
  <KIND>  Source kind: auto, sink, loopback, or none [possible values: auto, sink, loopback, none]

Options:
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
