# Current State

## What exists

Current spotuify is a Rust workspace with a unified `spotuify`
binary. The binary exposes TUI, CLI, daemon, cache, MCP, lyrics,
analytics, visualization, and maintenance surfaces backed by split
crates for core types, protocol, store, Spotify, player, daemon, CLI,
TUI, audio, system integration, lyrics, sync, and MCP.

Implemented CLI commands:

- `onboard`
- `login`
- `logout`
- `doctor`
- `logs path|tail`
- `config path|init|get|set`
- playback controls: status, play, pause, resume, toggle, next, previous,
  seek, volume, shuffle, repeat, queue, transfer
- browsing/search: search, devices, playlists, playlist tracks,
  recently played, library save/unsave
- artist discography: artist albums (grouped, with `--library-only` and
  repeatable `--group` filters) and artist followed
- listening reminders: reminder create/list/cancel and notifications
  list/play/queue/snooze/dismiss
- release awareness: update checks and upgrade hints for CLI, TUI, and
  the macOS app
- daemon lifecycle and IPC-backed one-shot commands
- cache status/reset/repair/reindex
- operations log: ops list/show/undo/redo
- analytics, lyrics, current-track media refresh, MCP, and visualization
  commands
- `episodes` for a flat, date-ordered feed across followed shows

Implemented TUI areas:

- search
- grouped search result rendering by media kind, including podcasts/shows
- selection artwork previews for albums, playlists, shows, and episodes in
  Search/Library where image metadata exists
- queue
- playlists
- library
- devices
- diagnostics
- synced lyrics
- persistent bottom player
- optional right rail for queue, lyrics, and contextual key hints
- fullscreen queue and lyrics overlays
- playlist picker modal for add-to-playlist
- artist discography overlay: releases grouped into Albums / Singles & EPs /
  Compilations / Appears On, with `L` toggling an in-library-only filter
- notifications screen for due listening reminders, plus a reminder picker modal
- Space starts the selected Home, Search, Library, or Playlist item when there
  is no resumable current item, including ended-track state
- manual cover-art and lyrics refresh for the current track

Implemented Spotify API capabilities:

- playback state
- devices
- queue read
- search tracks/episodes/shows/albums/artists/playlists
- playlists
- recently played
- playlist tracks
- artist discography (`/artists/{id}/albums` across all groups with
  `market=from_token` and id de-duplication, tagged with `album_group` and
  `in_library`)
- followed artists (`/me/following?type=artist`, cursor-paginated)
- cross-show episode feed built from followed shows and `show-episodes`
- play/pause
- play URI/context
- album/playlist context playback emits and caches a queue snapshot, with the
  first context track as currently playing and the rest as up next
- next/previous
- seek
- volume
- shuffle
- repeat
- add to queue
- append playlist and album selections to the queue by expanding them to
  track batches
- transfer playback
- add to playlist
- save track/episode
- library save/unsave by URI for tracks, albums, episodes, and artist
  follow/unfollow routing where the provider supports it
- cached cover-art and lyrics refresh for the current track

## Current fixes already applied

- Key-triggered TUI Spotify calls moved off the input loop.
- Empty Spotify write requests send explicit `Content-Length: 0` on
  playback, queue, save/unsave, follow/unfollow, and playlist-unfollow
  paths.
- Spotify OAuth now requests follow scopes, and token status reports missing
  scopes with a relogin hint.
- TUI queue, lyrics, and keymap rails are available without leaving the
  current screen; diagnostics/library refresh planning is automatic.
- TUI add-to-playlist opens an explicit picker instead of guessing a target.
- TUI diagnostics logs are filterable and keyboard-scrollable.
- TUI mouse support covers tabs, rows, progress seeking, right-rail controls,
  bottom-player play/pause, and bottom-player volume scrolling.
- Spotify search limit changed to the current valid max.
- Auth file reads/writes use private app config paths and fail clearly.
- Auth errors latch in the daemon; auth-error desktop notifications are
  deduped so unattended auth failures do not become a notification storm.
- embedded player device name set to `spotuify-hume`.
- legacy `[spotifyd] device_name` remains accepted as a migration fallback.
- daemon, local JSON IPC (Unix sockets on Unix and named pipes on Windows), workspace split, SQLite cache,
  operation receipts, typed Spotify errors, rate-limit handling,
  MCP stdio/HTTP surfaces, embedded librespot sink-chain wiring, local
  lyrics, and visualization plumbing have landed in later phases.
- Embedded player volume uses librespot's linear volume controller and
  re-applies the cached 0..100 volume when the device activates, so first
  playback does not start silently at a mis-scaled volume.
- Playback controls now use a daemon hot path: the command kind is frozen
  before optimistic state changes, embedded transport is tried within a
  bounded fast window, and the player actor services transport commands before
  normal commands and warm preloads.
- Queue additions schedule non-blocking warming for queued track metadata,
  cover art, lyrics, search-index rows, and next-track audio preload where the
  embedded backend supports it.
- `spotuify refresh-media` and TUI `U` refetch the current track's cover art
  and lyrics without clearing existing media before the new fetch returns.
- Playlist/album `PlayUri` commands publish `QueueChanged(play-context)` so
  Home, Queue, TUI rails, CLI watchers, MCP clients, and agents see the same
  context queue instead of an empty queue after context playback starts.
- Fast local resume/toggle skips ended or item-less playback snapshots, so
  Space after an ended track starts the selected item instead of sending a
  stale local resume to the embedded player.

## Current gaps

- Phase 14 media controls are live for the supported souvlaki path and
  route OS media-key commands through daemon playback requests. Discord
  remains scaffold-only until playback events carry rich metadata.
- Embedded sink visualization is attachable when built with
  `embedded-playback` plus exactly one librespot audio backend feature.
  Native PipeWire visualization remains an optional boundary; the cpal
  loopback path is the implemented default.
- TUI revamp plan items are implemented: playlist picker, full-screen
  queue/lyrics overlays, mouse controls for tabs/rows/progress/rails/bottom
  player, diagnostics log filtering, and playlist/album queue expansion are
  implemented and tested. Future polish should be planned as new scoped work.
- Implementation docs are now execution ledgers: checked items either
  have code/test evidence or are explicitly closed as pivots/follow-ups.

## Immediate risk

The broad surface can regress if plan docs drift from code. Current
verification should favor focused crate tests plus real CLI smoke paths
through daemon/IPC where user-visible behavior is involved.
