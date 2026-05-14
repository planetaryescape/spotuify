---
title: "JSON Output"
description: "Document machine-readable formats and stable output expectations."
---

Machine-readable output is part of the product. Scripts and agents should prefer `json`, `jsonl`, or `ids`.

## Pick a format

```bash
spotuify status --format json
spotuify search "luther" --format jsonl
spotuify playlists --format ids
```

## Playback

```bash
spotuify status --format json
```

Expected shape:

```json
{
  "device": {
    "id": "device-id",
    "name": "spotuify-hume"
  },
  "item": {
    "uri": "spotify:track:...",
    "name": "Track",
    "subtitle": "Artist"
  },
  "playing": true,
  "progress_ms": 42000
}
```

## Search rows

```bash
spotuify search "luther vandross" --type track --format jsonl
```

Expected row shape:

```json
{
  "uri": "spotify:track:...",
  "id": "track-id",
  "kind": "track",
  "name": "Never Too Much",
  "subtitle": "Luther Vandross",
  "duration_ms": 216000,
  "source": "spotify"
}
```

## Mutation receipts

```bash
spotuify next --format json
```

Expected shape:

```json
{
  "ok": true,
  "action": "next",
  "message": "Skipped"
}
```

Playlist creation receipts include playlist data:

```bash
spotuify playlist create "Focus" --from candidates.jsonl --dry-run --format json
```

```json
{
  "ok": true,
  "action": "playlist-create",
  "playlist_uri": "spotify:playlist:...",
  "name": "Focus",
  "added_item_count": 20
}
```

## IDs

```bash
spotuify search "luther" --format ids
```

Expected shape:

```text
spotify:track:...
spotify:album:...
```

## See Also

- [CLI Concepts](/reference/cli/concepts/)
- [IPC Protocol](/reference/ipc/)
- [Agents and MCP](/guides/agents-and-mcp/)
