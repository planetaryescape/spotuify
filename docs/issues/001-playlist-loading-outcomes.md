# First-load playlist errors can look like empty results

Baseline: `origin/main` at `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e`, `crates/spotuify-daemon/src/handlers/playlists.rs:1740-1751,1804-1807`. Source inspection 2026-10-26; the first-load rendering consequence still needs a deterministic CLI/client reproduction.

The background metadata path drops resolution errors; the items path logs a failure at debug level. A nonblocking request can return an empty successful list without an outcome event that distinguishes failure from an empty collection.

Next action: inject a delayed failing fake playlist read, request the collection without waiting, subscribe to events and capture whether loading settles. Define loading, available-empty, available-content and failed states consistently across CLI/TUI/MCP/macOS. Preserve cached content as stale when refresh fails rather than replacing it with empty success.

Done when every started refresh yields a terminal outcome, all clients expose the failure with the selected playlist retained, and the CLI event path proves the same state. Update the protocol version/compatibility policy if a new event is required. This is a complete cross-client slice, not a toast-only patch.


At recovery candidate `d9158ba1f387d34d309cf6f1bb2c1554b4658a19`, nonblocking `PlaylistTracks` returns empty `MediaItems` before the refresh finishes (`handlers/playlists.rs:93-119`). The background path returns on rate limiting or resolution failure and only debug-logs collection errors (`1729-1792`). TUI handles refreshed/inaccessible markers but has no ordinary-error terminal event. CLI and MCP use `wait=true`; macOS also waits but suppresses request errors with `try?` in `clients/macos/Sources/SpotuifyKit/Stores/LibraryStore.swift:157-164`.

The existing typed `SearchFailed` event is a useful precedent to inspect, not a chosen playlist schema. Reproduce delayed success, available-empty and provider failure through the same subscription before designing R2. The current hostile test provider has delays/unavailable results but no injected ordinary provider error; add only the control the reproduction needs.
