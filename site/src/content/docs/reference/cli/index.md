---
title: "CLI Reference"
description: "A keyboard-native Spotify TUI"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

A keyboard-native Spotify TUI

## Examples

```bash
spotuify
spotuify play "imagine dragons" --format json
spotuify search "luther vandross" --type track --format ids
```

## Help

```text
A keyboard-native Spotify TUI

Usage: spotuify [OPTIONS] [COMMAND]

Commands:
  onboard         Guided BYO Spotify app setup: config, browser login, and initial Spotify sync
  login           Log in to Spotify in your browser and store a refresh token in the local auth file
  logout          Remove the stored Spotify token from the local auth file
  auth            Authentication-adjacent debug commands
  doctor          Check config, auth, Spotify API access, and visible devices
  daemon          Manage the local spotuify daemon
  mcp             Run the MCP server transport
  status          Print current playback state
  devices         List visible Spotify Connect devices
  search          Search Spotify's catalog (or your local cache)
  search-page     Fetch a single page (10 items) of search results at a specific offset. Mirrors the TUI's scroll-load-more flow - useful for scripts walking past the 180-item streaming horizon
  resolve-tracks  Resolve playlist-plan track candidates
  queue           Print the current Spotify queue
  playlists       List the current user's playlists
  history         Listening history grouped into sessions (merges local plays + Spotify recently-played). Use --flat for a chronological track list
  play            Search Spotify and play the first matching result. Spotify URIs and open.spotify.com links skip the search and play directly
  play-uri        Play a Spotify URI directly
  next            Skip to the next track
  previous        Skip to the previous track
  pause           Pause playback
  resume          Resume playback
  toggle          Toggle play/pause
  seek            Seek relative to current playback position or to an absolute time
  volume          Set playback volume percent
  shuffle         Set or toggle shuffle
  repeat          Set repeat mode
  transfer        Transfer playback to a visible device by ID or name
  playlist        Playlist operations
  library         Cached library operations
  show            Podcast show operations
  album           Album operations
  artist          Artist operations
  radio           Mercury-backed radio stations
  lyrics          Synced lyrics operations
  reminder        Schedule and manage listening reminders
  notifications   View and act on reminder notifications (the inbox)
  refresh-media   Refresh current track cover art and lyrics
  viz             Configure the audio visualizer
  hooks           Test configured shell hooks
  mpris           Inspect OS media-control integration
  like            Save/like a Spotify URI or the current now-playing item
  unlike          Remove (un-like) a Spotify URI from the library
  save            Save a Spotify URI or the current now-playing item
  logs            Show spotuify log file location or recent log lines
  config          Read or write the current instance config file
  analytics       Inspect local analytics data
  ops             Inspect / undo / redo recorded operations (Phase 12)
  generate        Phase 13 (P13-J) - emit shell completions or a man page
  reload          Phase 13 (P13-I) - ask the running daemon to reload `config.toml`
  reconnect       Phase 13 (P13-I) - force the daemon to re-register its embedded player (after a VPN flap, network change, etc)
  audio-outputs   List the local audio output devices the embedded player can render to (the system speakers/headphones spotuify-hume plays through)
  audio-output    Choose which local audio output the embedded player renders to. Applies live: the daemon rebinds its sink in-process and resumes the interrupted track where it left off. Pass `default` (or empty) to follow the system default output again. Name must match one from `spotuify audio-outputs`
  bug-report      Phase 13 (P13-D) - bundle a redacted diagnostic tarball for bug reports. Never auto-uploads; the user inspects + shares it
  reindex         Rebuild the local search index from SQLite cache
  cache           Inspect local cache state
  sync            Refresh local cache from Spotify
  update          Check whether a newer spotuify release is available and how to upgrade
  episodes        A flat, date-ordered episode feed across all the podcasts you follow
  help            Print this message or the help of the given subcommand(s)

Options:
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
  -V, --version                  Print version
```
