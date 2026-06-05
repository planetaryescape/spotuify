---
title: "spotuify playlist set-image"
description: "Replace a playlist's cover art with a custom JPEG."
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Replace a playlist's cover art with a custom JPEG.

## Examples

```bash
spotuify playlist set-image "Quiet Storm" --file cover.jpg
```

## Help

```text
Replace a playlist's cover art with a custom JPEG.

Spotify accepts only JPEG and caps the base64-encoded body at 256 KB. Requires the `ugc-image-upload` OAuth scope - if your stored token predates spotuify 0.1.23, run `spotuify login` first.

Usage: spotuify playlist set-image [OPTIONS] --file <FILE> <PLAYLIST>

Arguments:
  <PLAYLIST>
          Playlist ID, URI, or exact name

Options:
      --file <FILE>
          Path to a JPEG file (or `-` to read JPEG bytes from stdin)

      --log-format <LOG_FORMAT>
          Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT`

          [possible values: text, json]

      --format <FORMAT>
          Output format for the mutation receipt

          [default: table]
          [possible values: table, json, jsonl, csv, ids]

      --no-daemon-start
          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing

  -o, --set <key.path=value>
          Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged

  -h, --help
          Print help (see a summary with '-h')
```
