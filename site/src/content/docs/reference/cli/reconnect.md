---
title: "spotuify reconnect"
description: "Force the daemon to rebuild its upstream Spotify session"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Force the daemon to rebuild its upstream Spotify session

## Examples

```bash
spotuify reconnect
```

## Help

```text
Force the daemon to rebuild its upstream Spotify session

Usage: spotuify reconnect [OPTIONS]

Options:
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
