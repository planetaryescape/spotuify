# Phase 8 - Wire Contract & Clients

## Goal

Teach the IPC contract and the three clients about providers — additively.
Requests gain an optional provider scope, capability data flows to clients so
they can gate surfaces, and the last provider-named wire types generalize.
After this phase the boundary test's documented rule ("clients = core +
protocol only") is finally enforced, not waived.

## Standalone value

Low-moderate standalone; this phase exists for the abstraction. The
independently useful pieces: capability-driven UI gating (TUI/MCP stop
offering actions the daemon will refuse), `ListAudioOutputs` moving over IPC
(closing the TUI's last daemon-crate import), and honest boundary
enforcement.

## Evidence base

- mxr's wire-evolution doctrine (verified): almost every request carries
  `account_id: Option<AccountId>` (`None` = default/all); additions use
  `#[serde(default, skip_serializing_if)]`; `IPC_PROTOCOL_VERSION` bumps only
  on true breaks; **no separate `provider` field — provider identity is
  subsumed into the account/scope identifier** (protocol/types.rs:424-434,
  lib.rs:18-35).
- `SearchSourceData { Local, Spotify, Hybrid }` (protocol/lib.rs:917-923) —
  the wire-level provider tag; CLI passes `::Spotify` at commands.rs:246,284,
  1570; MCP maps `"spotify"` at bridge.rs:467, server.rs:94.
- Spotify-shaped `IpcErrorKind` variants (protocol/lib.rs:1074-1076).
- MCP tool-description strings naming Spotify (tools.rs:50,62,105,111,172,
  221,234; resources.rs:30,103) — cosmetic.
- CLI's `normalize_spotify_target` calls (commands.rs:267,865,1301,1319,1513,
  1560) — provider-specific input parsing living client-side.
- TUI's remaining daemon import: `list_audio_outputs` (app.rs:6019 →
  server.rs:1072).
- Music Assistant (verified): capability set exposed to all clients so
  surfaces gate uniformly; search fan-out with per-provider soft/hard
  timeouts, provider errors never fail the aggregate.

## Design

1. **Scope field:** `provider: Option<ProviderId>` on requests where routing
   matters (search, library, playlists, sync triggers), serde-default so old
   clients keep working. `None` = default provider — today, always Spotify.
   Follow mxr: no version bump for additive fields.
2. **`SearchSourceData`** → `{ Local, Remote(ProviderId), Hybrid }` with
   custom serde keeping `"spotify"` as the legacy encoding for
   `Remote("spotify")` — wire-compatible both directions.
3. **Capabilities on the wire:** daemon status / a new `ProviderList`
   response carries each provider's `ProviderCaps`. Clients gate: TUI hides
   affordances (e.g. playlist-image editing when `playlists.image == false`),
   MCP omits tools per capability, CLI prints "provider X does not support Y"
   with a non-zero exit (documented exit code).
4. **Input normalization moves behind the daemon:** `ResolveTarget { input }`
   request — the daemon asks each registered provider's normalizer
   (Spotify's `normalize_spotify_target` becomes the first) to claim the
   input (OwnTone's tri-state "not mine — try next source" routing pattern).
   CLI/TUI pass raw user input through. This deletes the CLI's last `spotify`
   import.
5. **Error generalization:** provider-specific `IpcErrorKind` variants gain
   neutral shapes carrying `provider` + adapter detail strings; old variant
   names kept as serde aliases for one release.
6. **MCP/CLI strings:** description sweep, "Spotify" → provider-neutral
   phrasing where the tool is generic ("Spotify Connect devices" stays where
   it genuinely means Connect).
7. **`ListAudioOutputs` over IPC** — removes the TUI's `spotuify_daemon`
   import along with phase 7's auth move; **tighten `ALLOWED_DEPS` to the
   documented rule and delete the waiver comment.** This is the enforcement
   moment for the whole plan.

## Deliverables

Protocol additions + serde-compat tests (mxr-style round-trip tests: absent
field decodes as default, default omitted on encode); client ports; boundary
table tightened to `core, protocol` for tui/cli/mcp (+`cli` keeping `launcher`
for daemon lifecycle); capability-gating in all three clients; released-binary
compat drill.

## Verification

```bash
scripts/cargo-test --workspace && scripts/smoke.sh
# cross-version compat drill (the critical one):
spotuify status                                   # RELEASED client vs dev daemon
./target/release/spotuify status                  # dev client vs dev daemon
# capability gating:
./target/release/spotuify search "x" --provider spotify --format json
./target/release/spotuify search "x" --provider nope 2>&1; echo "exit=$?"   # clean error, documented code
# MCP: list tools, assert capability-gated set matches daemon caps
```

## Exit criteria

- `workspace_boundaries.rs` enforces clients = core+protocol (+launcher for
  lifecycle) with zero waivers — the documented rule finally true.
- Released client works against the new daemon (manual drill above).
- No functional `"spotify"` literal in cli/tui/mcp source (strings in
  Spotify-adapter-owned normalizer excluded).

## Dependencies

Phases 5 (caps exist), 7 (auth/config edges already removed).
