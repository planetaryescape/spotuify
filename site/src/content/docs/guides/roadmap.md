---
title: "Implementation Roadmap"
description: "Track current phases, shipped surfaces, and remaining work."
---

The roadmap is phased so every step leaves a usable command behind. No inert infrastructure, no TUI-only capability.

## Current command check

```bash
spotuify --help
spotuify doctor
scripts/cargo-test -p spotuify-cli --tests
```

## Phases

| Phase | Goal | Verification |
| --- | --- | --- |
| 0 | Stabilize current app | `spotuify doctor`, `spotuify search`, playback smoke |
| 1 | CLI parity | status/devices/search/playback/queue/playlists/library commands |
| 2 | Daemon and IPC | CLI/TUI use socket, daemon survives TUI exit |
| 3 | SQLite and Tantivy | local search works without waiting on Spotify |
| 4 | TUI redesign | player-first UI, hint bar, palette, diagnostics |
| 5 | Agent playlists | plan, resolve, dry-run, commit |
| 6 | Sync hardening | rate limits, freshness, snapshot gates |
| 7 | Workspace split | real crate boundaries |
| 8 | MCP server | tools/list, tool routing, live resources |
| 9 | Embedded librespot | one auth flow, local playback backend |
| 10 | Analytics | derived listens, top-N, habits, live hook recipes |
| 10 | Analytics | derived listens, top-N, habits, Last.fm import |
| 11 | Cross-platform | launchd, systemd user, Task Scheduler |
| 12 | Operation log and undo | recorded mutations with reversal plans |
| 13 | Spec compliance and QoL | reload, reconnect, overrides, bug reports |
| 14 | System integration | media keys, notifications, hooks |
| 15 | Cover art | inline art plus fallbacks |
| 16 | Lyrics | synced lyrics and offset tuning |
| 17 | Audio visualization | FFT spectrum via sink tap or loopback |

## Recent audit pass

A full-codebase audit (2026-06) shipped reliability + feature work across the
phases above: per-request IPC timeouts, search-service timeouts, a player
session-health/auto-reconnect loop, an atomic daemon startup lock + IPC
peer-credential check, OAuth localhost-redirect warnings, full macOS
`DaemonRequest` protocol parity (enforced by a fixture test), rich
notifications/media-controls/hooks + Discord Rich Presence from playback
events, Mercury-backed related-artists + radio (`spotuify artist related`,
`spotuify radio start`), sink-accurate audible time for analytics,
playlist-level top-k, TUI delete-playlist/bulk-unsave, MCP live resource
push (stdio), and a terminal/cover-art section in `doctor`.

Deliberately not done (see `docs/blueprint/13-decision-log.md` D019–D023):
removed the never-functional `analytics export/import` stubs; deferred the
`dispatch` god-function split and the `spotuify-launcher` crate extraction
(pure-layering refactors); Windows SMTC and CLI notarization (need a Windows
machine / Apple CI credentials). Row thumbnails, manual lyrics-provider
selection, native PipeWire capture, AUR/Scoop, and MCP-over-HTTP push remain
explicit won't-dos.

## Do not skip the CLI

Every new user-visible capability needs a real command:

```bash
spotuify <capability> --format json
```

If a capability has only a TUI button, it is incomplete.

## See Also

- [Architecture](/guides/architecture/)
- [CLI Reference](/reference/cli/)
- [Research Notes](/guides/research/)
