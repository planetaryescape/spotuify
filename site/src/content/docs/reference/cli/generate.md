---
title: "spotuify generate"
description: "Emit shell completions or a man page"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Emit shell completions or a man page

## Examples

```bash
spotuify generate completions zsh > _spotuify
spotuify generate man-page > spotuify.1
```

## Help

```text
Emit shell completions or a man page

Usage: spotuify generate [OPTIONS] <COMMAND>

Commands:
  completions  Emit shell completion script to stdout
  man-page     Emit man-page source to stdout
  help         Print this message or the help of the given subcommand(s)

Options:
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
