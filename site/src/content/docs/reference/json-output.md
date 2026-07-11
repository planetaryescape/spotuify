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

## Artist albums

```bash
spotuify artist albums spotify:artist:36QJpDe2go2KgaRleHCDTp --format json
```

Discography rows add `album_group` (album, single, compilation, or appears-on)
and `in_library` (whether the album is saved). Both are absent on rows where
they do not apply.

```json
{
  "uri": "spotify:album:...",
  "id": "album-id",
  "kind": "album",
  "name": "Never Too Much",
  "subtitle": "Luther Vandross",
  "context": "7 tracks",
  "album_group": "album",
  "in_library": true,
  "release_date": "1981-08-11",
  "source": "spotify"
}
```

`spotuify artist followed --format json` returns `artist` rows in the same
MediaItem shape, without the album-only fields.

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

## Lyrics follow events

```bash
spotuify lyrics follow --format jsonl
```

Expected active-line event shape:

```json
{
  "event": "line",
  "track_uri": "spotify:track:...",
  "track_name": "Track",
  "artist": "Artist",
  "is_playing": true,
  "progress_ms": 42000,
  "line_index": 4,
  "line_start_ms": 41000,
  "text": "lyric line",
  "is_rtl": false
}
```

When lyrics are missing or not synced, `lyrics follow --format jsonl` emits a
status object and keeps watching for the next track:

```json
{
  "event": "status",
  "track_uri": "spotify:track:...",
  "track_name": "Track",
  "artist": "Artist",
  "is_playing": true,
  "progress_ms": 42000,
  "message": "synced lyrics unavailable; use `spotuify lyrics show`"
}
```
## Last.fm import

Preview historical Last.fm import before writing rows:

```bash
spotuify analytics import lastfm --user your-lastfm-user --from 2024-01-01 --format json
```

Expected shape:

```json
{
  "run_id": "018f...",
  "provider": "lastfm",
  "username": "your-lastfm-user",
  "dry_run": true,
  "fetched": 1200,
  "stored": 0,
  "duplicates": 0,
  "resolved": 1138,
  "promoted": 0,
  "unresolved": 62,
  "started_at_ms": 1735689600000,
  "finished_at_ms": 1735689660000
}
```

Apply, status, unresolved, and undo use the same direct payload style:

```bash
spotuify analytics import lastfm --user your-lastfm-user --from 2024-01-01 --apply --format json
spotuify analytics import status 018f... --format json
spotuify analytics import unresolved 018f... --format json
spotuify analytics import undo 018f... --dry-run --format json
```

Unresolved rows are an array:

```json
[
  {
    "id": 42,
    "scrobbled_at_ms": 1705000000000,
    "artist": "Artist",
    "track": "Track",
    "album": "Album",
    "url": "https://www.last.fm/music/Artist/_/Track",
    "resolution_status": "unresolved",
    "confidence": null
  }
]
```

Undo returns the run id, dry-run flag, removed listen fact count, and preserved raw scrobble count:

```json
{
  "run_id": "018f...",
  "dry_run": true,
  "listen_facts_removed": 1138,
  "raw_scrobbles_preserved": 1200
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
- [Import Last.fm History](/guides/import-lastfm-history/)
