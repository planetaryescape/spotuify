# Current State

## What exists

Current spotuify is a single Rust binary with modules for app/TUI, auth, config, logging, Spotify API, spotifyd, and UI.

Implemented CLI commands:

- `onboard`
- `login`
- `logout`
- `doctor`
- `logs path|tail`
- `config path|init|get|set`

Implemented TUI areas:

- search
- queue
- playlists
- devices
- now playing/player header

Implemented Spotify API capabilities:

- playback state
- devices
- queue read
- search tracks/episodes/albums/playlists
- playlists
- recently played
- playlist tracks
- play/pause
- play URI/context
- next/previous
- seek
- volume
- shuffle
- repeat
- add to queue
- transfer playback
- add to playlist
- save track/episode

## Current fixes already applied

- Key-triggered TUI Spotify calls moved off the input loop.
- Spotify search limit changed to the current valid max.
- Keychain reads/writes bounded to avoid indefinite hangs.
- spotifyd config path set to the user's dotfiles path.
- preferred spotifyd device name set to `spotuify-hume`.

## Current gaps

- CLI has no playback/search/library/playlist command surface yet.
- TUI still owns action orchestration in `app.rs`.
- No daemon.
- No IPC protocol.
- No SQLite cache.
- No local search index.
- No action registry for hints/help/palette.
- No bulk selection model.
- No conformance suite for CLI/TUI parity.

## Immediate risk

The app can regress because the only easy verification path has been TUI interaction. Phase 1 must fix this by making CLI commands the testable surface.
