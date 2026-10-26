# First-load playlist errors can look like empty results

Baseline: `origin/main` at `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e`, `crates/spotuify-daemon/src/handlers/playlists.rs:1740-1751,1804-1807`. Source inspection 2026-10-26; the first-load rendering consequence still needs a deterministic CLI/client reproduction.

The background metadata path drops resolution errors; the items path logs a failure at debug level. A nonblocking request can return an empty successful list without an outcome event that distinguishes failure from an empty collection.

Next action: inject a delayed failing fake playlist read, request the collection without waiting, subscribe to events and capture whether loading settles. Define loading, available-empty, available-content and failed states consistently across CLI/TUI/MCP/macOS. Preserve cached content as stale when refresh fails rather than replacing it with empty success.

Done when every started refresh yields a terminal outcome, all clients expose the failure with the selected playlist retained, and the CLI event path proves the same state. Update the protocol version/compatibility policy if a new event is required. This is a complete cross-client slice, not a toast-only patch.


At recovery candidate `d9158ba1f387d34d309cf6f1bb2c1554b4658a19`, nonblocking `PlaylistTracks` returns empty `MediaItems` before the refresh finishes (`handlers/playlists.rs:93-119`). The background path returns on rate limiting or resolution failure and only debug-logs collection errors (`1729-1792`). TUI handles refreshed/inaccessible markers but has no ordinary-error terminal event. CLI and MCP use `wait=true`; macOS also waits but suppresses request errors with `try?` in `clients/macos/Sources/SpotuifyKit/Stores/LibraryStore.swift:157-164`.

The existing typed `SearchFailed` event is a useful precedent to inspect, not a chosen playlist schema. Reproduce delayed success, available-empty and provider failure through the same subscription before designing R2. The current hostile test provider has delays/unavailable results but no injected ordinary provider error; add only the control the reproduction needs.

## Queue expansion still hides classified access reasons

The [Spotify adapter](https://github.com/planetaryescape/spotuify/blob/6c90d79c0d9c9c6a2596d62f193595c5a8c49b73/crates/spotuify-spotify/src/provider.rs#L587-L610) classifies some playlist HTTP 403 responses as private, region restricted, subscription required, or temporarily unavailable. [Queue expansion for playlist URIs](https://github.com/planetaryescape/spotuify/blob/6c90d79c0d9c9c6a2596d62f193595c5a8c49b73/crates/spotuify-daemon/src/handler.rs#L3620-L3627) discards that reason and returns the generic `playlist is not accessible` error. This branch is unchanged between PR 154 commit `2b8d4801f18daef628ee3dd4ef476f5746d8051e` and release commit `6c90d79c0d9c9c6a2596d62f193595c5a8c49b73`. Track it as deferred queue error fidelity alongside R2; it is separate from background `PlaylistTracks` terminal outcomes and did not block R1.
