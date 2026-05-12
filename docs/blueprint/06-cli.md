# spotuify - CLI and Shell Integration

## CLI principle

The CLI is the canonical surface for humans, scripts, tests, and agents. New capabilities should land in CLI first or at the same time as TUI.

## Global conventions

Every data command should support:

```text
--format table|json|jsonl|csv|ids
--limit N
--offset N
```

Every broad mutation should support:

```text
--dry-run
--yes
```

Commands accepting multiple items should accept:

- positional IDs or URIs
- `--ids FILE`
- stdin IDs
- `--search QUERY` when the daemon can resolve the selection consistently

## Output defaults

- TTY stdout defaults to `table`.
- Piped stdout defaults to `json` for single records or `jsonl` for streams where appropriate.
- `ids` means one stable URI or ID per line.

## Core commands

```text
spotuify                         # open TUI
spotuify tui                     # explicit TUI
spotuify daemon start
spotuify daemon stop
spotuify daemon restart
spotuify daemon status
spotuify doctor
spotuify logs tail
spotuify config get KEY
spotuify config set KEY VALUE
```

## Playback commands

```text
spotuify status
spotuify play "query"
spotuify play-uri URI
spotuify pause
spotuify resume
spotuify toggle
spotuify next
spotuify previous
spotuify seek +15s
spotuify seek -15s
spotuify volume 70
spotuify shuffle on|off|toggle
spotuify repeat off|context|track
```

## Device commands

```text
spotuify devices
spotuify transfer DEVICE_ID_OR_NAME
spotuify device prefer DEVICE_ID_OR_NAME
spotuify device activate
```

## Search commands

```text
spotuify search "query"
spotuify search "query" --type track --source local
spotuify search "query" --type track --play --index 2
spotuify search "query" --format jsonl
```

## Queue commands

```text
spotuify queue
spotuify queue add URI
spotuify queue add --search "query"
spotuify queue add --ids tracks.txt
```

Spotify's Web API does not support queue removal or reorder. If we show these concepts, they must be local planned queues or clearly unsupported remotely.

## Playlist commands

```text
spotuify playlists
spotuify playlist create "Name"
spotuify playlist show PLAYLIST
spotuify playlist tracks PLAYLIST
spotuify playlist play PLAYLIST
spotuify playlist add PLAYLIST URI
spotuify playlist add PLAYLIST --ids tracks.txt
spotuify playlist remove PLAYLIST URI --dry-run
spotuify playlist reorder PLAYLIST --from 10 --to 1
```

## Library commands

```text
spotuify library tracks
spotuify library albums
spotuify library artists
spotuify like URI
spotuify unlike URI
spotuify like current
spotuify follow artist ARTIST
spotuify unfollow artist ARTIST
```

## Agent commands

```text
spotuify playlist plan "brief" --format json
spotuify playlist create-from-plan plan.json --dry-run
spotuify playlist create-from-plan plan.json --yes
spotuify resolve-tracks candidates.jsonl --format jsonl
```

## Exit codes

| Code | Meaning |
|---:|---|
| 0 | success |
| 1 | general error |
| 2 | usage error |
| 3 | daemon unavailable |
| 4 | auth error |
| 5 | no active device |
| 6 | Spotify rate limited |
| 7 | unsupported capability |
| 8 | partial mutation failure |
