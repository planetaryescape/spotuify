---
title: "Recipes"
description: "Copy-pasteable shell, fzf, jq, playlist, and agent workflows."
---

Recipes are the point of a CLI-first music app. Each one starts with a thing you actually want.

## Play something from fuzzy search

```bash
spotuify search "luther vandross" --type track --format ids \
  | fzf \
  | xargs spotuify play-uri
```

What you get: a fast terminal picker that starts the selected track.

## Queue a small search set

```bash
spotuify search "burial" --type track --limit 5 --format ids \
  | spotuify queue add --ids - --format json
```

What you get: the first five matching track URIs queued through the daemon.

## Follow lyrics in the terminal

```bash
spotuify play "never too much" --type track
spotuify lyrics follow --lines 3
```

What you get: a small karaoke-style lyrics window that advances with the current track. If a track has only plain lyrics, use `spotuify lyrics show`.

## Make a playlist from an agent plan

```bash
spotuify playlist plan "songs about exile and returning home" --format json > plan.json
spotuify resolve-tracks --from plan.json --format jsonl > candidates.jsonl
spotuify playlist create "Exile and Return" --from candidates.jsonl --dry-run
```

What you get: a preview. Commit after approval:

```bash
spotuify playlist create "Exile and Return" --from candidates.jsonl --yes --format json
```

## Inspect unresolved candidates

```bash
jq -r 'select(.status != "resolved") | [.query, .reason] | @tsv' candidates.jsonl
```

What you get: rows the agent should fix before a playlist write.

## Status line

```bash
spotuify status --format json \
  | jq -r 'if .item then .item.name + " - " + .item.subtitle else "not playing" end'
```

What you get: a now-playing string for tmux or a shell prompt.

## Save something for later

```bash
spotuify reminder create spotify:album:3kEtdS2pH6hKcMU9Wioob1 --at +3d --message "come back to this"
spotuify reminder list
```

What you get: a daemon-owned listening reminder. When it fires, act from the inbox:

```bash
spotuify notifications list
spotuify notifications queue <notification-id>
```
## Backfill Last.fm history

```bash
export SPOTUIFY_LASTFM_API_KEY="lastfm-api-key"
export SPOTUIFY_LASTFM_USER="your-lastfm-user"

spotuify analytics import lastfm --from 2024-01-01 --format json
```

What you get: a dry-run summary with fetched, resolved, promoted, and unresolved counts. Commit after the preview looks right:

```bash
spotuify analytics import lastfm --from 2024-01-01 --apply --format json
spotuify analytics import unresolved 018f... --format json
```
## Emergency quiet

```bash
spotuify pause
spotuify volume 30
```

## Agent prompt that works

```text
I need focused, hopeful music for a long coding session.
Use spotuify search and playlist plan. Show me the candidate list and
the playlist create --dry-run output. Do not run --yes until I approve.
```

## See Also

- [Terminal Control](/guides/terminal-control/)
- [Queue and Playlists](/guides/queue-and-playlists/)
- [Import Last.fm History](/guides/import-lastfm-history/)
- [Agents and MCP](/guides/agents-and-mcp/)
