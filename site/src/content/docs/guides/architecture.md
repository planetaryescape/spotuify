---
title: "Architecture"
description: "Read the daemon, protocol, cache, search, player, CLI, and TUI shape."
---

`spotuify` is moving from a single binary shape to a daemon-backed runtime. The daemon is the system. The CLI, TUI, scripts, and agents are clients.

## System shape

```text
TUI / CLI / Scripts / Agents
          |
          | length-delimited JSON
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

## Copy from mxr

The docs and architecture deliberately copy mxr patterns before inventing new ones: Starlight docs, generated CLI reference, length-delimited JSON IPC, local store/search, output formats, and daemon/client separation.

```bash
spotuify search "quiet storm" --format jsonl
spotuify playlist add "Coding" spotify:track:... --dry-run
```

## Target crate responsibilities

| Crate | Job |
| --- | --- |
| `spotuify-core` | domain types |
| `spotuify-protocol` | Request, Response, Event, IPC client |
| `spotuify-store` | SQLite tables and queries |
| `spotuify-search` | Tantivy indexing and local search |
| `spotuify-spotify` | Spotify Web API mapping |
| `spotuify-player` | playback backend orchestration |
| `spotuify-daemon` | server, state, sync, handlers |
| `spotuify-cli` | clap commands and output |
| `spotuify-tui` | ratatui client |
| `spotuify-mcp` | MCP tools and resources |

## See Also

- [IPC Protocol](/reference/ipc/)
- [Cache, Search, Sync](/guides/cache-search-sync/)
- [Implementation Roadmap](/guides/roadmap/)
