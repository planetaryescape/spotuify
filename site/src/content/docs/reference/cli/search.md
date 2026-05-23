---
title: "spotuify search"
description: "Search Spotify's catalog (or your local cache)"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Search Spotify's catalog (or your local cache)

## Examples

```bash
spotuify search "luther vandross" --type track
spotuify search "quiet storm" --source local --format jsonl
spotuify search "imagine dragons" --play --index 1
```

## Help

```text
Search Spotify's catalog (or your local cache)

Usage: spotuify search [OPTIONS] <QUERY>

Arguments:
  <QUERY>  Search query

Options:
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --type <KIND>              Media type to search [default: all] [possible values: all, track, episode, show, album, artist, playlist]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
      --source <SOURCE>          Where to search. `spotify` (default) queries the Web API for catalog discovery. `local` queries only the local Tantivy index (offline / library lookup). `hybrid` returns local cached hits immediately and refreshes Spotify in the background [default: spotify] [possible values: local, spotify, hybrid]
      --limit <LIMIT>            Maximum results to return (Spotify caps per-type at 10 empirically) [default: 50]
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
      --pages <PAGES>            Pages of 10 to request per media type. `1` = one-shot (current behavior, up to 60 items). `3` matches the TUI streaming fanout (up to 180 items). Aggregates pages via `SearchStream` before printing [default: 1]
      --play                     Play one result instead of printing results
      --index <INDEX>            1-based search result index for --play [default: 1]
      --format <FORMAT>          Output format [default: table] [possible values: table, json, jsonl, csv, ids]
  -h, --help                     Print help
```
