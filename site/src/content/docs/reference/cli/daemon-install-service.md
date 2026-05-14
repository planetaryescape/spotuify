---
title: "spotuify daemon install-service"
description: "Install the platform auto-start service for the spotuify daemon"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Install the platform auto-start service for the spotuify daemon

## Examples

```bash
spotuify daemon install-service
```

## Help

```text
Install the platform auto-start service for the spotuify daemon

Usage: spotuify daemon install-service [OPTIONS]

Options:
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
