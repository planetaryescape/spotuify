---
title: "spotuify daemon"
description: "Manage the local spotuify daemon"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Manage the local spotuify daemon

## Examples

```bash
spotuify daemon status
spotuify daemon start --foreground
```

## Help

```text
Manage the local spotuify daemon

Usage: spotuify daemon [OPTIONS] <COMMAND>

Commands:
  start              Start the daemon
  stop               Stop the daemon
  restart            Restart the daemon with the current binary
  status             Show daemon socket and lifecycle status
  install-service    Install the platform-appropriate auto-start service (launchd / systemd user / Windows Task Scheduler)
  uninstall-service  Remove the auto-start service registration
  help               Print this message or the help of the given subcommand(s)

Options:
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
