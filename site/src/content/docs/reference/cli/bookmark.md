---
title: "spotuify bookmark"
description: "Save and revisit positions inside podcasts and tracks"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Save and revisit positions inside podcasts and tracks

## Examples

```bash
spotuify bookmark add
spotuify bookmark list --current
spotuify bookmark play <bookmark-id>
```

## Help

```text
Save and revisit positions inside podcasts and tracks

Usage: spotuify bookmark [OPTIONS] <COMMAND>

Commands:
  add     Save a position. With no arguments, bookmarks the current item at its current playback position
  list    List bookmarks (newest first), or one item's bookmarks in position order
  note    Set or clear the note on a bookmark
  delete  Delete a bookmark by id
  play    Play the bookmarked item from its saved position
  help    Print this message or the help of the given subcommand(s)

Options:
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
```
