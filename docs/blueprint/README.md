# spotuify - Technical Blueprint

> A daemon-backed, CLI-first, keyboard-native Spotify controller and music library runtime for terminal users, built around a local cache, pipeable commands, and an impeccable player experience.

## Document index

| # | Document | What it covers |
|---|---|---|
| 00 | [Overview](00-overview.md) | Product identity, principles, non-goals, differentiators |
| 01 | [Architecture](01-architecture.md) | Daemon, clients, IPC, crate/module boundaries |
| 02 | [Data Model](02-data-model.md) | Core entities, IDs, SQLite source of truth, mutation receipts |
| 03 | [Spotify Provider](03-spotify-provider.md) | Web API, spotifyd/librespot, capability limits, rate limits |
| 04 | [Sync and Cache](04-sync-cache.md) | Background sync, local truth, optimistic updates, reconciliation |
| 05 | [Search](05-search.md) | Local Tantivy search, Spotify search, filters, query semantics |
| 06 | [CLI and Shell Integration](06-cli.md) | Canonical CLI surface, formats, stdin IDs, dry-run, exit codes |
| 07 | [Player](07-player.md) | Player-first contract, playback controls, device activation, queue |
| 08 | [TUI](08-tui.md) | Layout, command palette, contextual hints, search/filter UX |
| 09 | [Agent Workflows](09-agent-workflows.md) | Agent-native design, researched playlists, preview/commit loops |
| 10 | [Observability](10-observability.md) | Doctor, logs, diagnostics, action traces, bug reports |
| 11 | [Config and Auth](11-config-auth.md) | Config paths, OAuth, keychain, spotifyd configuration |
| 12 | [Roadmap](12-roadmap.md) | Phased milestones and definitions of done |
| 13 | [Decision Log](13-decision-log.md) | Settled decisions and alternatives considered |
| 14 | [Reuse Strategy](14-reuse-strategy.md) | Copy-from-mxr policy, reusable crate candidates, extraction thresholds |
| 15 | [Coding Rules](15-coding-rules.md) | Architecture, CLI/TUI parity, errors, mutation, output, and verification rules |
| 16 | [Analytics](16-analytics.md) | First-class listening/search/action analytics, local event store, derived metrics |

## For coding agents

The CLI is the canonical product surface. If a capability can only be done in the TUI, it is incomplete.

The TUI is a client. It should render state, dispatch actions, and show progress. It must not own durable playback/library/search truth.

The daemon is the runtime. It owns auth, Spotify API access, local cache, search index, sync, device activation, and mutation reconciliation.

Machine output is part of the product contract. `json`, `jsonl`, `csv`, and `ids` outputs must stay stable enough for scripts and agents.

When mxr has already solved an architectural layer, copy first and adapt. Do not reinvent daemon/IPC/store/search/TUI plumbing unless Spotify's domain forces a different shape.

## Core stack target

| Component | Technology |
|---|---|
| Language | Rust |
| Async runtime | Tokio |
| Local store | SQLite |
| Search index | Tantivy |
| TUI | Ratatui + crossterm |
| HTTP/API client | reqwest |
| CLI parser | clap |
| Credentials | macOS Keychain first, cross-platform keyring later |
| Playback device | spotifyd/librespot or external Spotify Connect device |
| IPC | JSON over Unix socket |
