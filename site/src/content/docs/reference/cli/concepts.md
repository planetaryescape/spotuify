---
title: "CLI Concepts"
description: "Understand output formats, globals, IDs, dry-run, and exit codes."
---

The CLI is the stable surface for humans, scripts, tests, and agents.

## Global flags

```bash
spotuify --log-format json status
spotuify --no-daemon-start status
spotuify -o player.bitrate=160 play "ambient"
```

| Flag | Meaning |
| --- | --- |
| `--log-format text|json` | choose log format for this invocation |
| `--no-daemon-start` | do not auto-start the daemon |
| `-o, --set key.path=value` | one-shot config override; does not edit the file |
| `-h, --help` | print help |
| `-V, --version` | print version |

## Output formats

```bash
spotuify status --format table
spotuify status --format json
spotuify search "luther" --format jsonl
spotuify playlists --format csv
spotuify playlists --format ids
```

| Format | Use it when |
| --- | --- |
| `table` | a human is reading the terminal |
| `json` | one object or array goes to a script |
| `jsonl` | stream rows one JSON object per line |
| `csv` | spreadsheet or shell text processing |
| `ids` | piping stable URIs into another command |

## IDs over names

Display names are not stable. Prefer Spotify URIs when writing scripts.

```bash
spotuify search "never too much" --type track --format ids \
  | head -n 1 \
  | xargs spotuify queue add
```

## Dry-run before broad mutation

```bash
spotuify playlist add "Coding" --ids tracks.txt --dry-run
spotuify playlist add "Coding" --ids tracks.txt --yes
```

Use `--dry-run` for playlist creation, playlist edits, and anything an agent might do in bulk.

## Exit codes

| Code | Meaning |
| ---: | --- |
| 0 | success |
| 1 | general error |
| 2 | usage error |
| 3 | daemon unavailable |
| 4 | auth error |
| 5 | no active device |
| 6 | Spotify rate limited |
| 7 | unsupported capability |
| 8 | partial mutation failure |

## See Also

- [CLI Reference](/reference/cli/)
- [JSON Output](/reference/json-output/)
- [Config](/reference/config/)
