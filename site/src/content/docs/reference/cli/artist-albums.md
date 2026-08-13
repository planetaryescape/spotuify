---
title: "spotuify artist albums"
description: "Print an artist's discography (albums, singles, compilations, appears-on)"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Print an artist's discography (albums, singles, compilations, appears-on)

## Examples

```bash
spotuify artist albums spotify:artist:36QJpDe2go2KgaRleHCDTp
spotuify artist albums spotify:artist:36QJpDe2go2KgaRleHCDTp --library-only
spotuify artist albums spotify:artist:36QJpDe2go2KgaRleHCDTp --group album --group single --format json
```

## Help

```text
Print an artist's discography (albums, singles, compilations, appears-on)

Usage: spotuify artist albums [OPTIONS] <ARTIST>

Arguments:
  <ARTIST>  Artist ID or URI

Options:
      --library-only             Only albums already in your library (saved albums)
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --group <GROUPS>           Restrict to one or more album groups (repeatable). Default: all [possible values: album, single, compilation, appears-on]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
      --provider <PROVIDER>      Provider to target (defaults to the daemon's default provider)
      --format <FORMAT>          [default: table] [possible values: table, json, jsonl, csv, ids]
  -h, --help                     Print help
```
