---
title: "Architecture"
description: "Read the daemon, protocol, cache, search, player, CLI, and TUI shape."
---

`spotuify` is a daemon-backed runtime. The daemon is the system. The CLI, TUI, MCP server, scripts, agents, and macOS app are clients.

## System shape

```text
TUI / CLI / MCP / macOS / Scripts / Agents
                    |
                    | length-delimited JSON
                    | Unix socket on Unix; named pipe on Windows
                    v
                 Daemon
                    |
                    +-- SQLite cache
                    +-- Tantivy search index
                    +-- Spotify Web API
                    +-- Spotify Connect player
```

Run the surfaces:

```bash
spotuify
spotuify status --format json
spotuify daemon status
```

## Startup state

Event-driven clients seed themselves from cached daemon state first. The TUI asks for `ClientSeed`, which returns playback, queue, devices, recent items, and visualizer status from the daemon/store layer without touching Spotify's Web API. Live provider refreshes stay in daemon-owned warm/sync loops so opening the TUI does not spend rate-limit budget before the user acts.

```bash
spotuify status --format json
spotuify queue --format json
spotuify devices --format json
```

## Playback events and provider reads have separate revisions

Native player events update the live playback clock before metadata and analytics
work. Provider reads capture both command and native-transition revisions before
fetching. Their commits recheck ownership and revisions; the clock performs its
final check under its write lock. This prevents an earlier read from restoring a
previous track after a native transition, including a repeat of the same URI.

A coalescing worker persists the current clock after acquiring the transport lane.
It does not replay an old queued event snapshot. SQLite orders equal snapshot
timestamps by insertion ID. The live clock and durable cache are separate stores:
failed I/O or cancellation can leave cached state behind.

An older provider state-change timestamp also rejects a different-track snapshot
for the active embedded device. That comparison needs timestamps and a meaningful
clock relationship; equal or missing timestamps do not establish order. It does
not suppress genuine foreign-device state.

Observe the current track and native transition events in separate terminals:

```bash
spotuify status --format json | jq '{uri: .item.uri, is_playing, progress_ms, source}'
spotuify events --kind playback-changed --format jsonl --timeout 15
```

The event timeout measures inactivity, not total runtime. Stop the watcher with
`Ctrl-C` when finished. For implementation and regression evidence, read the
[playback-state explanation](https://github.com/planetaryescape/spotuify/blob/f039672fafc762791eef9077117b40dedf4f261c/docs/build-log/recovery/2026-10-26-playback-poll-freshness.md).

## IPC buckets

| Bucket | Examples |
| --- | --- |
| `core-music` | playback, devices, queue, playlists, library, search |
| `spotuify-platform` | cache/index state, playlist plans, saved recipes |
| `admin-maintenance` | status, events, logs, doctor, reset, repair, reindex |
| `client-specific` | pane state, selected row, modal state |

Client-specific state stays out of daemon IPC.

## Local truth

SQLite is the cache. Tantivy is derived and rebuildable.

```bash
spotuify cache status --format json
spotuify reindex --format json
```

Analytics are local SQLite too. Observed playback becomes `listen_facts`; historical Last.fm import stores raw rows first, then promotes high-confidence matches into the same analytics tables with import provenance.

```bash
spotuify analytics top --kind tracks --since all --format json
spotuify analytics import lastfm --user your-lastfm-user --from 2024-01-01 --format json
```

## Copy from mxr

The docs and architecture deliberately copy mxr patterns before inventing new ones: Starlight docs, generated CLI reference, length-delimited JSON IPC, local store/search, output formats, and daemon/client separation.

```bash
spotuify search "quiet storm" --format jsonl
spotuify playlist add "Coding" spotify:track:... --dry-run
```

## Workspace responsibilities

The workspace has 18 packages: the root binary and 17 focused crates.

| Package | Job |
| --- | --- |
| `spotuify` | unified binary entry point |
| `spotuify-core` | domain types |
| `spotuify-protocol` | Request, Response, Event, IPC client |
| `spotuify-store` | SQLite tables and queries |
| `spotuify-search` | Tantivy indexing and local search |
| `spotuify-spotify` | Spotify adapter, auth, and Web API middleware |
| `spotuify-provider-fake` | deterministic provider and conformance harness |
| `spotuify-config` | provider-neutral config loading and migration |
| `spotuify-player` | provider-neutral playback backends |
| `spotuify-sync` | background cache and provider sync |
| `spotuify-daemon` | server, state, sync, handlers |
| `spotuify-launcher` | client-side daemon lifecycle and compatibility checks |
| `spotuify-cli` | clap commands and output |
| `spotuify-tui` | ratatui client |
| `spotuify-mcp` | MCP tools and resources |
| `spotuify-system` | media controls, notifications, hooks, Discord, cover cache |
| `spotuify-audio` | audio visualizer and loopback capture |
| `spotuify-lyrics` | lyrics providers, parsing, and cache support |

## See Also

- [IPC Protocol](/reference/ipc/)
- [Cache, Search, Sync](/guides/cache-search-sync/)
- [Implementation Roadmap](/guides/roadmap/)
