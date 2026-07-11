---
title: "spotuify analytics import lastfm"
description: "Preview/apply Last.fm historical scrobble import"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Preview/apply Last.fm historical scrobble import

## Examples

```bash
spotuify analytics import lastfm --user your-lastfm-user --from 2024-01-01 --format json
spotuify analytics import lastfm --user your-lastfm-user --from 2024-01-01 --apply --format json
```

## Help

```text
Preview/apply Last.fm historical scrobble import

Usage: spotuify analytics import lastfm [OPTIONS]

Options:
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --user <USER>
      --api-key <API_KEY>
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
      --from <FROM>
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
      --to <TO>
      --apply
      --format <FORMAT>          [default: table] [possible values: table, json, jsonl, csv, ids]
  -h, --help                     Print help
```
