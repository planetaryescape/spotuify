---
title: "spotuify reminder"
description: "Schedule and manage listening reminders"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Schedule and manage listening reminders

## Examples

```bash
spotuify reminder list
spotuify reminder create spotify:album:... --at +3d
```

## Help

```text
Schedule and manage listening reminders

Usage: spotuify reminder [OPTIONS] <COMMAND>

Commands:
  create  Schedule a listening reminder for a media URI
  list    List reminder schedules
  cancel  Cancel a reminder schedule by id
  help    Print this message or the help of the given subcommand(s)

Options:
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
