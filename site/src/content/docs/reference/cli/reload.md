---
title: "spotuify reload"
description: "Ask the running daemon to reload config.toml"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Ask the running daemon to reload config.toml

## Examples

```bash
spotuify reload
```

## Help

```text
Ask the running daemon to reload config.toml

Usage: spotuify reload [OPTIONS]

Options:
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
