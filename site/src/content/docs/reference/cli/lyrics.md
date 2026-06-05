---
title: "spotuify lyrics"
description: "Synced lyrics operations"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Synced lyrics operations

## Examples

```bash
spotuify lyrics show
spotuify lyrics follow
```

## Help

```text
Synced lyrics operations

Usage: spotuify lyrics [OPTIONS] <COMMAND>

Commands:
  show    Print lyrics for the current or specified track
  follow  Follow synced lyrics for the current track
  fetch   Force-refresh cached lyrics for a Spotify track URI
  export  Export lyrics as an LRC file
  offset  Save a per-track lyrics timing offset, e.g. +50ms or -200ms
  help    Print this message or the help of the given subcommand(s)

Options:
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
