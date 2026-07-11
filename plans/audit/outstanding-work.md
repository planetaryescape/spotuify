# Outstanding Work Audit

Scope: TODO/FIXME/HACK/XXX comments, incomplete/stubbed functions, and planned work in `docs/` that appears unimplemented.

Worktree: `/Users/bhekanik/.dotfiles/.codex/manual-worktrees/spotuify-audit/outstanding-work`
Branch: `codex/audit-outstanding-work-20260619`
Base HEAD: `124b911`
Tracked files scanned: 699

## Summary

- Code TODO/FIXME/HACK/XXX markers in `crates/**` + `clients/macos/**`: 0.
- Production `todo!()` / `unimplemented!()` hits: 0.
- Docs planned/stub markers reviewed: 101.
- Findings: 8.
- Highest-risk themes: analytics progress samples, scrobbling recipes, MCP lyrics subscriptions, release/distribution follow-ups.

## Findings

### P1 - `playback_progress` is schema/prune-only; production never inserts samples

Issue: The `playback_progress` table exists and retention prunes it, but no production writer inserts rows.

Evidence:
- `docs/implementation/13-phase-10-analytics-derivations.md:128` says migration and prune tests exist, but production insert coverage is absent.
- `docs/implementation/13-phase-10-analytics-derivations.md:132` lists production `playback_progress` inserts as a remaining fidelity gap.
- `crates/spotuify-store/src/lib.rs:2794` creates `playback_progress`.
- `crates/spotuify-store/src/listen_facts.rs:440` only prunes `playback_progress`.
- `rg 'INSERT INTO playback_progress|insert.*playback_progress|playback_progress.*INSERT|record_progress' crates --glob '!**/tests/**'` returned 0 production insert hits.

Impact: Raw progress retention and any future progress replay/analytics are dead surface area. `analytics prune` can report/prune a table normal usage never fills.

Recommended action: Add a bounded writer on the daemon playback sampling path, or remove/defer the table and prune surface until a writer exists. Keep it behind the same retention config.

Confidence: High.

Validation idea: Start daemon with fake provider, simulate playback/progress, then assert `SELECT COUNT(*) FROM playback_progress` increases; add a daemon/store integration test around the same path.

### P1 - Scrobbling recipes are incomplete and one stub prints credential-bearing fields

Issue: Last.fm scrobbling is explicitly a sketch/stub and does not POST. The stub output includes credential-bearing field names/values, which is unsafe if a user wires it into daemon hook logs.

Evidence:
- `docs/recipes/scrobble-lastfm.sh:7` says to fill in signing logic before relying on it.
- `docs/recipes/scrobble-lastfm.sh:30` says it demonstrates request shape only.
- `docs/recipes/scrobble-lastfm.sh:43` through `docs/recipes/scrobble-lastfm.sh:55` echo the would-be request payload instead of sending it.
- `docs/recipes/README.md:35` calls the Last.fm recipe a sketch.

Impact: Users can configure a hook that appears installed but never scrobbles. If stderr is collected, the stub can expose credential-bearing values in local logs.

Recommended action: Either implement the signed Last.fm POST with a dry-run mode that redacts credential-bearing fields, or move the stub to a non-executable `.example` with a clear "not runnable" name.

Confidence: High.

Validation idea: Run `spotuify hooks test` with fake Last.fm env vars and assert no credential-bearing fields are printed; then run against a mock HTTP server and assert a signed POST is sent.

### P2 - Scrobble enrichment docs reference a missing `analytics show` command

Issue: The ListenBrainz recipe tells users to run `spotuify analytics show <uri>`, but the CLI has no `AnalyticsCommand::Show`.

