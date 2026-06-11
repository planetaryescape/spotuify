---
name: spotuify
description: Control Spotify from the terminal by driving the `spotuify` CLI — search, play, queue, manage devices, build and edit playlists (preview-first), and read listening analytics. Use when the user wants to play or queue music, control playback or output devices, create or modify playlists, or analyze what they listen to.
---

# spotuify

`spotuify` is a daemon-backed, CLI-first Spotify controller. The CLI is its canonical surface: anything the app can do, `spotuify <subcommand>` can do. Drive it directly with shell commands. A background daemon owns playback, queue, devices, and a local cache; the first command auto-starts it.

## Before starting

- The user must have run `spotuify` once interactively (or `spotuify login`) so OAuth is set up. On an auth error, tell the user to run `spotuify login` — do not try to fix credentials yourself.
- Streaming needs Spotify Premium. Browse, search, and remote control work without it.
- Pass `--format json` (or `ids` / `jsonl` / `csv`) on every read command and parse the output. Never scrape the human table.

## Core commands

```bash
# State (read)
spotuify status --format json            # current playback
spotuify devices --format json           # connectable devices
spotuify queue --format json             # the queue

# Search (read) — type is track|album|playlist|episode; source local|spotify
spotuify search "never too much" --type track --format json
spotuify search "lo-fi beats" --type playlist --format ids

# Playback (transport)
spotuify play "imagine dragons"          # search-and-play
spotuify play-uri spotify:track:4uLU6hMCjMI75M1A2tKUQC
spotuify pause | spotuify resume | spotuify next | spotuify previous
spotuify seek 90s | spotuify volume 40 | spotuify shuffle on | spotuify repeat track
spotuify toggle                          # play/pause toggle

# Queue (a set: re-adding a queued track is skipped, never duplicated)
spotuify queue add spotify:track:4uLU6hMCjMI75M1A2tKUQC
spotuify queue add --search "never too much"   # idle: first match plays, the rest queue

# Devices
spotuify transfer spotuify-hume          # move playback to a Connect device
spotuify audio-outputs                    # local output devices (which speaker on this machine)
spotuify audio-output "MacBook Pro Speakers"   # switches live: no daemon restart, resumes the track

# Discovery (Mercury, via the daemon's librespot session)
spotuify artist related spotify:artist:0OdUWJ0sBjDrqHygGUXeCF --format json
spotuify radio start spotify:track:4uLU6hMCjMI75M1A2tKUQC --dry-run   # preview the resolved seed tracks
```

## Playlists, library, analytics, lyrics (read)

```bash
spotuify playlists --format json
spotuify playlist tracks "Quiet Storm" --format jsonl
spotuify playlist play "Quiet Storm"
spotuify library tracks --format jsonl
spotuify like | spotuify save             # like/save the current track
spotuify analytics top --kind tracks --since 30d --format json   # kind: tracks|artists|albums|playlists
spotuify analytics habits --window day --format json
spotuify analytics rediscovery --format json
spotuify lyrics show --format json
```

## Mutations are preview-first

Playlist creation and edits, bulk likes/follows, and batch queue ops support `--dry-run`. Always preview, show the user, and run `--yes` only after explicit approval. The dry-run uses the same selection path as the real run, so the preview is honest.

The safe agent playlist loop:

```bash
spotuify playlist plan "exile and returning home" --format json > plan.json
spotuify resolve-tracks --from plan.json --format jsonl > candidates.jsonl
spotuify playlist create "Exile and Return" --from candidates.jsonl --dry-run   # show this to the user
# only after explicit approval:
spotuify playlist create "Exile and Return" --from candidates.jsonl --yes --format json
```

Reversible mutations (playlist edits, library save/unsave, like, transfer) are recorded in an operation log. To reverse the last one:

```bash
spotuify ops undo --dry-run               # preview the reversal ("would undo …")
spotuify ops undo --yes                    # apply it
```

Queue adds are NOT reversible — Spotify has no queue-remove endpoint — so `ops undo` won't undo a `queue add`. Treat queueing as committed.

## Verify your own work

Ask the binary instead of guessing:

```bash
spotuify doctor --format json
spotuify daemon status --format json
spotuify logs tail 200
```

## Guardrails

- Prefer Spotify URIs and IDs over display names; resolve names with `search ... --format ids` first.
- Never run a mutating command with `--yes` without explicit user approval. Show the `--dry-run` first.
- Do not claim a song's lyrics or "vibe" unless you read them via `spotuify lyrics` or another source.
- The queue is a set: re-adding a track that is already queued is skipped (the receipt says `skipped N already queued`). Spotify has no queue-move, so the existing entry stays put. Queue adds are not undoable.
- One empirical test beats five guesses: when a parameter or error is unclear, run the command with `--format json` and read the real response.

## Alternative: the MCP server

When the agent speaks MCP, run the same capabilities as structured tools instead of shelling out:

```bash
spotuify mcp                              # JSON-RPC 2.0 over stdio (default)
spotuify mcp --http 127.0.0.1:8765       # loopback Streamable HTTP; needs SPOTUIFY_MCP_TOKEN
```

Tools mirror the CLI (`search`, `now_playing`, `play`, `queue_add`, `playlist_create`, `analytics_top`, `related_artists`, `radio_start`, `undo_last`, and more — 37 in total) with the same preview-first rules.
