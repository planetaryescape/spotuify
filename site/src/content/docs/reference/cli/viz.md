---
title: "spotuify viz"
description: "Configure the audio visualizer"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Configure the audio visualizer

## Examples

```bash
spotuify viz status
spotuify viz enable
```

## Help

```text
Configure the audio visualizer

Usage: spotuify viz [OPTIONS] <COMMAND>

Commands:
  enable   Enable the TUI spectrum visualizer
  disable  Disable the TUI spectrum visualizer
  source   Select the audio source used by the visualizer
  status   Show visualizer status and diagnostics
  help     Print this message or the help of the given subcommand(s)

Options:
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