Evidence:
- `docs/recipes/scrobble-listenbrainz.sh:31` through `docs/recipes/scrobble-listenbrainz.sh:33` suggests `spotuify analytics show <uri>`.
- `crates/spotuify-cli/src/cli_args.rs:515` through `crates/spotuify-cli/src/cli_args.rs:568` defines analytics commands as `rebuild`, `top`, `habits`, `search`, `rediscovery`, and `prune`; no `show`.
- `docs/recipes/scrobble-listenbrainz.sh:53` through `docs/recipes/scrobble-listenbrainz.sh:55` fall back to track/artist/album IDs as display names.

Impact: Hook users cannot follow the documented enrichment path, and ListenBrainz/Discord recipe output records IDs where human-readable names are expected.

Recommended action: Add a real enrichment command, or update recipes to call an existing CLI/API surface. Prefer adding the missing CLI only if scrobbling is a validated workflow; otherwise make the recipes honest minimal examples.

Confidence: High.

Validation idea: Run the documented recipe path from a fake `listen_qualified` env and assert the command exists and returns track/artist/album names in JSON.

### P2 - MCP lyrics resource is subscribable but never invalidated

Issue: `spotuify://now_playing/lyrics` is advertised as subscribable, but resource invalidation maps only playback/devices/playlists/library.

Evidence:
- `crates/spotuify-mcp/src/resources.rs:41` through `crates/spotuify-mcp/src/resources.rs:47` marks `spotuify://now_playing/lyrics` as `subscribable: true`.
- `docs/implementation/11-phase-8-mcp-server.md:67` through `docs/implementation/11-phase-8-mcp-server.md:70` describes now-playing lyrics as a live resource.
- `crates/spotuify-mcp/src/resources.rs:71` through `crates/spotuify-mcp/src/resources.rs:78` maps invalidations for playback/devices/playlists/library only.
- `crates/spotuify-mcp/tests/resources.rs:96` through `crates/spotuify-mcp/tests/resources.rs:99` explicitly skips the lyrics resource mapping.

Impact: MCP clients that subscribe to lyrics may never receive `notifications/resources/updated`, so lyrics can stay stale across track changes.

Recommended action: Either map `PlaybackChanged` to both `spotuify://playback` and `spotuify://now_playing/lyrics`, or make lyrics non-subscribable until a lyrics-specific event exists.

Confidence: High.

Validation idea: Subscribe to `spotuify://now_playing/lyrics` over stdio, emit/trigger a playback change, and assert a resource-updated notification is sent for the lyrics URI.

### P2 - `--no-media-controls` remains documented, but only an env var exists

Issue: Phase 14 planned a daemon `--no-media-controls` flag. Current wiring uses `SPOTUIFY_NO_MEDIA_CONTROLS=1`; no CLI flag surfaced in `spotuify-cli`.

Evidence:
- `docs/implementation/17-phase-14-system-integration.md:107` through `docs/implementation/17-phase-14-system-integration.md:110` says to provide `--no-media-controls`.
- `docs/blueprint/13-decision-log.md:536` through `docs/blueprint/13-decision-log.md:543` records the opt-out gap and later env-var behavior.
- `crates/spotuify-daemon/src/state.rs:2220` through `crates/spotuify-daemon/src/state.rs:2229` reads `SPOTUIFY_NO_MEDIA_CONTROLS`.
- `rg 'no-media-controls|NoMedia' crates/spotuify-cli crates/spotuify-daemon` finds docs/env wiring but no CLI flag.

Impact: Users can opt out only if they know the env var. The documented daemon flag is not available for service files, troubleshooting, or scripts.

Recommended action: Either add the daemon flag and pass it into config, or update docs to make the env var the settled interface.

Confidence: Medium-high.

Validation idea: `spotuify daemon start --no-media-controls` should parse and yield disabled media-control diagnostics, or docs/help should stop mentioning the flag.

### P2 - Cross-platform release channels remain explicitly unshipped

Issue: Docs still track Linux musl, Linux arm64, AUR, Scoop, and `.deb` as unshipped release/distribution work.

