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
spotuify lyrics fetch spotify:track:...
```

## Help

```text
Synced lyrics operations

Usage: spotuify lyrics [OPTIONS] <COMMAND>

Commands:
  show    Print lyrics for the current or specified track
  fetch   Force-refresh cached lyrics for a Spotify track URI
  offset  Save a per-track lyrics timing offset
  help    Print this message or the help of the given subcommand(s)

Options:
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
