# spotuify - Sync and Cache

## Philosophy

The daemon should answer from local state whenever it can, then reconcile with Spotify.

This is not offline Spotify playback. It is offline-ish browsing and fast control over cached metadata.

## Sync domains

### Fast refresh

Runs often and remains cheap:

- playback state
- active device
- devices
- queue

### Library sync

Runs in background:

- saved tracks
- saved albums
- saved episodes
- followed artists
- followed playlists

### Playlist sync

Runs incrementally where possible:

- playlist metadata
- playlist items
- snapshot IDs

### Discovery cache

Stores remote search results the user has seen:

- query
- filters
- result URIs
- result metadata
- fetched timestamp

This lets repeated searches respond instantly while refreshing in the background.

## Freshness model

Every cached row carries freshness metadata:

- `fetched_at`
- `etag` or Spotify snapshot ID when available
- `source`
- `sync_generation`

Freshness classes:

- `fresh`
- `stale_but_usable`
- `refreshing`
- `failed_refresh`
- `unknown`

## Optimistic mutation flow

1. Client sends mutation request.
2. Daemon validates selection and capability.
3. `--dry-run` returns preview and exits.
4. Real mutation records pending receipt.
5. Daemon applies optimistic local effect if safe.
6. Daemon calls Spotify.
7. Daemon updates receipt and local cache.
8. Daemon emits event to all clients.

## Reconciliation

When Spotify returns unexpected state:

- prefer Spotify truth for remote-owned data
- preserve local mutation receipt
- emit a reconciliation event
- show human-readable explanation in CLI/TUI

## Sync commands

```text
spotuify sync
spotuify sync playback
spotuify sync library
spotuify sync playlists
spotuify sync search-cache --prune
spotuify reindex
spotuify cache status --format json
```

## Rate-limit behavior

The daemon should centralize rate-limit handling:

- respect `Retry-After`
- delay background sync first
- keep playback controls highest priority
- surface degraded state in `doctor` and TUI diagnostics
