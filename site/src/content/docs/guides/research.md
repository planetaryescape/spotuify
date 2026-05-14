---
title: "Research Notes"
description: "Summarize competitor research and the choices spotuify made from it."
---

The research set compares active Rust Spotify terminal tools and the older abandoned `spotify-tui`. Treat the notes as a snapshot, not permanent truth.

## Re-run date

```bash
date -u +"%Y-%m-%d"
```

The committed research was captured on 2026-05-13. Re-check before using any competitor claim in product copy or a design decision.

## What we found

| Project | What it taught spotuify |
| --- | --- |
| `ncspot` | Long-lived local player patterns, gapless preload, MPRIS discipline |
| `spotify-player` | Daemon-like shape, librespot token bridge, CLI precedent |
| `spotatui` | Modern Ratatui direction, audio visualization, recovery wrappers |
| `spotify-tui` | Historical UX baseline, and what breaks when APIs move |

## What spotuify keeps

```bash
spotuify daemon status
spotuify search "quiet storm" --source local
spotuify analytics top --kind tracks
```

Kept ideas: embedded librespot path, local cache, daemon/client split, recovery wrappers around audio backends, event hooks, cover art, and strong diagnostics.

## What spotuify rejects

```bash
spotuify doctor --format json
spotuify cache status --format json
```

Rejected ideas: UDP request/response for IPC, blocking sleeps on rate limits, plaintext token storage, hand-rolled command parsers, and JSON blob caches.

## Differentiation that actually means something

`spotuify` is not "user friendly" as a claim. Everyone says that. The real trade-off is:

- CLI as product contract, not a debugging side door.
- SQLite plus Tantivy instead of remote-only search.
- Daemon-backed playback instead of TUI-owned state.
- Agent workflows with dry-run mutations instead of blind automation.

## See Also

- [Architecture](/guides/architecture/)
- [Agents and MCP](/guides/agents-and-mcp/)
- [Roadmap](/guides/roadmap/)
