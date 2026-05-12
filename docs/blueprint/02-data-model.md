# spotuify - Data Model

## Source of truth

SQLite is the local source of truth for cached Spotify/user data. Spotify remains the remote authority for actual account state. The daemon reconciles the two.

Tantivy is derived state. It must be rebuildable from SQLite.

## ID policy

Use explicit typed IDs internally:

| ID | Meaning |
|---|---|
| `TrackId` | Spotify track ID |
| `AlbumId` | Spotify album ID |
| `ArtistId` | Spotify artist ID |
| `PlaylistId` | Spotify playlist ID |
| `EpisodeId` | Spotify episode ID |
| `ShowId` | Spotify show ID |
| `AudiobookId` | Spotify audiobook ID |
| `DeviceId` | Spotify Connect device ID, nullable remotely |
| `SearchId` | Local search execution record |
| `MutationId` | Local mutation receipt |

Display names are never stable identifiers. CLI mutation commands should prefer IDs or URIs.

## Core entities

### Media item

Common fields:

- `uri`
- `id`
- `kind`
- `name`
- `subtitle`
- `duration_ms`
- `image_url`
- `external_url`
- `is_playable`
- `source`
- `updated_at`

Kinds:

- `track`
- `album`
- `artist`
- `playlist`
- `episode`
- `show`
- `audiobook`
- `chapter`

### Playback

- active device
- current item
- context URI
- progress
- playing/paused state
- shuffle state
- repeat state: `off`, `context`, `track`
- volume
- fetched timestamp

### Device

- device ID, nullable
- name
- type
- active flag
- restricted flag
- volume support
- volume percent
- last seen timestamp

### Playlist

- playlist ID
- owner
- name
- description
- public/collaborative flags
- snapshot ID
- track count
- image
- last synced timestamp

### Library state

Saved/followed relationships are first-class rows:

- saved tracks
- saved albums
- saved episodes
- saved shows
- saved audiobooks
- followed artists
- followed playlists

This allows fast local filters like `is:liked`, `is:saved`, `artist:followed`, and playlist membership queries.

## SQLite schema target

Initial tables:

- `accounts`
- `devices`
- `playback_snapshots`
- `artists`
- `albums`
- `tracks`
- `episodes`
- `shows`
- `audiobooks`
- `playlists`
- `playlist_items`
- `library_items`
- `search_runs`
- `search_results`
- `sync_cursors`
- `sync_events`
- `mutations`
- `mutation_items`
- `api_errors`
- `action_trace`

## Mutation receipts

Every mutation should return a receipt:

```json
{
  "mutation_id": "...",
  "kind": "playlist.add_items",
  "status": "accepted",
  "dry_run": false,
  "requested_count": 25,
  "accepted_count": 25,
  "spotify_snapshot_id": "...",
  "warnings": []
}
```

Receipt states:

- `preview`
- `accepted`
- `confirmed`
- `failed`
- `partially_failed`
- `reconciled`

## Optimistic state

Optimistic updates may be shown to clients only after the daemon accepts a mutation. The daemon records the original state, the intended mutation, and the reconciliation result.

If Spotify rejects the mutation, the daemon must emit an event explaining rollback or partial completion.
