---
title: "spotuify playlist remove-at"
description: "Remove exact playlist item occurrences by one-based row number"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Remove exact playlist item occurrences by one-based row number

## Examples

```bash
spotuify playlist remove-at "Quiet Storm" 2 5 --dry-run
spotuify playlist remove-at "Quiet Storm" 2 5 --yes
```

## Help

```text
Remove exact playlist item occurrences by one-based row number

Usage: spotuify playlist remove-at [OPTIONS] <PLAYLIST> <ROWS>...

Arguments:
  <PLAYLIST>  Playlist ID, URI, or exact name
  <ROWS>...   One-based playlist row number(s)

Options:
      --dry-run                  Validate the exact removal without changing the playlist
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
      --yes                      Commit a multi-row removal without an interactive prompt
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
      --provider <PROVIDER>      Provider to target (omitted routes by the resolved playlist URI)
      --format <FORMAT>          Output format for the mutation receipt [default: table] [possible values: table, json, jsonl, csv, ids]
  -h, --help                     Print help
```
