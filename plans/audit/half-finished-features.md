# Half-Finished Features Audit

Scope: partially implemented or abandoned user-visible logic, especially no-op branches, placeholder behavior, and CLI/TUI/MCP/daemon parity gaps.

## Summary

- Findings: 6
- Priority split: P1: 2, P2: 3, P3: 1
- Highest-risk theme: mutation surfaces are partly wired but bypass one of the project contracts: CLI-everywhere, daemon-owned optimistic state, previewable mutations, or operation-log undo.

## Findings

### P1: `playlist_remove` is MCP/daemon-only; CLI has no equivalent

Issue: Playlist item removal is exposed to MCP and handled by the daemon, but there is no `spotuify playlist remove` CLI subcommand.

Evidence:
- MCP advertises `playlist_remove` as a destructive tool: `crates/spotuify-mcp/src/tools.rs:188`.
- MCP translates it to `Request::PlaylistRemoveItems`: `crates/spotuify-mcp/src/bridge.rs:250`.
- The daemon executes `Request::PlaylistRemoveItems`: `crates/spotuify-daemon/src/handlers/playlists.rs:153`.
- `PlaylistCommand` has `Plan`, `Create`, `Tracks`, `Play`, `Add`, `AddCurrent`, `Unfollow`, and `SetImage`, but no `Remove`: `crates/spotuify-cli/src/cli_args.rs:284`.

Impact: Agents can mutate playlist contents through MCP in a way humans/scripts cannot drive through the canonical CLI. This violates the CLI-everywhere contract and leaves no obvious shell path to validate or undo-test the same workflow.

Recommended action: Add `spotuify playlist remove <playlist> <uris...|--ids FILE> --dry-run --yes --format ...`, route it to `PlaylistRemoveItems`, and add help snapshot + fake-daemon tests. Consider a TUI contextual remove only if the UX is already validated.

Confidence: High.

Validation idea: `spotuify playlist remove --help` snapshot exists; fake daemon asserts the CLI emits `playlist-remove-items`; MCP and CLI route to the same request shape.

### P1: TUI artist follow/unfollow mutates local UI state and ignores failures

Issue: The artist detail view flips `view.is_followed` locally before the daemon confirms the mutation, then drops the async error path.

Evidence:
- TUI comment says the action is fire-and-forget: `crates/spotuify-tui/src/app.rs:3742`.
- TUI flips local state directly: `crates/spotuify-tui/src/app.rs:3747`.
- The spawned request ignores failure beyond an empty `if request_data(...).await.is_err()` branch: `crates/spotuify-tui/src/app.rs:3755`.
- Daemon only emits `LibraryChanged` after the Spotify call succeeds for follow/unfollow: `crates/spotuify-daemon/src/handlers/library.rs:306` and `crates/spotuify-daemon/src/handlers/library.rs:340`.

Impact: On a 403, auth failure, or network failure, the TUI can show the artist as followed/unfollowed even though Spotify rejected it. Other clients also miss the optimistic state because the mutation happened in the TUI, not through a daemon-owned optimistic event.

Recommended action: Move the optimistic follow-state update into the daemon event path or remove the local flip and refresh after `LibraryChanged`. Surface request failures as a toast/banner and roll back if retaining optimistic UI.

Confidence: High.

Validation idea: Add a TUI/app-state test or fake daemon path where `ArtistFollow` fails; assert the followed state does not remain flipped and an error is surfaced.

### P2: Artist follow/unfollow operation records have no undo plan

Issue: Artist follow/unfollow has operation kinds and CLI/TUI surfaces, but is not reversible and captures no pre-state/reversal plan.

