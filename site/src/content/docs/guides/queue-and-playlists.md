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

The TUI append behavior is the same product rule: queueing a track appends one
URI; queueing a playlist or album expands it to playable tracks and appends
them. It does not replace the current queue.

The queue is a set: a track appears at most once. Adding a URI that is already
queued is skipped, and the receipt says so (`skipped N already queued`).
Spotify has no queue-move, so the existing entry stays where it is rather than
moving up. Queue adds are also not undoable; Spotify has no queue-remove
endpoint, so `ops undo` refuses them instead of pretending.

```bash
spotuify queue
```

After tracks enter the queue, the daemon starts a best-effort background warm
for those URIs. It resolves metadata, caches cover art, warms lyrics, indexes
the items, and asks the embedded player to preload the next audio URI when that
backend supports it. The queue command does not wait for this work.

### Queueing when nothing is playing

The queue lives on the active Spotify session, so there has to be one. When
nothing is playing, `spotuify` plays the first selected track on the embedded
device to start a session, then queues the rest, instead of failing with
`NO_ACTIVE_DEVICE`:

```bash
spotuify queue add --search "never too much"   # idle → it starts playing
```

What you get: the first item playing on `spotuify-hume`, any remaining
selections queued behind it. Once something is playing, `queue add` appends as
usual.

In the TUI, `Enter` *replaces* the queue with the item you picked and starts
playback. `e` appends. A toast after `Enter` reminds you of the alternative:

```text
Playing Wonderwall (queue replaced · e to enqueue next time)
```

If you wanted to append, `Esc` dismisses the toast, then re-select and press
`e`. Queue changes are not undoable (Spotify has no queue-remove), so the
replace is final; rebuild the queue from search or a playlist if you change
your mind.

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

Playing an album or playlist context publishes a daemon queue snapshot. The
first context track appears as the current item; the rest appear as up next.
That snapshot is cached and broadcast, so Home, Queue, the queue rail, CLI
watchers, MCP clients, and agents all see the same context queue instead of an
empty queue.

In the TUI, `Enter` on a playlist or playlist track uses the same context-play
path. Space does too when there is no resumable current item.

## Add with a dry-run

```bash
spotuify playlist add "Quiet Storm" spotify:track:4uLU6hMCjMI75M1A2tKUQC --dry-run
```

In the TUI, press `a` or `A` on a selected item. A playlist picker opens; use
`Space` to select one or more playlists, then `Enter` to add.

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

```bash
spotuify playlist create "Exile and Return" --from candidates.jsonl --yes --format json
```

## See Also

- [Agents and MCP](/guides/agents-and-mcp/)
- [Playlist Create CLI](/reference/cli/playlist-create/)
- [Resolve Tracks CLI](/reference/cli/resolve-tracks/)
