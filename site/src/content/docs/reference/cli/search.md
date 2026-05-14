---
title: "spotuify search"
description: "Search local cache and Spotify"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Search local cache and Spotify

## Examples

```bash
spotuify search "luther vandross" --type track
spotuify search "quiet storm" --source local --format jsonl
spotuify search "imagine dragons" --play --index 1
```

## Help

```text
Search local cache and Spotify

Usage: spotuify search [OPTIONS] <QUERY>

Arguments:
  <QUERY>  Search query

Options:
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --type <KIND>              Media type to search [default: all] [possible values: all, track, episode, album, artist, playlist]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
      --source <SOURCE>          Search source. hybrid returns cached local results immediately and refreshes Spotify in the background [default: hybrid] [possible values: local, spotify, hybrid]
      --limit <LIMIT>            Maximum results to return [default: 10]
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
      --play                     Play one result instead of printing results
      --index <INDEX>            1-based search result index for --play [default: 1]
      --format <FORMAT>          Output format [default: table] [possible values: table, json, jsonl, csv, ids]
  -h, --help                     Print help
```