Evidence:
- `OperationKind` includes `ArtistFollow` and `ArtistUnfollow`: `crates/spotuify-protocol/src/operations.rs:173`.
- `OperationKind::is_reversible` omits both artist variants: `crates/spotuify-protocol/src/operations.rs:228`.
- Daemon records follow with `None, None` for pre-state and plan: `crates/spotuify-daemon/src/handlers/library.rs:285`.
- Daemon records unfollow with `None, None` for pre-state and plan: `crates/spotuify-daemon/src/handlers/library.rs:319`.
- CLI exposes both commands: `crates/spotuify-cli/src/cli_args.rs:149`.
- TUI exposes a follow toggle: `crates/spotuify-tui/src/app.rs:3750`.

Impact: The operation log can record these mutations, but `ops undo` cannot reverse them even though the inverse action is available. This weakens the safety net for account/library mutations.

Recommended action: Add artist pre-state/reversal variants or reuse library-like save/unsave semantics with `prior_was_followed`, mark the operation kinds reversible, and wire undo execution to follow/unfollow artist.

Confidence: Medium-high.

Validation idea: Unit-test `ArtistFollow` and `ArtistUnfollow` undo planning; fake-daemon test `artist follow`, then `ops undo --dry-run`, then `ops undo --yes`.

### P2: `radio_start` queues tracks outside the normal mutation/receipt path

Issue: `radio_start` resolves a station and, unless `dry_run` is set, queues every resolved track directly with best-effort error swallowing.

Evidence:
- MCP classifies `radio_start` as non-destructive transport even though the description says it queues tracks: `crates/spotuify-mcp/src/tools.rs:232`.
- MCP defaults `dry_run` to false: `crates/spotuify-mcp/src/bridge.rs:291`.
- Daemon queues every resolved URI when `dry_run` is false: `crates/spotuify-daemon/src/handlers/library.rs:171`.
- Per-track queue failures are only debug-logged and do not fail the response: `crates/spotuify-daemon/src/handlers/library.rs:176`.

Impact: A batch queue mutation can partially fail while the caller receives `MediaItems` as if the station started successfully. It also bypasses the queue mutation receipt/operation-log path, so users and agents have poor auditability.

Recommended action: Route non-dry-run radio through the same queue mutation path as `QueueAddMany`, or return a structured receipt with queued/failed counts. Consider defaulting MCP to preview/dry-run or requiring confirmation for the queueing mode.

Confidence: Medium.

Validation idea: Fake a queue failure for one URI and assert `radio start` returns a partial-failure receipt or non-zero status; assert the operation log records the batch when queueing happens.

### P2: Playlist set-image and unfollow lack CLI dry-run previews

Issue: Two destructive playlist mutations do not have CLI dry-run previews even though the project requires broad/destructive mutations to be previewable where feasible.

Evidence:
- `playlist set-image` args only include playlist, file, and format; no `--dry-run` or `--yes`: `crates/spotuify-cli/src/cli_args.rs:375`.
- `ipc_playlist_set_image` reads/validates the image and immediately sends `Request::PlaylistSetImage`: `crates/spotuify-cli/src/commands.rs:1053`.
- `playlist unfollow` has `--yes`, but no dry-run preview: `crates/spotuify-cli/src/cli_args.rs:359`.
- `ipc_playlist_unfollow` confirms interactively or sends `Request::PlaylistUnfollow`: `crates/spotuify-cli/src/commands.rs:1064`.

Impact: Scripts cannot get a machine-readable preview for high-impact playlist changes. MCP has preview-only behavior for destructive calls, so CLI and MCP safety semantics diverge.

Recommended action: Add `--dry-run` to both commands. For set-image, preview resolved playlist, source path/stdin, byte sizes, JPEG validation result, and irreversibility. For unfollow, preview resolved playlist id/name/owner and irreversibility.

Confidence: Medium-high.

Validation idea: CLI tests prove `--dry-run --format json` emits preview JSON and fake daemon sees no mutation request.

### P3: MCP `now_playing` promises lyrics but only requests playback

Issue: MCP manifest copy says `now_playing` returns lyrics when available, but the bridge only sends `PlaybackGet`.

