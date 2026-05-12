# spotuify - Roadmap

## Guiding principle

Each phase must produce a usable improvement. Do not build inert infrastructure that cannot be exercised by CLI or TUI.

## Phase 0 - Stabilize current app

Goal: current TUI and Spotify API behavior are reliable enough to build on.

Deliverables:

- [ ] Search limit fixed and covered by CLI smoke test.
- [ ] Keychain calls bounded everywhere.
- [ ] Preferred spotifyd device visible and verified by doctor.
- [ ] Playback actions do not block TUI input.
- [ ] Doctor completes under bounded time.
- [ ] Release build verified.

Definition of done: `spotuify doctor`, `spotuify search`, and `spotuify play` can prove auth, search, device activation, and playback without opening the TUI.

## Phase 1 - CLI parity for current features

Goal: everything the current TUI can do is available from CLI.

Deliverables:

- [ ] Shared action layer extracted from TUI.
- [ ] CLI status/devices/search/playback/queue/playlists/library commands.
- [ ] Output formats: table, json, jsonl, csv, ids.
- [ ] Dry-run for playlist and bulk actions where feasible.
- [ ] CLI smoke tests.

Definition of done: TUI uses the same actions as CLI; no Spotify operation is TUI-only.

## Phase 2 - Daemon and IPC

Goal: CLI and TUI talk to a daemon over JSON IPC.

Deliverables:

- [ ] Protocol types.
- [ ] Daemon start/status/stop/restart.
- [ ] Socket server.
- [ ] CLI client.
- [ ] TUI client.
- [ ] Event stream.
- [ ] Action trace.

Definition of done: TUI has no direct Spotify API calls; daemon owns Spotify and player operations.

## Phase 3 - SQLite cache and Tantivy search

Goal: local library/search is fast and useful.

Deliverables:

- [ ] SQLite schema and migrations.
- [ ] Sync for playback/devices/playlists/recent/library.
- [ ] Tantivy index from SQLite.
- [ ] Local and remote search source modes.
- [ ] Reindex command.
- [ ] Cache status diagnostics.

Definition of done: library/playlists can be searched locally without waiting on Spotify.

## Phase 4 - TUI redesign

Goal: player-first TUI with mxr-style UX.

Deliverables:

- [ ] Player tab.
- [ ] Search tab with filters and groups.
- [ ] Library tab.
- [ ] Contextual hint bar.
- [ ] Command palette.
- [ ] Searchable help.
- [ ] Multi-select and bulk actions.
- [ ] Diagnostics tab.

Definition of done: TUI is a pleasant controller, not a source of fragile app logic.

## Phase 5 - Agent playlist workflows

Goal: agents can research, preview, and create playlists safely.

Deliverables:

- [ ] Playlist plan schema.
- [ ] Candidate resolution command.
- [ ] Dry-run playlist creation preview.
- [ ] Commit command with mutation receipt.
- [ ] JSON examples and recipes.

Definition of done: an agent can create a playlist from a brief using only CLI commands and explicit approval.
