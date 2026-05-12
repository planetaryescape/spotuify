# spotuify - Project Overview

## One-line pitch

A daemon-backed, CLI-first Spotify controller and local music library runtime for terminal users.

## Extended pitch

spotuify is a terminal-native music app where the CLI is the stable automation surface, the TUI is a fast human controller, and a daemon keeps Spotify state, local cache, search, and playback control alive even when the TUI is closed.

It should feel like a proper music player, not a fragile Web API demo. Playback controls, device activation, queueing, search, playlists, likes, follows, and bulk actions need to be reliable from both CLI and TUI.

## What spotuify is

- A Spotify controller for terminal users.
- A CLI-first local music runtime with a TUI on top.
- A daemon-backed system where background sync and playback state continue after the TUI exits.
- A local cache of user library, playlists, recent tracks, and discovered search results.
- A pipeable Unix tool with stable machine-readable output.
- A music-player UX where the player is central, not decorative.
- An agent-native tool that can be scripted to research, preview, and create playlists.

## What spotuify is not

- Not an audio streaming implementation through the Spotify Web API. The Web API controls playback; it does not stream audio.
- Not a TUI-only app. The TUI is one client.
- Not a generic downloader. Spotify content policy and API limits apply.
- Not dependent on AI. Agents can use it, but core behavior must remain deterministic and inspectable.
- Not allowed to hide broken playback behind a pretty interface.

## Core principles

### 1. Player first

If play, pause, seek, shuffle, repeat, queue, device activation, and track selection are unreliable, the app is broken. UI polish does not compensate for a flaky player.

### 2. Daemon-backed architecture

The daemon is the system. TUI, CLI, shell scripts, and agents are clients. Closing the TUI must never stop music.

### 3. CLI first, TUI supported

Every meaningful capability must be reachable from the CLI. The TUI can make common flows faster, but it cannot be the only surface.

### 4. Unix composition

Commands must compose with `jq`, `xargs`, `fzf`, shell scripts, and agent runtimes. Stable IDs and machine-readable output are product features.

### 5. Local cache before network round trips

Saved library, playlists, recent tracks, and search history should be cached locally. The app should answer from local state when it can and reconcile with Spotify in the background.

### 6. Search is navigation

Search is not just a Spotify API call. Local library search, playlist filtering, result refinement, and remote catalog search are all navigation primitives.

### 7. Safe mutations

Playlist creation, playlist edits, bulk likes, bulk queueing, and follows should support preview or dry-run where feasible. Agents should never have to mutate blindly.

### 8. Observability is product UX

When Spotify, keychain, network, spotifyd, or the daemon fails, the app must explain what failed and what command fixes it.

## Differentiators

| Feature | Basic Spotify CLI | Typical Spotify TUI | spotuify target |
|---|---|---|---|
| CLI as canonical API | Partial | No | Yes |
| Daemon-backed cache | Rare | Rare | Yes |
| TUI independent of playback process | Mixed | Mixed | Yes |
| JSON/JSONL/CSV/IDs output | Inconsistent | No | Yes |
| Local search index | Rare | Rare | Yes |
| Agent playlist workflows | No | No | Yes |
| Contextual TUI action hints | Rare | Mixed | Yes |
| Dry-run mutation previews | Rare | No | Yes |
