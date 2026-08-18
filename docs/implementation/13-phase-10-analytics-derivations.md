# Phase 10 - Analytics Derivations

## Goal

Turn the raw `analytics_events` log into first-class derived listening analytics per `blueprint/16-analytics.md`. The current implementation has the event store, `listen_facts`, track rollups, top/habits/search/rediscovery/prune commands, Last.fm historical import, and MCP analytics tools. Remaining work is concentrated in richer artist/album/habit rollups, additional import/export providers, and optional live external scrobble polish.

## Evidence base

- **None** of ncspot, spotify-player, or spotatui ship any local analytics. Their playback observability is "look at Spotify's Wrapped once a year." This is a real spotuify differentiator.
- ncspot's queue snapshot persistence (`config.rs:138-144`, `application.rs:144-163`) is the closest analog — they save queue state across restarts. We extend that to a full event log + derived metrics.
- spotify-player's "shell hook" pattern (`player_event_hook_command`) is the right mechanism to bridge our live event stream to external scrobblers (Last.fm, ListenBrainz, Maloja, custom user scripts).

## Deliverables

### Session tracker
- `SessionTracker` actor inside the daemon (or `spotuify-sync`).
- Subscribes to `PlayerEvent::{Playing, Paused, Stopped, EndOfTrack, TrackChanged, Seeked, SessionDisconnected}` from Phase 9.
- Maintains state machine: `Idle → Playing → Paused → Playing → ... → Stopped`.
- Emits domain events as `analytics_events` rows: `playback_started`, `playback_paused`, `playback_resumed`, `playback_skipped`, `playback_completed`.
- Handles `SessionDisconnected` mid-track as "session_died" (don't count as skip; don't count as completion).
- Computes `audible_ms` from the embedded PCM sample counter when available, with elapsed-minus-paused wall-clock time as the fallback.

### `listen_qualified` rule
Per blueprint §"Listen qualification":
- `qualified = audible_ms >= max(30_000, min(0.5 * duration_ms, 240_000))`.
- Persist `qualification_rule_version` per row so future tweaks don't retroactively change history.
- Emit `listen_qualified` event when threshold crosses; otherwise mark `playback_completed` event with `qualified: false`.

### Derived tables
```text
listen_facts
- id
- track_uri
- session_id
- started_at_ms
- ended_at_ms
- elapsed_ms
- audible_ms              -- sink-tap sample count, with wall-clock fallback
- completion_ratio        -- audible_ms / duration_ms
- qualified
- qualification_rule_version
- skip_reason             -- user_next | user_previous | track_end | error | session_died
- source                  -- search | playlist | album | queue | library | agent | radio
- backend                 -- embedded

track_metrics            -- materialized view
artist_metrics, album_metrics   -- analogous

habit_metrics
- bucket                 -- day | week | month
- bucket_start_ms
- listening_minutes
- unique_tracks
- unique_artists
- sessions
- top_hour_of_day
- exploration_ratio      -- new-to-user tracks / total
- repeat_ratio
```

### Sink-tap for accurate audible_ms
- Phase 9's sink-factory chain includes an `AudioCounterTap` sink that counts PCM samples written.
- More accurate than wall-clock timing because it excludes buffer drops, output-disconnect gaps, and time spent paused.
- `audible_ms = (samples_written / sample_rate) * 1000`.
- Current state: `SessionTracker` samples the counter at session start and finalization, writes the delta into `listen_facts`, and falls back to wall-clock time when no embedded counter is available. It also stores production `playback_progress` samples.

