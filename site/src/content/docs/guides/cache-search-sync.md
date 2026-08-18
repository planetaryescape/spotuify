---
title: "Cache, Search, Sync"
description: "Use SQLite cache, Tantivy search, sync, reindex, and repair commands."
---

`spotuify` keeps Spotify metadata locally so the CLI and TUI can answer quickly. SQLite is the cache. Tantivy is the rebuildable search index.

## Inspect provider capabilities

```bash
spotuify providers list
spotuify providers list --format json
```

The result names the default provider and the capabilities each adapter
supports, including search kinds, page and mutation limits, transport actions,
lyrics, related artists, and radio. Scripts can inspect this contract instead
of assuming every provider behaves like Spotify.

## Sync data

```bash
spotuify sync
spotuify sync library --format json
spotuify sync playlists --format json
```

Sync targets:

```bash
spotuify sync playback
spotuify sync devices
spotuify sync recent
```

## Inspect cache

```bash
spotuify cache status --format json
```

What you get: database path, index path, row counts, search result counts, lyrics cache counts, cover cache size, and last sync/search timestamps.

Queued tracks also warm the reusable cache in the background: metadata,
search-index rows, cover art, lyrics, and the next embedded-player audio
preload when available. This is best effort; playback and queue commands do not
wait for it.

## Rebuild the search index

```bash
spotuify reindex --format json
```

Use this when SQLite has data but local search looks empty or stale.

## Repair cache

```bash
spotuify cache repair --format json
```

What you get: schema repair plus a local index rebuild.

## Reset cache

This deletes local cache files. It does not delete Spotify data.

```bash
spotuify cache reset --confirm
```

After reset:

```bash
spotuify sync all
spotuify cache status
```

## See Also

- [Architecture](/guides/architecture/)
- [Cache Status CLI](/reference/cli/cache-status/)
- [Sync CLI](/reference/cli/sync/)