Evidence:
- `docs/implementation/14-phase-11-cross-platform.md:105` through `docs/implementation/14-phase-11-cross-platform.md:113` lists Linux musl/arm, AUR, Scoop, and `cargo-deb` gaps.
- `docs/implementation/14-phase-11-cross-platform.md:163` through `docs/implementation/14-phase-11-cross-platform.md:169` leaves Linux musl/arm, signing, AUR, Scoop, and `cargo-deb` as follow-ups.
- `docs/implementation/14-phase-11-cross-platform.md:191` through `docs/implementation/14-phase-11-cross-platform.md:203` excludes those channels from release gates.
- `.github/workflows/release.yml:53` shows the currently published release matrix includes macOS arm64, macOS Intel, Linux x86_64 GNU, and Windows x64, not those follow-up targets.

Impact: Users outside the shipped target/channel set must build from source or use Nix. This is not a runtime bug, but it is real planned release work.

Recommended action: Keep as release-ops backlog unless demand validates a channel. Prioritize `.deb`/Linux arm only if install friction is showing up in user reports.

Confidence: High.

Validation idea: Add matrix jobs/artifact assertions for each chosen channel; for external channels, add smoke install in an isolated container/VM.

### P3 - Playlist planner is intentionally a scaffold/stub, not a real planner

Issue: `build_playlist_plan` is shipped as a deterministic scaffold and docs explicitly say to treat it as a stub.

Evidence:
- `docs/implementation/06-phase-5-agent-playlists.md:15` through `docs/implementation/06-phase-5-agent-playlists.md:18` says the local heuristic is a stub and should not compete with MCP-client planning.
- `crates/spotuify-protocol/src/agent_playlists.rs:94` through `crates/spotuify-protocol/src/agent_playlists.rs:128` builds a plan from simple brief-derived search strings with fixed target length and generic sequencing notes.
- `crates/spotuify-cli/src/cli_args.rs:284` through `crates/spotuify-cli/src/cli_args.rs:292` exposes this as `playlist plan`.

Impact: The command is useful for shell composition/tests, but users may overread it as an intelligent playlist planner.

Recommended action: Do not expand this unless there is validated demand. Consider naming/copy that makes "scaffold" more prominent in CLI help and docs.

Confidence: High.

Validation idea: CLI help and README examples should say scaffold/heuristic; MCP workflow docs should remain the primary agent-planning path.

### P3 - TUI cover-art terminal protocol reporting is still a diagnostics follow-up

Issue: Cover art works through `ratatui-image`, but docs still mark terminal protocol reporting in diagnostics as TUI-side follow-up.

Evidence:
- `docs/implementation/18-phase-15-cover-art.md:97` through `docs/implementation/18-phase-15-cover-art.md:98` says terminal protocol detection in doctor remains a follow-up.
- `docs/implementation/18-phase-15-cover-art.md:131` through `docs/implementation/18-phase-15-cover-art.md:132` repeats that terminal protocol reporting is still a TUI diagnostics follow-up.
- `crates/spotuify-tui/src/app.rs:768` constructs the picker from stdio query with halfblock fallback, but no searched code path reports that chosen protocol through daemon doctor/system diagnostics.

Impact: Users can see cover rendering fail/degrade, but diagnostics cannot tell them which terminal image protocol was selected or why fallback occurred.

Recommended action: Add TUI-local diagnostics/status text for the selected `ratatui-image` protocol; keep daemon doctor out of terminal-probing.

Confidence: Medium.

Validation idea: Run TUI in kitty/iTerm/TERM=screen contexts and verify diagnostics/help panel reports the selected image protocol or fallback.

## False Positives Excluded

