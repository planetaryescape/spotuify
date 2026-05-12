# spotuify - Search

## Philosophy

Search is navigation. It covers local library, playlists, cached discoveries, and remote Spotify catalog search.

## Search sources

- `local`: SQLite + Tantivy over cached data
- `spotify`: live Spotify Web API search
- `hybrid`: local results first, remote fill-in as needed
- `current`: filter the currently displayed list in TUI

## Search types

- tracks
- albums
- artists
- playlists
- episodes
- shows
- audiobooks

## Query examples

```text
luther vandross
artist:"luther vandross" type:track
playlist:"Quiet Storm"
is:liked genre:soul
source:local added:2026
```

## Structured filters

Initial filters:

- `type:`
- `artist:`
- `album:`
- `playlist:`
- `is:liked`
- `is:saved`
- `is:queued`
- `source:`
- `before:`
- `after:`
- `duration:`

Filters must be shared by CLI and TUI through a `SearchSpec` data structure.

## CLI examples

```text
spotuify search "luther vandross" --type track
spotuify search "luther vandross" --type track --format jsonl
spotuify search "artist:luther vandross" --source local --format ids
spotuify search "quiet storm" --source hybrid --explain
```

## TUI search UX

- `/` opens global catalog/library search.
- `Ctrl-f` filters the current list.
- `Tab` cycles result type chips.
- Results are grouped by type.
- Multi-select enables bulk queue, like, save, and playlist actions.
- Search state shows query, source, filters, result count, loading, stale, and error state.

## Search output records

Machine output should include:

- stable URI
- Spotify ID
- kind
- name
- subtitle
- context
- duration
- source
- cached/freshness state
- rank/explain fields when requested

## Remote search cache

Every remote search should be optionally cached as:

- normalized query
- filters
- raw result IDs
- display metadata
- fetch timestamp
- API status

This makes repeated agent workflows deterministic and debuggable.
