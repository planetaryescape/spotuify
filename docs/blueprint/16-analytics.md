# spotuify - Analytics

## Principle

Analytics is a first-class product surface, not logging. Every client action, playback state change, search, selection, and provider call should feed a local SQLite event store.

## Source of truth

Use an append-only local event log first. Derived tables and summaries are rebuildable.

Initial table:

```text
analytics_events
- id
- kind
- occurred_at_ms
- received_at_ms
- source
- subject_uri
- search_query
- search_query_hash
- payload_json
```

Initial inspection command:

```text
spotuify analytics events --limit 50 --format table|json|jsonl|csv|ids
```

`source` distinguishes CLI, TUI, daemon, imports, and provider telemetry.

Historical imports use separate audit tables before they become listen facts:

```text
analytics_import_runs
- run_id
- provider
- username
- state
- dry_run
- from_ms / to_ms
- fetched / stored / duplicates / resolved / promoted / unresolved
- cursor
- started_at_ms / finished_at_ms

external_scrobbles
- id
- provider
- username
- import_run_id
- idempotency_key
- scrobbled_at_ms
- artist_name / track_name / album_name
- artist_mbid / track_mbid / album_mbid
- url
- raw_json
- resolution_status
- resolved_track_uri
- confidence
```

Promoted imports point back to `external_scrobbles` through
`listen_facts.external_scrobble_id` and mark
`measurement_kind = "lastfm_scrobble_import"`.

## Event classes

Core events:

- `action_finished`: command/action, result, source, subject URI, counts or command payload.
- `search_performed`: raw query, normalized query, query hash, result count, latency.
- `search_result_selected`: query/session id, selected URI, rank, time to select.
- `playback_started`: track/episode URI, context, source, device, start position.
- `playback_paused` / `playback_resumed`: URI, position, device.
- `playback_skipped`: URI, position, elapsed, reason.
- `playback_completed`: URI, elapsed, completion ratio.
- `listen_qualified`: emitted once a play crosses the scrobble threshold.
- `spotify_api_finished`: redacted provider path, method, status, elapsed, error class.

## Listen qualification

Store partial plays even if they do not count as listens.

For durable listens and future Last.fm/ListenBrainz export, qualify when track duration is over 30 seconds and audible play time reaches `min(50% of duration, 4 minutes)`.

Persist the qualification rule version with derived listen facts.

Current code truth: `listen_facts` use the daemon session tracker's elapsed-minus-paused wall-clock fallback. The embedded sink has an `AudioCounterTap`, but the session tracker does not yet consume `AudioCounterHandle::audible_ms()` in production. `playback_progress` exists and is pruned, but current code does not insert production progress samples.
Observed playback uses actual audible samples when the embedded sink tap is
available and wall-clock fallback otherwise. Last.fm imports cannot reconstruct
pause, stop, or progress samples, so imported facts use the qualification lower
bound for `audible_ms` and keep their measurement kind explicit.
## Search analytics

Search is its own journey:

```text
search_performed -> search_result_selected -> playback_started -> listen_qualified
```

Store raw queries locally because the user wants personal analytics. Also store normalized query hash so aggregate analysis and redacted export are possible.

## Derived metrics

Track metrics:

- play count
- qualified listen count
- skip count
- completion rate
- average completion ratio
- time to skip
- repeat rate
- rediscovery after 30/90/365 day gaps
- source mix: search, playlist, album, queue, library

Artist and genre metrics:

- total listens
- unique tracks/albums
- binge sessions
- active weeks/months
- discovery velocity
- genre by hour/weekday
- diversity/entropy over time

Habit metrics:

- listening minutes by day/week/month
- active days and streaks
- sessions per day
- tracks per session
- weekday/hour heatmap
- exploration vs comfort ratio
- release-era distribution

## Privacy

Default is local-first. Raw queries and raw progress samples stay local unless the user opts into export.

Provider telemetry must redact query params such as `q`, `ids`, `uri`, and market before persistence.

Private Spotify sessions should suppress external scrobbling and can downgrade local event payloads to aggregate-only mode later.

## Retention

Suggested defaults:

- raw progress samples: 90 days
- action/search/playback events: 1 year
- derived listen facts and aggregates: forever until user deletes

Retention must be user-configurable once daemon settings exist.

## Implementation order

1. SQLite `analytics_events` store and event builders.
2. Provider API telemetry from the Spotify request seams.
3. Shared action-layer events for CLI and TUI.
4. Playback progress/session tracker in daemon.
5. Derived listen facts and top-N analytics queries.
6. Live shell-hook recipes for ListenBrainz/Last.fm/Discord.
7. Provider export/import bridges only after the CLI/IPC/store path is implemented.
6. Historical Last.fm import with raw scrobble audit rows, idempotent promotion,
   unresolved reporting, and undo.
7. Export bridges or additional import providers only when there is a validated
   product need.
