# Phase 1 - CLI Parity

## Goal

Everything the current TUI can do must be available from CLI.

## Architecture step

Extract TUI-private action code into a shared action module before adding broad CLI surface.

Target modules in current single-crate phase:

```text
src/actions.rs       # CommandKind, CommandResult, shared action execution
src/output.rs        # table/json/jsonl/csv/ids renderers
src/selection.rs     # resolve URI/ID/search/current selections
src/commands.rs      # clap command handlers
```

## CLI commands for parity

Playback:

```text
spotuify status
spotuify play QUERY
spotuify play-uri URI
spotuify pause
spotuify resume
spotuify toggle
spotuify next
spotuify previous
spotuify seek +15s
spotuify volume 70
spotuify shuffle on|off|toggle
spotuify repeat off|context|track
```

Search:

```text
spotuify search QUERY --type track|album|artist|playlist|episode --format table|json|jsonl|csv|ids
spotuify search QUERY --play --index N
```

Devices:

```text
spotuify devices
spotuify transfer DEVICE
```

Queue:

```text
spotuify queue
spotuify queue add URI
spotuify queue add --search QUERY
```

Playlists:

```text
spotuify playlists
spotuify playlist tracks PLAYLIST
spotuify playlist play PLAYLIST
spotuify playlist add PLAYLIST URI
spotuify playlist add-current PLAYLIST
```

Library:

```text
spotuify like current
spotuify save current
```

## Renderer contract

Data commands support:

- `table`
- `json`
- `jsonl`
- `csv`
- `ids`

Initial implementation can use serde structs and simple render helpers. Do not print human strings from the shared action layer.

## Verification

Add tests for:

- clap parsing
- output format snapshots
- search command returns valid output shape using mocked provider later
- no TUI-only action remains for current feature set

Manual smoke:

```text
spotuify doctor
spotuify devices --format json
spotuify search "luther vandross" --type track --format json
spotuify play "luther vandross"
spotuify next
spotuify pause
```

## Definition of done

TUI action code calls the same shared action layer as CLI. The CLI can test every current Spotify capability without opening the TUI.
