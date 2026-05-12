# Phase 3 - Local Store and Search

## Goal

Add SQLite cache and Tantivy search so local library/playlists/search history are fast and scriptable.

## Deliverables

- SQLite migrations.
- Store module with explicit SQL.
- Sync jobs for devices/playback/playlists/recent/library.
- Remote search result cache.
- Tantivy schema and indexing.
- `spotuify search --source local|spotify|hybrid`.
- `spotuify reindex`.
- `spotuify cache status`.

## Implementation order

1. Add SQLite connection and migrations.
2. Persist playback/device snapshots.
3. Persist playlists and playlist items.
4. Persist recent tracks and search results.
5. Add local query over SQLite only.
6. Add Tantivy index from SQLite.
7. Add reindex command.
8. Add background sync scheduler.

## Search schema starter

Fields:

- URI
- kind
- name
- artist names
- album name
- playlist name
- owner
- source
- liked/saved flags
- added timestamp
- duration

## Verification

- sync creates rows
- reindex creates documents
- local search works without Spotify network
- remote search caches results
- cache status shows row counts and freshness

## Definition of done

Common library and playlist searches respond from local state, with Spotify refresh happening in background.
