---
title: "Quick Start"
description: "Run the TUI, play a search result, and use pipeable commands."
---

This page is the shortest path from a working install to music.

## Open the TUI

```bash
spotuify
```

What you get: the player-first terminal UI. Press `/`, type a query, press `Enter`, then press `Enter` on a result to play it.

## Play without the TUI

```bash
spotuify play "imagine dragons"
```

What you get: a mutation receipt. Use `--format json` when a script or agent needs to read it.

```bash
spotuify play "imagine dragons" --format json
```

## Pick with fzf

```bash
spotuify search "luther vandross" --type track --format ids \
  | fzf \
  | xargs spotuify play-uri
```

What you get: one Spotify URI selected from search and sent back into playback.

## Queue the first match

```bash
spotuify queue add --search "never too much"
spotuify queue --format json
```

## Add the current track to a playlist

```bash
spotuify playlist add-current "Coding"
```

## Use a dry-run before playlist writes

```bash
spotuify playlist add "Coding" spotify:track:4uLU6hMCjMI75M1A2tKUQC --dry-run
```

Commit after the preview looks right:

```bash
spotuify playlist add "Coding" spotify:track:4uLU6hMCjMI75M1A2tKUQC --yes
```

## Ask an agent for music

Prompt:

```text
Make me an upbeat but focused playlist. Use spotuify playlist plan,
resolve-tracks, then show playlist create --dry-run before changing Spotify.
```

Commands the agent should run:

```bash
spotuify playlist plan "upbeat but focused" --format json > plan.json
spotuify resolve-tracks --from plan.json --format jsonl > candidates.jsonl
spotuify playlist create "Upbeat Focus" --from candidates.jsonl --dry-run
```

## See Also

- [Search and Play](/guides/search-and-play/)
- [Queue and Playlists](/guides/queue-and-playlists/)
- [CLI Reference](/reference/cli/)
