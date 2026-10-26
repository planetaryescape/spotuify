# First-load playlist errors can look like empty results

Baseline: `origin/main` at `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e`, `crates/spotuify-daemon/src/handlers/playlists.rs:1740-1751,1804-1807`. Source inspection 2026-10-26; the first-load rendering consequence still needs a deterministic CLI/client reproduction.

The background metadata path drops resolution errors; the items path logs a failure at debug level. A nonblocking request can return an empty successful list without an outcome event that distinguishes failure from an empty collection.

Next action: inject a delayed failing fake playlist read, request the collection without waiting, subscribe to events and capture whether loading settles. Define loading, available-empty, available-content and failed states consistently across CLI/TUI/MCP/macOS. Preserve cached content as stale when refresh fails rather than replacing it with empty success.

Done when every started refresh yields a terminal outcome, all clients expose the failure with the selected playlist retained, and the CLI event path proves the same state. Update the protocol version/compatibility policy if a new event is required. This is a complete cross-client slice, not a toast-only patch.
