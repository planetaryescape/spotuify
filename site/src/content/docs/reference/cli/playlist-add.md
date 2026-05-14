---
title: "spotuify playlist add"
description: "Add a Spotify URI to a playlist"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Add a Spotify URI to a playlist

## Examples

```bash
spotuify playlist add "Quiet Storm" spotify:track:... --dry-run
spotuify playlist add "Quiet Storm" --ids tracks.txt --yes
```

## Help

```text
Add a Spotify URI to a playlist

Usage: spotuify playlist add [OPTIONS] <PLAYLIST> [URIS]...

Arguments:
  <PLAYLIST>  Playlist ID, URI, or exact name
  [URIS]...   Track or episode URI(s)

Options:
      --ids <FILE>               Read Spotify URI(s) from a file, or `-` for stdin
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --dry-run                  Show the exact mutation without adding to the playlist
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
      --yes                      Commit a multi-item playlist add without an interactive prompt
      --format <FORMAT>          Output format for the mutation receipt [default: table] [possible values: table, json, jsonl, csv, ids]
  -h, --help                     Print help
```
