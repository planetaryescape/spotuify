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
  onboard         Guided first-run setup: config, OAuth, and initial Spotify sync
  login           Authorize Spotify and store a refresh token in macOS Keychain
  logout          Remove the stored Spotify token from macOS Keychain
  doctor          Check config, auth, Spotify API access, and visible devices
  daemon          Manage the local spotuify daemon
  status          Print current playback state
  devices         List visible Spotify Connect devices
  search          Search local cache and Spotify
  resolve-tracks  Resolve playlist-plan track candidates
  queue           Print the current Spotify queue
  playlists       List the current user's playlists
  play            Search Spotify and play the first matching result
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
  lyrics          Synced lyrics operations
  viz             Configure the audio visualizer
  like            Save/like a Spotify URI or the current now-playing item
  save            Save a Spotify URI or the current now-playing item
  logs            Show spotuify log file location or recent log lines
  config          Read or write ~/.config/spotuify/spotuify.toml
  analytics       Inspect local analytics data
  ops             Inspect / undo / redo recorded operations (Phase 12)
  generate        Phase 13 (P13-J) - emit shell completions or a man page
  reload          Phase 13 (P13-I) - ask the running daemon to reload `config.toml`
  reconnect       Phase 13 (P13-I) - force the daemon to rebuild its upstream Spotify session (after a VPN flap, network change, etc)
  bug-report      Phase 13 (P13-D) - bundle a redacted diagnostic tarball for bug reports. Never auto-uploads; the user inspects + shares it
  reindex         Rebuild the local search index from SQLite cache
  cache           Inspect local cache state
  sync            Refresh local cache from Spotify
  help            Print this message or the help of the given subcommand(s)

Options:
      --log-format <LOG_FORMAT>  Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --no-daemon-start          Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>     Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                     Print help
  -V, --version                  Print version
```