### CLI commands
- `spotuify analytics rebuild [--since ISO]` — recompute derivations from raw events.
- `spotuify analytics top --kind tracks|artists|albums|playlists --since 7d|30d|90d|365d|all [--limit] [--format]`
- `spotuify analytics habits --window day|week|month [--since] [--format]`
- `spotuify analytics search [--raw|--normalized] [--limit] [--format]`
- `spotuify analytics rediscovery --gap 30d|90d|365d [--format]`
- Provider export remains a reserved command that returns a follow-up error. Live scrobbling is the shell-hook bridge below; see `docs/recipes/`.
- `spotuify analytics import lastfm --user USER [--from DATE] [--to DATE] [--apply] [--format json]`.
- `spotuify analytics import status RUN_ID`.
- `spotuify analytics import unresolved RUN_ID`.
- `spotuify analytics import undo RUN_ID --dry-run|--yes`.
- `spotuify analytics import --target lastfm` remains a dry-run compatibility alias.
- `spotuify analytics export --target listenbrainz|lastfm --since DATE` exists as a command surface but still returns the documented follow-up error.

### Last.fm historical import
- Fetches Last.fm `user.getRecentTracks` with `limit=200`, `extended=1`, optional `from` / `to` bounds, and bounded retry/backoff.
- Skips `nowplaying=true` rows.
- Stores raw rows in `external_scrobbles` and run state in `analytics_import_runs`.
- Resolves through local exact match, local search, then Spotify search.
- Promotes only high-confidence matches into `listen_facts`.
- Marks imported listen facts with `measurement_kind = "lastfm_scrobble_import"` and `external_scrobble_id`.
- Treats imported listens as qualified but uses estimated audible time because Last.fm does not include stop/progress history.
- Leaves ambiguous/unresolved rows stored for review without affecting analytics.
- Undo removes promoted listen facts and rebuilds rollups while preserving raw scrobble audit rows.

### Shell-hook bridge to external scrobblers
- Phase 14's `spotuify_hook listen-qualified <uri> <duration_ms>` event is the bridge.
- Sample hook scripts in `docs/recipes/`:
  - `recipes/scrobble-listenbrainz.sh`
  - `recipes/scrobble-lastfm.sh`
  - `recipes/notify-discord-listening.sh`
- Spotuify does not ship live provider export in-tree today. External hooks are the shipped path for live scrobbling.
- Live scrobbling stays outside the daemon through hooks. Historical Last.fm import is in-tree because it is read-only backfill, not live `track.scrobble` credential handling.

### Privacy
- `[analytics] store_raw_queries = true` (default true; user-configurable).
- Provider telemetry redacts `q`, `ids`, `uri`, `market` query params before persistence.
- Private/incognito Spotify session: detect via `me().product == "open"` heuristic + `is_private_session` if exposed; suppress `listen_qualified` and write `listen_facts` with `private_session: true`.

### Retention
- Raw `playback_progress` samples: 90 days
- Action / search / playback events: 1 year
- Derived listen facts and aggregates: forever until user deletes
- `spotuify analytics prune [--apply]` enforces retention; daily background job runs prune.

### MCP integration
- `analytics_top`, `analytics_habits`, `analytics_search`, `analytics_rediscovery` exposed as MCP tools (Phase 8).
- Agents can answer "what's my most-played artist this month?" using local data, no API call.

## Work items

