# Phase 13 - Spec Compliance and QoL Cleanup

## Goal

Close small gaps between code and blueprint specs, plus adopt small-but-impactful QoL patterns the competitors all agree on.

## Evidence base

| Pattern | Source | Why |
|---|---|---|
| `reload` command for hot-config reload | ncspot `commands.rs:213-235` | Re-reads config, rebuilds theme, rebinds keys; no restart |
| `reconnect` command | ncspot `application.rs:275-284` | Rebuilds session after network change |
| `-o KEY=VALUE` config override on CLI | spotify-player `config/mod.rs:526-553` | Dot-path override of TOML values without editing file |
| Auto-generated `.gitignore` in config dir | spotatui `core/config.rs:99-115` | Hedge against dotfile-sync token leaks |
| `cache_version` constant for schema invalidation | ncspot `config.rs:17`, `library.rs:97-103` | Cache breaks safe and explicit |
| Confirmation popups on destructive actions | spotify-player commit #966 | TUI parity with Phase 8 MCP confirmation pattern |
| Token-refresh `refresh_token` merge | spotatui PR #217 | Prevents re-auth on every restart |
| `User-Agent` header on all outbound HTTP | spotatui | Etiquette for LRCLIB and Spotify operability |
| Backtrace dump on panic | ncspot `panic.rs`, spotify-player `main.rs:84-93` | Stdout is owned by TUI; logs need to go to file |

## Deliverables

### CLI flags and commands
- `spotuify sync search-cache --prune [--older-than 30d]` (blueprint `04-sync-cache.md`).
- `--no-daemon-start` global CLI flag (Phase 2 spec).
- `spotuify bug-report [--include-logs N]` — bundle redacted system info + logs + doctor report (blueprint `10-observability.md`).
- `spotuify reload` — daemon re-reads `config.toml` and applies runtime-safe settings. Current hot-reload coverage applies visualization enable/source/FPS/smoothing/noise-gate without restart; backend swaps and TUI-only theme/keymap changes remain restart-scoped until those subsystems expose hot-swap seams.
- `spotuify reconnect` — daemon shuts down and re-registers the embedded player; useful after VPN/network change.
- `-o key.path=value` global CLI flag for one-shot TOML config override (e.g., `spotuify -o player.bitrate=160 play "jazz"`).
- `spotuify generate completions <shell>` (clap-built-in, just wire it).
- `spotuify generate man-page`.

### Config & UX patterns
- Auto-write `.gitignore` in `~/.config/spotuify/` listing `*.json`, `credentials.*`, `*.encrypted` on first config init.
- `cache_version` constant in `crates/spotuify-store`; daemon refuses to start with mismatch and suggests `spotuify cache reset --confirm`.
- `User-Agent: spotuify/<version> (https://github.com/planetaryescape/spotuify)` on outbound HTTP request clients (Spotify, OAuth token exchange, LRCLIB, image downloads).
- Backtrace dump on panic to `~/.cache/spotuify/backtrace/<ts>.log` with terminal-restoration cleanup; surface "panic occurred — see logs" message on next start.
- Confirmation modal infrastructure in TUI for destructive actions. Delete-playlist / unfollow / bulk-unsave actions are not exposed in the TUI today; CLI/MCP destructive surfaces use explicit dry-run/confirm flows.

### Observability
- `tracing-subscriber` JSON output mode via `--log-format json` or `SPOTUIFY_LOG_FORMAT=json`. Agent-consumable logs.
- `spotuify logs tail --follow --format json` streams structured events.
- Doctor reports: backend kind, audio backend, MPRIS bus name, image protocol, lyrics provider, MCP server state if running, cache version, last rate-limit event.

### HealthClass
- Promote `HealthClass` to three variants: `Healthy`, `Degraded`, `Unhealthy` (cannot reach Spotify, no auth, no daemon at all). Or document the two-variant choice in D013. Recommended: add the third variant.

### Decision-log backfill
- D010 — librespot embed (records Phase 9 outcome).
- D011 — MCP server (Phase 8 commitment).
- D012 — operation log (Phase 12 commitment).
- D013 — HealthClass cardinality.
- D014 — competitor study (this commit; record the source repos studied and date).

### README rewrite
- Match the shipped CLI surface; remove pre-daemon-era language.
- Add per-platform quickstart sections (filled by Phase 11).
- Add MCP-server setup snippet (Phase 8).
- Document embedded-only playback plus legacy `[spotifyd]` device-name migration (Phase 9).
- Add competitor comparison table.

### Phase 5 doc clarification
- Add a "Note on agent semantics" section to `06-phase-5-agent-playlists.md` clarifying that `build_playlist_plan` is a heuristic scaffold and that real LLM-driven planning lives in the upstream agent / MCP-client model.

## Work items

