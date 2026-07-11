---
title: "spotuify analytics export"
description: "Export qualified listens. Not implemented yet; use live hooks"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Export qualified listens. Not implemented yet; use live hooks

## Examples

```bash
spotuify analytics export --help
```

## Help

```text
Export qualified listens. Not implemented yet; use live hooks

Usage: spotuify analytics export [OPTIONS]

Options:
      --target <TARGET>  Export target reserved for the future export bridge [possible values: listenbrainz, lastfm]
      --since <SINCE>    ISO timestamp to export from
      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
