---
title: "Queue and Playlists"
description: "Queue tracks, inspect playlists, and use dry-run playlist mutations."
---

Queue changes are quick. Playlist changes are durable, so broad writes should preview first.

## Show the queue

```bash
spotuify queue
spotuify queue --format json
```

## Add one item

```bash
spotuify queue add spotify:track:4uLU6hMCjMI75M1A2tKUQC
```

Add by search:

```bash
spotuify queue add --search "never too much"
```

## List playlists

```bash
spotuify playlists
spotuify playlists --format ids
```

## Show playlist tracks

```bash
spotuify playlist tracks "Quiet Storm" --format jsonl
```

## Play a playlist

```bash
spotuify playlist play "Quiet Storm"
```

## Add with a dry-run

```bash
spotuify playlist add "Quiet Storm" spotify:track:4uLU6hMCjMI75M1A2tKUQC --dry-run
```

Commit only after the preview:

```bash
spotuify playlist add "Quiet Storm" spotify:track:4uLU6hMCjMI75M1A2tKUQC --yes
```

## Agent playlist workflow

```bash
spotuify playlist plan "exile and returning home" --format json > plan.json
spotuify resolve-tracks --from plan.json --format jsonl > candidates.jsonl
spotuify playlist create "Exile and Return" --from candidates.jsonl --dry-run
```

After approval:

```bash
spotuify playlist create "Exile and Return" --from candidates.jsonl --yes --format json
```

What you get: a receipt with playlist id, playlist URI, playlist name, and added item count.

## See Also

- [Agents and MCP](/guides/agents-and-mcp/)
- [Playlist Create CLI](/reference/cli/playlist-create/)
- [Resolve Tracks CLI](/reference/cli/resolve-tracks/)