Evidence:
- Tool description promises "lyrics if available": `crates/spotuify-mcp/src/tools.rs:55`.
- Bridge maps `now_playing` to `Request::PlaybackGet` only: `crates/spotuify-mcp/src/bridge.rs:91`.
- `Playback` has item/device/progress fields, but no lyrics field: `crates/spotuify-core/src/lib.rs:28`.
- Lyrics are a separate response path: `crates/spotuify-protocol/src/lib.rs:1148`.

Impact: Agents relying on the manifest will not receive the advertised lyrics payload and may make an unnecessary second call or act on missing data.

Recommended action: Either change the description to playback-only, or make `now_playing` an aggregate workflow that calls `PlaybackGet` plus `LyricsGet` for the current track.

Confidence: High.

Validation idea: Update the MCP manifest snapshot if copy changes, or add an RPC test proving `now_playing` includes a lyrics field when aggregation is implemented.

### P3: macOS library refresh omits saved shows after library changes

Issue: macOS `LibraryStore` has a `loadShows` path, but `LibraryChanged` refreshes liked songs, albums, and followed artists only.

Evidence:
- `LibraryChanged` observer refreshes liked, albums, and followed artists: `clients/macos/Sources/SpotuifyKit/Stores/LibraryStore.swift:35`.
- The store has a separate saved-show loader: `clients/macos/Sources/SpotuifyKit/Stores/LibraryStore.swift:80`.
- Daemon emits `LibraryChanged` for save mutations: `crates/spotuify-daemon/src/handlers/library.rs:240`.
- Daemon emits `LibraryChanged` for unsave mutations: `crates/spotuify-daemon/src/handlers/library.rs:272`.

Impact: The Podcasts/Saved Shows view can stay stale after save/unsave mutations until a manual reload or reconnect, while other library panes refresh.

Recommended action: Include `loadShows(force: true)` in the `LibraryChanged` observer, or inspect event URIs and refresh only the affected collection.

Confidence: Medium.

Validation idea: Swift unit test emits a synthetic `.libraryChanged` and asserts `loadShows(force:)` is invoked or saved-shows state is refreshed.

## Verification

Commands run:

- `pwd`
- `git rev-parse --show-toplevel`
- `git branch --show-current`
- `git status --short`
- `sed -n '1,260p' AGENTS.md`
- `git ls-files`
- `sed -n '1,220p' Cargo.toml`
- `sed -n '1,180p' ARCHITECTURE.md`
- `sed -n '1,200p' README.md`
- `rg -n "todo!|unimplemented!|panic!|TODO|FIXME|placeholder|stub|no-op|noop|not implemented|not yet|Unsupported|early return|return Ok\\(\\(\\)\\)|Ok\\(\\(\\)\\)\\s*//" --glob '*.rs' --glob '*.swift'`
- `rg -n "DaemonRequest|RequestKind|enum .*Command|Subcommand|Commands|match .*Command|match request|kind\\(\\)" crates/spotuify-protocol/src crates/spotuify-cli/src crates/spotuify-daemon/src crates/spotuify-tui/src crates/spotuify-mcp/src src --glob '*.rs'`
- `rg -n "fatalError|TODO|FIXME|placeholder|Not implemented|return nil|return \\[\\]|return false|case .*: break|default: break" clients/macos/Sources clients/macos/Tests --glob '*.swift'`
- Targeted `sed`, `nl -ba`, and `rg` reads for protocol, daemon, CLI, TUI, MCP, and macOS files cited above.

Not run:

- Build/test suite. This audit only created a markdown report and did not modify code.

## Residual Risk

- This was a retrieval-led static audit, not exhaustive runtime verification.
- Absence findings are based on command enums and direct routing code, not generated CLI help.
- Some MCP omissions may be intentional safe-subset choices; findings only include cases where the advertised/live behavior conflicts with another project contract or description.