1. [x] `sync search-cache --prune [--older-than <window>]` is wired through the CLI and daemon `SearchCachePrune` request. Verified by `sync_search_cache_prune_accepts_older_than_and_json_output` and the sync help snapshot.
2. [x] `--no-daemon-start` is threaded through clap root and daemon-start helpers. Verified by `no_daemon_start_status_fails_without_spawning_daemon`.
3. [x] `bug-report` collects version/platform, doctor JSON when the daemon is reachable, last N log lines, last 50 operations when reachable, and redacted config; bundles as a local tar file and never auto-uploads. Verified by `bug_report_writes_requested_tar_and_redacts_config`.
4. [x] `spotuify reload` request is wired in the daemon and applies runtime visualization settings without restart. Verified by `reload_applies_viz_config_without_daemon_restart`. Backend swaps and TUI-only theme/keymap reload are intentionally not claimed until those subsystems expose hot-swap seams.
5. [x] `spotuify reconnect` request shuts down and re-registers the embedded player. Verified by `reconnect_re_registers_player_backend`.
6. [x] `-o key.path=value` global flag is wired through clap and merged through TOML `Value` without writing the config file. Verified by `config_load_applies_dotpath_override_without_writing_config_file`, dot-path override unit tests, and CLI help snapshots.
7. [x] Auto-write `.gitignore` in config dir on first init/load. Verified by `init_config_writes_gitignore_next_to_template`.
8. [x] `cache_version` constant + startup gate are implemented. Verified by `Store::check_cache_version`, daemon startup guard in `DaemonState::new`, and `test_check_cache_version_*` migration coverage.
9. [x] `User-Agent` headers are attached to Spotify Web API, OAuth token exchange, LRCLIB, and cover-art HTTP clients. The retired standalone premium gate no longer owns an HTTP client; provider-policy classification now lives in the paired adapter/player error path. The shared Spotify user-agent shape remains covered by `user_agent_string_carries_version_os_arch_and_github_url`.
10. [x] Panic hook wiring + backtrace log path + next-start warning are implemented in `src/logging.rs` and installed during CLI startup. Backtrace file writing is covered by `panic_backtrace_writer_records_payload_and_location`; hidden `--panic-test` remains intentionally unshipped.
11. [x] TUI confirmation modal shell is implemented and rendered, but no delete-playlist/unfollow/bulk-unsave TUI command is currently exposed. CLI/MCP destructive actions remain protected by dry-run/confirm contracts.
12. [x] `tracing-subscriber` JSON formatter behind `--log-format json` / `SPOTUIFY_LOG_FORMAT=json` is wired.
13. [x] `logs tail --follow --format json` is wired and covered by help snapshots.
14. [x] `HealthClass` enum has `Healthy`, `Degraded`, and `Unhealthy`; doctor election is implemented. Verified by diagnostics tests for degraded and unhealthy reports.
15. [x] Decision-log entries D010-D014 exist in `docs/blueprint/13-decision-log.md`.
16. [x] README rewrite matches the daemon/CLI/MCP/workspace-era surface, includes platform quickstarts, MCP setup, embedded playback build guidance, and a trade-off comparison table.
17. [x] Phase 5 doc clarification edit is present in `docs/implementation/06-phase-5-agent-playlists.md`, documenting `build_playlist_plan` as a deterministic heuristic scaffold rather than an LLM planner.

## Verification

- `spotuify --help` snapshot updates clean.
- `spotuify --no-daemon-start status` errors clearly when daemon is not running and does not create a daemon socket.
- `spotuify bug-report --output <path>` produces that tar path; test coverage checks config secrets and email addresses are redacted.
- `spotuify -o player.bitrate=96 play X` plays at 96kbps for that invocation only; config file unchanged.
- `spotuify reload` after editing `[viz]` settings updates the running daemon without losing playback.
- `spotuify reconnect` after toggling VPN re-registers the embedded player.
- `SPOTUIFY_LOG_FORMAT=json spotuify status` emits structured tracing output on stderr.
- `spotuify sync search-cache --prune --older-than 7d --format json` reports pruned counts.
- TUI confirmation modal blocks normal input; `n`/Esc cancel and `y` dispatches the deferred action. Specific delete-playlist/unfollow/bulk-unsave actions are not exposed yet.
- Panic backtrace writer records payload/location/version in the cache backtrace dir; terminal restoration and next-start warning remain manual smoke because they depend on the real TUI process lifecycle.
- Decision log matches actual decisions made in Phases 8, 9, 12, 13.

## Definition of done

Every CLI/event/setting promised in the blueprint or implementation plan is either implemented or has a referenced decision-log entry explaining the deliberate omission. The competitor-cribbed QoL patterns (`reload`, `reconnect`, `-o`, auto-gitignore, cache_version, confirmation modals, User-Agent, panic-to-file) are either shipped or documented with their current runtime boundary. Documentation reflects shipped reality. The blueprint stops drifting from the code.