1. [x] Add migrations for `listen_facts`, `track_metrics`, `artist_metrics`, `album_metrics`, `habit_metrics`, `qualification_rules`.
2. [x] Build `SessionTracker` in the daemon subscribing to `PlayerEvent`.
3. [x] Implement audible-time wall-clock fallback and embedded sink sample counter. `SessionTracker` uses the counter delta when available, falls back to wall-clock time, and stores production progress samples.
4. [x] Listen qualification at finalization. Verified by `crates/spotuify-daemon/tests/session_tracker_finalize.rs`; includes regression coverage that cached track duration, not last playback position, drives qualification for long-track skips.
5. [x] Rebuild logic: `analytics rebuild` recomputes derivations from `analytics_events`.
6. [x] Incremental track rollup: on each finalized listen, update `track_metrics`.
7. [x] Rich daily habit rollups: habits now derive `top_hour_of_day`, `exploration_ratio`, and `repeat_ratio` from `listen_facts` at read time. Verified by `habit_buckets_include_top_hour_exploration_and_repeat_ratios`.
8. [x] CLI wiring for analytics top/habits/search/rediscovery/rebuild/prune. Export remains a follow-up; Last.fm import is now implemented as a nested import command with the `--target lastfm` compatibility alias.
9. [x] Recipes directory with sample shell-hook scrobblers. Verified `docs/recipes/scrobble-listenbrainz.sh`, `docs/recipes/scrobble-lastfm.sh`, and `docs/recipes/notify-discord-listening.sh` with `bash -n`.
10. [x] Private-session suppression for `ListenQualified`; local `listen_facts.private_session` still persists.
11. [x] Retention: `analytics prune` dry-run/apply is wired; daemon startup and daily background retention prune use the same configured retention windows. Verified by `retention_cutoffs_honor_configured_windows`, `cargo check -p spotuify-daemon`, and daemon clippy.
12. [x] MCP tools for `analytics_top`, `analytics_habits`, `analytics_search`, and `analytics_rediscovery`.
13. [x] Last.fm historical import persistence, provider, resolution, promotion, status, unresolved, undo, CLI, IPC, daemon events, and MCP tools. Verified by focused store/daemon/CLI/protocol/MCP/player tests, `cargo clippy --all-targets -- -D warnings`, `cargo build --locked --release`, `scripts/smoke.sh`, and CLI help snapshots.

## Verification

- Play a track for ~60% of its length → `listen_qualified` fires, `listen_facts.qualified = true`, `track_metrics.qualified_count` increments.
- Skip a track in <5s → `listen_facts.qualified = false`, `track_metrics.skip_count` +1, qualified_count unchanged.
- Skip 31s into a cached 4-minute track → `listen_facts.duration_ms = 240000`, `qualified = false`; guards against using last playback position as track duration.
- AirPods disconnect mid-track (simulated by injecting `SessionDisconnected`) → `skip_reason = session_died`, NOT counted as qualified.
- `analytics top --kind tracks --since 30d` matches equivalent hand-written SQL within ±0 rows.
- `analytics habits --window week` returns one row per ISO week with non-negative listening minutes.
- `analytics habits` includes top hour, exploration ratio, and repeat ratio; tested with deterministic day-bucket data.
- `analytics rebuild` is idempotent: running twice produces identical derived tables.
- Private session → no listen_qualified emitted; `listen_facts.private_session = true`.
- Shell hook: configure `[analytics] hook_command = scrobble-listenbrainz.sh`, play a track to qualified threshold, scrobble appears on ListenBrainz.
- Recipe scripts syntax-check with `bash -n docs/recipes/{scrobble-listenbrainz.sh,scrobble-lastfm.sh,notify-discord-listening.sh}`.
- MCP `analytics_top` returns same data as CLI `analytics top --format json`.
- `finalize_uses_injected_audio_counter_over_wall_clock` proves that production listen facts prefer the sink counter over wall-clock time.
- Session tracking inserts `playback_progress` rows with audible samples, sample rate, and channel count when a counter is available.
- Last.fm dry-run fetches/resolves without writing `external_scrobbles` or `listen_facts`.
- Last.fm apply then repeat apply does not duplicate raw scrobbles or promoted listen facts.
- `analytics import unresolved RUN_ID --format json` returns unresolved scrobble rows.
- `analytics import undo RUN_ID --dry-run` previews promoted fact removal; `--yes` removes promoted facts, rebuilds rollups, and preserves raw audit rows.
- MCP `analytics_import_lastfm`, `analytics_import_status`, `analytics_import_unresolved`, and `analytics_import_undo` route to the same daemon requests as the CLI.

## Definition of done

A week of normal usage produces non-trivial Wrapped-style output from `spotuify analytics top` and `spotuify analytics habits`. Existing Last.fm users can backfill historical scrobbles into local analytics with dry-run, apply, unresolved review, idempotency, and undo. The MCP server exposes the same data and import workflow. Sample shell-hook scripts let users scrobble live listens to Last.fm/ListenBrainz without bundling live write credentials in-tree. Privacy gate respected. Retention enforced. spotuify becomes the only Spotify TUI/CLI with first-class local listening analytics.
