---
title: "Import Last.fm History"
description: "Backfill local analytics from timestamped Last.fm scrobbles."
---

Import your Last.fm listening history into `spotuify` when you want local analytics that predate your `spotuify` install. The import fetches Last.fm scrobbles, stores the raw rows in SQLite, resolves confident matches to Spotify tracks, and promotes those matches into qualified local listens.

Run a dry-run first. Apply only after the counts look right.

## What Gets Imported

A scrobble is one timestamped listen recorded by a scrobbling service. Last.fm is a music history service that stores those scrobbles across players and devices.

`spotuify` imports Last.fm history as:

| Data | Stored where | Used for analytics |
| --- | --- | --- |
| Raw Last.fm row | `external_scrobbles.raw_json` | no, kept as audit/history |
| Artist, track, album, MBIDs, URL, timestamp | `external_scrobbles` | yes, after resolution |
| Import run counts and state | `analytics_import_runs` | status and undo |
| Resolved Spotify listen | `listen_facts` | yes, if high confidence |

Imported listens are marked with `measurement_kind = "lastfm_scrobble_import"` and `external_scrobble_id`. They count as qualified listens, but their audible time is estimated from the Last.fm qualification lower bound because Last.fm does not contain the playback stop point or progress samples.

Verify the distinction:

```bash
spotuify analytics top --kind tracks --since all --format json
```

## Get a Last.fm API Key

Create a Last.fm API application and copy the API key. The import uses the read-only `user.getRecentTracks` endpoint. It does not need your Last.fm password and does not scrobble new listens.

Set credentials with environment variables for one run:

```bash
export SPOTUIFY_LASTFM_API_KEY="lastfm-api-key"
export SPOTUIFY_LASTFM_USER="your-lastfm-user"
```

Or put defaults in config:

```toml
[analytics]
lastfm_api_key = "lastfm-api-key"
lastfm_user = "your-lastfm-user"
```

CLI flags override both:

```bash
spotuify analytics import lastfm \
  --user your-lastfm-user \
  --api-key lastfm-api-key \
  --from 2024-01-01 \
  --to 2024-12-31 \
  --format json
```

## Preview an Import

Dry-run is the default. It fetches and resolves but does not write `external_scrobbles` or `listen_facts`.

```bash
spotuify analytics import lastfm \
  --user your-lastfm-user \
  --from 2024-01-01 \
  --to 2024-12-31 \
  --format json
```

Expected shape:

```json
{
  "run_id": "018f...",
  "provider": "lastfm",
  "username": "your-lastfm-user",
  "dry_run": true,
  "fetched": 1200,
  "stored": 0,
  "duplicates": 0,
  "resolved": 1138,
  "promoted": 0,
  "unresolved": 62,
  "started_at_ms": 1735689600000,
  "finished_at_ms": 1735689660000
}
```

If the command says the API key or username is missing, pass `--api-key` and `--user`, set the `SPOTUIFY_LASTFM_API_KEY` and `SPOTUIFY_LASTFM_USER` environment variables, or add the `[analytics]` config keys.

## Apply the Import

Apply writes the import audit rows and promotes high-confidence Spotify matches into `listen_facts`.

```bash
spotuify analytics import lastfm \
  --user your-lastfm-user \
  --from 2024-01-01 \
  --to 2024-12-31 \
  --apply \
  --format json
```

Save the returned `run_id`. You need it for status, unresolved review, or undo.

Expected shape:

```json
{
  "run_id": "018f...",
  "provider": "lastfm",
  "username": "your-lastfm-user",
  "dry_run": false,
  "fetched": 1200,
  "stored": 1200,
  "duplicates": 0,
  "resolved": 1138,
  "promoted": 1138,
  "unresolved": 62,
  "started_at_ms": 1735689600000,
  "finished_at_ms": 1735689660000
}
```

Apply is idempotent. Re-running the same range with the same user does not duplicate raw scrobbles or promoted listen facts.

## Check Status

```bash
spotuify analytics import status 018f... --format json
```

Expected shape:

```json
{
  "run_id": "018f...",
  "provider": "lastfm",
  "username": "your-lastfm-user",
  "state": "completed",
  "dry_run": false,
  "from_ms": 1704067200000,
  "to_ms": 1735689600000,
  "fetched": 1200,
  "stored": 1200,
  "duplicates": 0,
  "resolved": 1138,
  "promoted": 1138,
  "unresolved": 62,
  "cursor": null,
  "started_at_ms": 1735689600000,
  "finished_at_ms": 1735689660000
}
```

## Review Unresolved Scrobbles

Unresolved rows stay in `external_scrobbles` and are not promoted.

```bash
spotuify analytics import unresolved 018f... --format json
```

Expected shape:

```json
[
  {
    "id": 42,
    "scrobbled_at_ms": 1705000000000,
    "artist": "Artist",
    "track": "Track",
    "album": "Album",
    "url": "https://www.last.fm/music/Artist/_/Track",
    "resolution_status": "unresolved",
    "confidence": null
  }
]
```

This pass does not include a manual fuzzy-match UI. Use the unresolved list to decide whether the source metadata is worth fixing later.

## Undo a Run

Undo removes promoted `listen_facts` and rebuilds analytics rollups. It preserves the raw `external_scrobbles` audit rows.

Preview first:

```bash
spotuify analytics import undo 018f... --dry-run --format json
```

Expected shape:

```json
{
  "run_id": "018f...",
  "dry_run": true,
  "listen_facts_removed": 1138,
  "raw_scrobbles_preserved": 1200
}
```

Apply the undo:

```bash
spotuify analytics import undo 018f... --yes --format json
```

Verify analytics changed:

```bash
spotuify analytics top --kind tracks --since all --limit 10
spotuify analytics rediscovery --gap 90d
```

## How Matching Works

Resolution tries the cheapest source first:

1. Exact local cache match by track, artist, and album when available.
2. Local search for a single high-confidence track match.
3. Spotify search for a single high-confidence track match.

Ambiguous, low-confidence, malformed, or unavailable rows remain unresolved. They stay stored for audit, but they do not affect analytics.

## Troubleshooting

`Last.fm username required`

Pass `--user`, set `SPOTUIFY_LASTFM_USER`, or add `analytics.lastfm_user` to config.

`Last.fm API key required`

Pass `--api-key`, set `SPOTUIFY_LASTFM_API_KEY`, or add `analytics.lastfm_api_key` to config.

`Last.fm rate limited (29)`

Wait and retry with a narrower date range:

```bash
spotuify analytics import lastfm --from 2024-01-01 --to 2024-03-31
```

Too many unresolved rows

Sync or search Spotify first so the local cache has more metadata, then dry-run again:

```bash
spotuify sync
spotuify search "artist track" --type track
spotuify analytics import lastfm --from 2024-01-01 --to 2024-12-31 --format json
```

## See Also

- [Analytics and Hooks](/guides/analytics-hooks/)
- [Config](/reference/config/)
- [JSON Output](/reference/json-output/)
- [IPC Protocol](/reference/ipc/)
- [spotuify analytics import](/reference/cli/analytics-import/)
