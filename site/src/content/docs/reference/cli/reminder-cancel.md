---
title: "spotuify reminder cancel"
description: "Cancel a reminder schedule by id"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Cancel a reminder schedule by id

## Examples

```bash
spotuify reminder cancel <reminder-id>
```

## Help

```text
Cancel a reminder schedule by id

Usage: spotuify reminder cancel [OPTIONS] <ID>

Arguments:
  <ID>  Reminder id

Options:
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
