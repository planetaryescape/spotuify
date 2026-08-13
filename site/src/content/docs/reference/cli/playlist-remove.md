---
title: "spotuify playlist remove"
description: "Remove track or episode occurrences from a playlist"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Remove track or episode occurrences from a playlist

## Examples

```bash
spotuify playlist remove "Quiet Storm" spotify:track:... --dry-run
```

## Help

```text
Remove track or episode occurrences from a playlist

Usage: spotuify playlist remove [OPTIONS] <PLAYLIST> [URIS]...

Arguments:
  <PLAYLIST>  Playlist ID, URI, or exact name
  [URIS]...   Track or episode URI(s)

Options:
      --ids <FILE>               Read resource references from a file, or `-` for stdin
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --dry-run                  Show the exact mutation without removing from the playlist
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
      --yes                      Commit a multi-item playlist removal without an interactive prompt
      --provider <PROVIDER>      Provider to target (defaults to the daemon's default provider)
      --format <FORMAT>          Output format for the mutation receipt [default: table] [possible values: table, json, jsonl, csv, ids]
  -h, --help                     Print help
```
