---
title: "spotuify generate completions"
description: "Emit shell completion script to stdout"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Emit shell completion script to stdout

## Examples

```bash
spotuify generate completions zsh > _spotuify
```

## Help

```text
Emit shell completion script to stdout

Usage: spotuify generate completions [OPTIONS] <SHELL>

Arguments:
  <SHELL>  Shell to generate completions for

Options:
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
