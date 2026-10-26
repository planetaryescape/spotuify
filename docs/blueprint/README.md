# spotuify - Technical Blueprint

> A daemon-backed, CLI-first, keyboard-native Spotify controller and music library runtime for terminal users, built around a local cache, pipeable commands, and an impeccable player experience.

## Document index

| # | Document | What it covers |
|---|---|---|
| 00 | [Overview](00-overview.md) | Product identity, principles, non-goals, differentiators |
| 01 | [Architecture](01-architecture.md) | Daemon, clients, IPC, crate/module boundaries |
| 02 | [Data Model](02-data-model.md) | Core entities, IDs, SQLite source of truth, mutation receipts |
| 03 | [Spotify Provider](03-spotify-provider.md) | Web API, embedded librespot, capability limits, rate limits |
| 04 | [Sync and Cache](04-sync-cache.md) | Background sync, local truth, optimistic updates, reconciliation |
| 05 | [Search](05-search.md) | Local Tantivy search, Spotify search, filters, query semantics |
| 06 | [CLI and Shell Integration](06-cli.md) | Canonical CLI surface, formats, stdin IDs, dry-run, exit codes |
| 07 | [Player](07-player.md) | Player-first contract, playback controls, device activation, queue |
| 08 | [TUI](08-tui.md) | Layout, command palette, contextual hints, search/filter UX |
| 09 | [Agent Workflows](09-agent-workflows.md) | Agent-native design, researched playlists, preview/commit loops |
| 10 | [Observability](10-observability.md) | Doctor, logs, diagnostics, action traces, bug reports |
| 11 | [Config and Auth](11-config-auth.md) | Config paths, OAuth, auth files, player configuration |
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
| Credentials | Private auth files under the app config directory |
| Playback device | embedded librespot plus Spotify Connect device control |
| IPC | Length-delimited JSON over Unix sockets on Unix and named pipes on Windows |


## The listening repairs start at chapter 17

The 2026-10-26 extension is pinned to `origin/main` at `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e`. It repairs current search, selection, provider error, diagnostics and device ownership paths before adding features (D-042). The original chapters remain the design history.

| # | Document | What it establishes |
|---|---|---|
| 17 | [Repair the listening journeys](17-reliability-recovery.md) | Verified inventory and architecture boundary |
| 18 | [Give the music one readable place](18-listening-desk.md) | Proposed TUI concept, journeys and visual gate |
| 19 | [Respect playback ownership](19-playback-and-provider-corrections.md) | Player recovery, provider access, CLI references and doctor |
| 20 | [Follow the visible selection](20-search-and-action-corrections.md) | Search, category navigation and contextual actions |
| 21 | [Isolate native build state](21-native-ci-corrections.md) | macOS smoke, local runner and signing |
| 22 | [Reuse corrections with evidence](22-recovery-reuse-map.md) | Adopt/reject/defer from recent mxr work |
| 23 | [Finish one listening journey per rung](23-recovery-roadmap.md) | Usable correction ladder and completion gates |
| 24 | [Keep unknowns testable](24-recovery-questions.md) | Spikes and assumptions requiring BK's judgement |
| — | [Resume the correction work](KICKOFF.md) | Next-agent prompt and handover |

Research: [runtime and UX evidence](../research/2026-10-26-reliability-audit.md), [mxr reuse review](../research/2026-10-26-mxr-reuse.md).

The concept is a blueprint assumption: one primary list, quiet player anchor, detail on demand (Q1). Preserve `e` as queue (D-046), distinguish deactivation from lost connection (D-044), and keep skipped checks from implying provider health (D-048). BK approved the dedicated runner and blueprint orchestration on 2026-10-26; S5 tracks native workflow proof. Follow [the implementation handover](../implementation/2026-10-26-recovery-handover.md) for current evidence and exact next actions.