- Analytics sink-tap audible time: docs still say `SessionTracker` does not consume `AudioCounterHandle`, but code now wires `player_box.audio_counter()` into `SessionTracker::with_store` and prefers counter deltas in `finalize` (`crates/spotuify-daemon/src/state.rs:598`, `crates/spotuify-daemon/src/state.rs:613`, `crates/spotuify-daemon/src/session_tracker.rs:346`).
- MCP radio/related-artists: docs contain old deferral wording, but typed requests, daemon handlers, Mercury parsers, MCP tools, and tests now exist (`crates/spotuify-mcp/src/bridge.rs:282`, `crates/spotuify-daemon/src/handlers/library.rs:109`, `crates/spotuify-spotify/src/mercury.rs:51`).
- Notifications and Discord rich presence: Phase 14 docs still call parts scaffold/follow-up, but code has snapshot-based notification token expansion and a live Discord handle (`crates/spotuify-system/src/notifications.rs:159`, `crates/spotuify-system/src/discord.rs:96`).
- Librespot reconnect TODO: the TODO is upstream; spotuify pins a fork with the upstream recovery PR (`docs/maintenance/librespot-fork.md:10`, `docs/maintenance/librespot-fork.md:15`).

## Commands Run

```text
pwd
git status --short
sed -n '1,520p' AGENTS.md
git rev-parse --show-toplevel
git branch --show-current
git rev-parse --short HEAD
git ls-files
find . -maxdepth 3 -type f \( -name 'README*' -o -name 'ARCHITECTURE.md' -o -name 'Cargo.toml' -o -name 'CONTRIBUTING.md' \)
rg -n --hidden --glob '!target*/**' --glob '!Cargo.lock' --glob '!plans/audit/**' '\b(TODO|FIXME|HACK|XXX|todo!\(|unimplemented!\(|panic!\("TODO|panic!\("not implemented|stub|placeholder|coming soon|not yet implemented)\b'
rg -n --glob 'docs/**' --glob '!plans/audit/**' '\b(TODO|FIXME|HACK|XXX|future|planned|not yet|will|phase|Phase|backlog|stub|placeholder|later|roadmap|missing|unimplemented)\b'
rg -n --glob '*.rs' '\b(todo!|unimplemented!|unreachable!|panic!\(|Default::default\(\)|todo\b|TODO|FIXME|HACK|XXX|stub|placeholder|not implemented)\b' crates clients/macos
rg -n 'radio_start|related_artists|Related|Radio|Lyrics|Analytics|OpsUndo|LibraryUnsave|PlaylistRemove|playlist_set_image' crates/spotuify-protocol crates/spotuify-daemon crates/spotuify-cli crates/spotuify-mcp
rg -n 'AudioCounterHandle|audible_ms\(|playback_progress|insert.*progress|Progress|SessionTracker|finalize' crates/spotuify-daemon crates/spotuify-player crates/spotuify-store crates/spotuify-protocol
rg -n '^\\s*[-*]?\\s*\\[[ ~]\\]|\\[~\\]|remain .*follow|remains .*follow|pending|manual|not shipped|not yet|stub|sketch|future work|awaits|TODO|FIXME|HACK|XXX' docs --glob '!plans/audit/**'
rg -n 'INSERT INTO playback_progress|insert.*playback_progress|playback_progress.*INSERT|record_progress' crates --glob '!**/tests/**'
rg -n 'analytics show|AnalyticsCommand::|show <uri>|analytics_show' crates/spotuify-cli docs/recipes docs/implementation/13-phase-10-analytics-derivations.md
rg -n 'no-media-controls|SPOTUIFY_NO_MEDIA_CONTROLS|media_controls_off|NoMedia|daemon.*media' crates/spotuify-cli crates/spotuify-daemon crates/spotuify-spotify README.md docs/implementation/17-phase-14-system-integration.md docs/blueprint/13-decision-log.md
rg -n 'AUR|Scoop|cargo-deb|linux arm|musl|Apple Developer|notarize|signing secrets|macOS signing' .github/workflows docs README.md clients/macos --glob '!docs/research/**'
nl -ba <targeted files listed in findings>
```
