# Playback documentation needed separate completion and freshness contracts

The audit used `origin/main` `f039672fafc762791eef9077117b40dedf4f261c`. Runtime code is unchanged by these
repairs. The current architecture, examples and historical evidence have
different jobs; a historical observation is not a fresh acceptance check.

## Current source supports the architecture, with specific qualifications

| Claim | Assessment | Source and documentation correction |
| --- | --- | --- |
| The daemon owns reusable state for its clients | Retained | [Server](https://github.com/planetaryescape/spotuify/blob/f039672fafc762791eef9077117b40dedf4f261c/crates/spotuify-daemon/src/server.rs); [workspace boundaries](https://github.com/planetaryescape/spotuify/blob/f039672fafc762791eef9077117b40dedf4f261c/tests/workspace_boundaries.rs). |
| Store and search depend only on core | Qualified | The current boundary test permits protocol types, and search also depends on store. The stricter design guidance was not silently changed. |
| A context should publish current and upcoming tracks | Retained and extended | [Queue handler](https://github.com/planetaryescape/spotuify/blob/f039672fafc762791eef9077117b40dedf4f261c/crates/spotuify-daemon/src/handlers/playback.rs#L825). Visible snapshots and native continuation are separate requirements. |
| Idle playlists start one track and append the rest | Repaired | Idle collections load the resolved ordered list; individual selections use first-item start plus appends. A paused active session can remain paused. |
| Queue add accepts a playlist display name | Repaired | An isolated CLI probe accepted the name for playlist browsing but rejected it for queue add. The queue guide now uses a playlist URI. |
| `queue add --wait` establishes audible start | Rejected | [CLI wait](https://github.com/planetaryescape/spotuify/blob/f039672fafc762791eef9077117b40dedf4f261c/crates/spotuify-cli/src/commands.rs#L2064) waits for finalization; [Spirc load](https://github.com/planetaryescape/spotuify/blob/f039672fafc762791eef9077117b40dedf4f261c/crates/spotuify-player/src/backends/embedded/mod.rs#L924) can still be queued. |
| One command revision prevents stale playback reads | Extended | [Poll commit](https://github.com/planetaryescape/spotuify/blob/f039672fafc762791eef9077117b40dedf4f261c/crates/spotuify-daemon/src/state.rs#L6054) also checks native transitions and provider ownership. |
| Guarding the rendered value covers the cache | Rejected | [Native cache worker](https://github.com/planetaryescape/spotuify/blob/f039672fafc762791eef9077117b40dedf4f261c/crates/spotuify-daemon/src/native_playback_cache.rs) persists current state after waiting. Cache I/O and cancellation still have limits. |
| Timestamps alone identify the latest persisted snapshot | Repaired | [Latest snapshot query](https://github.com/planetaryescape/spotuify/blob/f039672fafc762791eef9077117b40dedf4f261c/crates/spotuify-store/src/lib.rs#L769) breaks ties with insertion ID. This orders local inserts, not remote causality. |
| Playback JSON has a `playing` field | Repaired | The public playback field is `is_playing`; checked through the installed CLI with an isolated fake provider. |
| Number keys 5–8 open Queue, Devices, Diagnostics and Lyrics | Repaired | [Executed key dispatcher](https://github.com/planetaryescape/spotuify/blob/f039672fafc762791eef9077117b40dedf4f261c/crates/spotuify-tui/src/app.rs#L6307) opens Podcasts, History, Notifications and Bookmarks. Queue uses Alt-q, devices D, and the other views are palette actions. |
| `V` cycles the visualizer source | Repaired | [Action and handler](https://github.com/planetaryescape/spotuify/blob/f039672fafc762791eef9077117b40dedf4f261c/crates/spotuify-tui/src/app.rs#L6668) toggle the fullscreen visualizer. Audio output O is handled before the generic key map. |
| Auth files are unscoped and may fall back to Keychain | Repaired | [Credential store](https://github.com/planetaryescape/spotuify/blob/f039672fafc762791eef9077117b40dedf4f261c/crates/spotuify-spotify/src/auth.rs#L1173) uses `auth/spotify/`; unscoped files are migration inputs. The old mirror/keychain lesson is historical. |
| `instant` remains an accepted advisory exception | Superseded | [Policy](https://github.com/planetaryescape/spotuify/blob/f039672fafc762791eef9077117b40dedf4f261c/deny.toml) lists RSA and two quick-xml identifiers; [lockfile](https://github.com/planetaryescape/spotuify/blob/f039672fafc762791eef9077117b40dedf4f261c/Cargo.lock) has Tantivy 0.27 and no instant. This compares source policy and resolution, not a new advisory scan. |
| The agent skill can pipe independent transport actions together | Repaired | Alternatives now use separate command lines and an instruction to choose the requested action. Streaming and Web API playback control both require Premium. |
| The latest CLI release also provides the current DMG download | Repaired | Release 0.1.112 has no DMG; the asset inventory found app assets on 0.1.109. The install guide selects an app-bearing release and verifies its digest-only checksum. [App update availability](https://github.com/planetaryescape/spotuify/blob/f039672fafc762791eef9077117b40dedf4f261c/clients/macos/Sources/SpotuifyKit/Stores/AppModel.swift#L766) already checks DMG reachability before offering an update. |
| Every 403 is repaired by obtaining Extended Quota Mode | Rejected | [Error classification](https://github.com/planetaryescape/spotuify/blob/f039672fafc762791eef9077117b40dedf4f261c/crates/spotuify-spotify/src/error.rs#L182) retains several causes. Spotify's [quota rules](https://developer.spotify.com/documentation/web-api/concepts/quota-modes) distinguish app access and extension eligibility. |
| Local runner isolation also bounds machine contention | Qualified | [Isolated homes](https://github.com/planetaryescape/spotuify/blob/f039672fafc762791eef9077117b40dedf4f261c/.github/actions/self-hosted-rust-home/action.yml) protect toolchain state. The separate `scripts/ci-cargo` queue bounds heavy commands; signing and provenance still need their own evidence. |
| The launcher waits for usable IPC rather than a PID alone | Retained | [Launcher](https://github.com/planetaryescape/spotuify/blob/f039672fafc762791eef9077117b40dedf4f261c/crates/spotuify-launcher/src/lib.rs#L24) has a 60-second startup bound. Player initialization precedes listener bind; failure can continue with a degraded player. |

Inspect the relevant command contracts before repeating a workflow:

```sh
spotuify queue add --help
spotuify events --help
spotuify daemon status --format json
```

## Verify examples without changing live playback

The documentation probe used the installed 0.1.112 CLI with an isolated fake
provider, instance, socket, database and config. It checked playlist selection,
queue finalization, playback JSON keys, queue output, daemon status and visualizer
output. This validates the documented command and output surfaces, not acoustic
playback. The existing [installed playback evidence](../build-log/recovery/evidence/playback-status-installed112.json)
records two natural transitions after seeking near the endings and 60 seconds
on the third track, with 96 status samples and no reversions.

Generate reference through its source snapshots and build the site:

```sh
npm --prefix site ci --ignore-scripts
npm --prefix site run build
```

The docs validator checks its declared patterns and 142 required CLI pages.
Its success alone does not establish that every handwritten example or link is
correct. Changed examples and links need their own checks.

## Documentation checks passed without rebuilding the application

The site built 179 pages and its validator checked 142 required CLI pages. An
additional check resolved 1,849 internal link and anchor occurrences on the
changed docs pages. The 16 touched Obsidian notes passed frontmatter, wikilink and
pinned source-path checks; the three new concepts have reciprocal defining links.
The local skill and downloadable bundle match the source.

The contributor examples for CI argument forwarding and signing cleanup passed
their shell checks. The versioned 0.1.109 DMG downloaded and passed its published
digest (`219bf93ee5aa191c87bb992f53f04524449b676a4ea9f2e096d8430f7511dbe8`);
it was neither mounted nor installed. Signing uses stubs here; this did not sign or notarize an
artifact. Whitespace and blueprint checks passed. No Rust build or live playback
mutation was needed for these documentation changes.

## Preserve the boundaries that remain open

The playback timestamp comparison needs available, comparable timestamps; ties
and clock skew limit it. A cancelled cache worker or failed write can leave the
cache behind the live clock. Coalescing state notifications is not an audit log.

External-device handoff, human visual acceptance, the foreign-cluster recovery
race and reconnect during pending initial activation remain outside the passing
playlist check. [Recovery handover](../implementation/2026-10-26-recovery-handover.md)
owns their next actions.

Queue add has no dry-run flag, and its help describes provider confirmation
broadly. The guide now explains the native-load completion boundary. Changing
help text or runtime semantics requires a separate code change.

Use the corrected operational guides:

- [Queue and playlists](../../site/src/content/docs/guides/queue-and-playlists.md).
- [Playback and daemon](../../site/src/content/docs/guides/player-and-daemon.md).
- [JSON output](../../site/src/content/docs/reference/json-output.md).
- [Troubleshooting](../../site/src/content/docs/reference/troubleshooting.md).
