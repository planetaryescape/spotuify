# Phase 2 - Daemon and Protocol

## Goal

Move Spotify/auth/player ownership into a daemon. CLI and TUI become clients.

## Deliverables

- `spotuify daemon start|stop|restart|status`.
- Local JSON IPC protocol. The original phase shipped Unix sockets; current code uses Unix sockets on Unix and named pipes on Windows through `spotuify-protocol::ipc_stream`.
- Request/Response/Event types.
- CLI client wrapper.
- TUI client wrapper.
- Daemon-owned Spotify client and embedded player lifecycle.
- Event stream for playback, sync, mutation, and error events.

## Protocol starter set

Requests:

- `Status`
- `Doctor`
- `DevicesList`
- `DeviceTransfer`
- `PlaybackGet`
- `PlaybackCommand`
- `Search`
- `QueueGet`
- `QueueAdd`
- `PlaylistsList`
- `PlaylistTracks`
- `PlaylistAddItems`
- `LibrarySave`
- `SyncTrigger`
- `LogsTail`

Events:

- `PlaybackChanged`
- `DeviceChanged`
- `SyncStarted`
- `SyncFinished`
- `MutationAccepted`
- `MutationFinished`
- `RateLimited`
- `AuthError`

## Lifecycle

- `spotuify` with no command starts TUI and autostarts daemon if needed.
- CLI commands autostart daemon unless `--no-daemon-start` is passed.
- `spotuify daemon --foreground` supports debugging.

## Verification

- daemon starts and writes socket
- CLI can connect and run `status`
- TUI can render daemon `status`
- daemon survives TUI exit
- one CLI command can run while TUI is open

## Definition of done

No TUI code directly calls Spotify. Daemon owns auth, Spotify API, and player actions.
