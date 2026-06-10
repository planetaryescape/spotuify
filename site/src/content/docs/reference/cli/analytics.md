---
title: "spotuify analytics"
description: "Inspect local analytics data"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Inspect local analytics data

## Examples

```bash
spotuify analytics events --limit 20
spotuify analytics top --kind artists
```

## Help

```text
Inspect local analytics data

Usage: spotuify analytics [OPTIONS] <COMMAND>

Commands:
  events       Print recent analytics events
  top          Top-N most-played tracks / artists / albums / playlists
  habits       Habit metrics bucketed by `day` / `week` / `month`
  search       Search history (raw or normalized mode)
  rediscovery  Tracks worth re-discovering
  rebuild      Recompute derived listen facts from analytics_events
  prune        Apply retention prune (default: dry-run)
  export       Export qualified listens. Not implemented yet; use live hooks
  import       Import historical scrobbles. Not implemented yet
  help         Print this message or the help of the given subcommand(s)

Options:
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
