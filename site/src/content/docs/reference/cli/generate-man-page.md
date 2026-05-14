---
title: "spotuify generate man-page"
description: "Emit roff man page source to stdout"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Emit roff man page source to stdout

## Examples

```bash
spotuify generate man-page > spotuify.1
```

## Help

```text
Emit roff man page source to stdout

Usage: spotuify generate man-page [OPTIONS]

Options:
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
