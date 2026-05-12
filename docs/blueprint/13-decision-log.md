# spotuify - Decision Log

This document records settled decisions so future agents do not re-litigate them without new evidence.

## D001: Architecture - daemon-backed, not TUI-owned

Chosen: daemon-backed runtime with CLI/TUI clients.

Considered:

- monolithic TUI that calls Spotify directly
- CLI-only controller
- daemon-backed runtime

Why:

- music must continue after TUI exits
- CLI and agents need the same capabilities
- local cache/search requires background work
- TUI state should not be durable app state

## D002: CLI is canonical

Chosen: CLI-first product surface.

Why:

- every action becomes testable
- agents can use the app safely
- scripts and pipelines become first-class
- TUI-only features are hard to verify and easy to break

## D003: Playback device - use Spotify Connect, not Web API audio

Chosen: controller plus Spotify Connect device.

Why:

- Spotify Web API does not stream audio
- spotifyd/librespot or official apps are the playback devices
- spotuify should control, not impersonate a streaming client unless we deliberately embed librespot later

## D004: Search - local first, Spotify remote as provider

Chosen: SQLite/Tantivy local search plus live Spotify search.

Why:

- saved library and playlist data should be instant
- remote API is rate-limited and occasionally flaky
- agents need repeatable search results

## D005: Output formats are stable product contract

Chosen: table/json/jsonl/csv/ids on data commands.

Why:

- Unix composition
- agent integration
- testability
- less screen scraping

## D006: Lyrics are optional provider, not core Spotify feature

Chosen: no core lyrics promise until a legal/provider-backed source exists.

Why:

- Spotify Web API does not expose official lyrics
- unreliable scraping would make the player feel broken

## D007: TUI UX follows contextual action registry

Chosen: action registry drives hint bar, command palette, help, and command availability.

Why:

- prevents hidden keymap mystery
- keeps hints contextual
- makes CLI/TUI parity auditable
- supports future configurable keymaps

## D008: Implementation strategy - copy mxr before inventing

Chosen: copy/adapt mxr implementations for daemon, IPC, SQLite, Tantivy, CLI output, mutation discipline, and TUI async/action plumbing wherever the shape matches.

Considered:

- greenfield spotuify-specific architecture
- copying mxr first, then extracting shared crates after repetition is proven
- extracting shared crates before spotuify uses the patterns

Why copy first:

- mxr has already paid the design/debugging cost for local daemon architecture
- daemon/IPC/store/search mechanics are nearly identical across these terminal-native apps
- copy/paste/adapt is faster and safer than designing abstractions too early
- after two or three apps share the same shape, extraction targets become obvious

Future extraction candidates:

- local JSON IPC codec/client/server
- daemon lifecycle and socket management
- CLI output rendering formats
- mutation preview/confirmation/receipt helpers
- TUI action registry, keymaps, hint bar, command palette
- SQLite/Tantivy sync/index scaffolding

Do not abstract before the second real use case proves the seam.
