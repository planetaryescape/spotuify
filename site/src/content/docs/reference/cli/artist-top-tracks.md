---
title: "spotuify artist top-tracks"
description: "The artist's ten Popular tracks, in Spotify's order: all-time streams in your country, weighted toward recent listening, updated daily. The same list as the top of Spotify's artist page. Session-backed: the Web API endpoint was removed for developer apps"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

The artist's ten Popular tracks, in Spotify's order: all-time streams in your country, weighted toward recent listening, updated daily. The same list as the top of Spotify's artist page. Session-backed: the Web API endpoint was removed for developer apps

## Examples

```bash
spotuify artist top-tracks spotify:artist:19y5MFBH7gohEdGwKM7QsP
spotuify artist top-tracks spotify:artist:19y5MFBH7gohEdGwKM7QsP --format ids | head -5
```

## Help

```text
The artist's ten Popular tracks, in Spotify's order: all-time streams in your country, weighted toward recent listening, updated daily. The same list as the top of Spotify's artist page. Session-backed: the Web API endpoint was removed for developer apps

Usage: spotuify artist top-tracks [OPTIONS] <ARTIST>

Arguments:
  <ARTIST>  Artist ID or URI

Options:
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --provider <PROVIDER>      Provider to target (defaults to the daemon's default provider)
      --format <FORMAT>          [default: table] [possible values: table, json, jsonl, csv, ids]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
