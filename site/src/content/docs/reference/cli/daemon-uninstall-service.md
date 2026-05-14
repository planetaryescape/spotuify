---
title: "spotuify daemon uninstall-service"
description: "Remove the platform auto-start service for the spotuify daemon"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Remove the platform auto-start service for the spotuify daemon

## Examples

```bash
spotuify daemon uninstall-service
```

## Help

```text
Remove the platform auto-start service for the spotuify daemon

Usage: spotuify daemon uninstall-service [OPTIONS]

Options:
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
