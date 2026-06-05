# Phase 16 - Lyrics

## Goal

Show synced lyrics in the TUI scrolling with playback position, sourced from Spotify's own backend via embedded librespot's mercury bus, with LRCLIB as a fallback for tracks Spotify doesn't have lyrics for. Persistent local cache.

## Evidence base

| Source | Reference | Notes |
|---|---|---|
| Spotify lyrics via librespot mercury | spotify-player `client/mod.rs:642-661` | `librespot_metadata::Lyrics::get(&session, &track_id)`. Musixmatch-sourced |
| LRCLIB HTTP API | spotatui `infra/network/utils.rs:89-141` | Free community lyrics database; `https://lrclib.net/api/get` with track/artist/duration |
| Line-level alignment via binary-search-by-progress | spotify-player `ui/page.rs:579+` | Each line is `(start_time_ms, text)`; render by finding active index |
| RTL / bidirectional text handling | spotify-player | `unicode-bidi` crate |
| Failure semantics | spotify-player | Treat "not found" as `Ok(None)` rather than error |

## Provider strategy

1. **Spotify (preferred)** — via embedded librespot's mercury bus. Synced. Same source as the official Spotify app. Requires Phase 9's embedded backend; falls back to (2) when mercury lyrics are unavailable.
2. **LRCLIB (fallback)** — public HTTP API. Synced when available; plain text when only that exists. No auth required, but they ask for rate-limit etiquette (max ~5 req/s) and a `User-Agent`.
3. **None (last resort)** — show "No lyrics available" with a link/button to suggest manual config.

Provider selection happens per-track and is cached. If Spotify returns "not found", try LRCLIB before giving up.

## Deliverables

### `crates/spotuify-lyrics`
Leaf crate for parsing and provider adapters:

```text
crates/spotuify-lyrics/
├── src/
│   ├── lib.rs
│   ├── types.rs           // SyncedLyrics, LyricLine, Provider
│   ├── spotify_provider.rs   // via spotuify-player::mercury_get
│   ├── lrclib_provider.rs
│   ├── parser.rs          // LRC format parser (regex-free, shared between providers)
│   ├── cache.rs           // superseded: daemon/store own SQLite persistence
│   └── alignment.rs       // binary-search active-line lookup
```

The original plan put cache and mercury access directly in this crate.
Current implementation keeps `spotuify-lyrics` small: it depends on
`spotuify-core`, parses Spotify mercury payload bytes, and implements
LRCLIB fetching. The daemon owns `PlayerBackend::mercury_get` access and
`spotuify-store` owns `lyrics_cache` / `lyrics_offsets` persistence.

### Wire format

```rust
pub struct SyncedLyrics {
    pub provider: Provider,
    pub track_uri: String,
    pub lines: Vec<LyricLine>,
    pub fetched_at_ms: i64,
    pub synced: bool,                   // false = plain text fallback
    pub language: Option<String>,
    pub source_url: Option<String>,
}

pub struct LyricLine {
    pub start_ms: u64,
    pub text: String,
    pub is_rtl: bool,                   // derived via unicode-bidi
}

pub enum Provider {
    SpotifyMercury,
    Lrclib,
}
```

### LRC parser
Parse `[mm:ss.xx]` and `[mm:ss.xxx]` timestamps. Lines with no timestamp are appended to the previous line's text (Musixmatch's "multi-line per timestamp" pattern). Handles BOM, malformed timestamps (skip with warning), and multiple timestamps per line (duplicate the line).

Reference: spotatui `utils.rs:106-141` is a good template; refine error handling.

### Persistence
- SQLite table `lyrics_cache`:
  ```
  track_uri TEXT PRIMARY KEY
  provider TEXT NOT NULL
  synced INTEGER NOT NULL
  lines_json TEXT NOT NULL
  fetched_at_ms INTEGER NOT NULL
  source_url TEXT
  ```
- TTL: 30 days (configurable). LRCLIB lyrics rarely change; Spotify mercury lyrics also stable.
- Cache miss → fetch → store.
- ETag/If-Modified-Since: LRCLIB doesn't support reliably; rely on TTL.

### LRCLIB etiquette
- Set `User-Agent: spotuify/<version> (https://github.com/bhekanik/spotuify)`.
- Rate-limit to 2 req/s globally.
- Backoff and retry once on 429, honoring numeric `Retry-After` seconds.
- Send `track_name`, `artist_name`, `album_name`, `duration` (seconds) as query params.
- Try `/api/get` first (exact match), `/api/search` second if no result.

### TUI integration
- Lyrics tab/panel on the Player screen.
- Active line centered vertically, surrounding lines fade.
- Bidirectional text rendered correctly (`unicode-bidi`).
- Manual offset adjustment (`+50ms` / `-50ms`) saved per-track in `lyrics_offsets` table.
- "Lyrics" command in command palette opens lyrics panel.
- Falls back gracefully: plain text scroll if not synced.

### Alignment algorithm
Compute `current_position_ms` from Phase 9's `PlayerEvent::PositionChanged` and the derived offset. Find active line by binary search:

```rust
let idx = lyrics.lines.partition_point(|line| line.start_ms <= position_ms);
let active = idx.saturating_sub(1);
```

Re-render only when `active` changes (avoid every-frame re-render).

### CLI commands
- `spotuify lyrics show [--track URI] [--format table|json|jsonl|csv|ids]`
- `spotuify lyrics follow [--lines N] [--lead OFFSET] [--format table|jsonl]`
  follows the current track and advances synced lines from local playback time.
- `spotuify lyrics fetch <track-uri>` (force refresh)
- `spotuify lyrics export <track-uri> [--output FILE]` writes LRC to stdout or a file
- `spotuify lyrics offset <track-uri> +50ms` (save per-track timing tweak)
- Provider selection is currently automatic: Spotify mercury first, LRCLIB fallback. A manual `provider --set spotify|lrclib|auto` command is deferred until there is a validated need to override automatic fallback.

### MCP integration
- `lyrics` tool in MCP server (Phase 8) returns synced lyrics for current or specified track.

## Work items

1. [x] New `crates/spotuify-lyrics` crate.
2. [x] LRC parser with tests for: BOM, 2/3-digit ms, duplicate timestamps, malformed lines.
3. [x] Spotify mercury parser plus daemon-side `mercury_get("hm://lyrics/v1/track/{id}")` fallback path.
4. [x] LrclibProvider with HTTP client, etiquette wrapper, 2 req/s pacing, and 429 retry/backoff.
5. [x] SQLite migration + cache layer in `spotuify-store`.
6. [x] unicode-bidi integration for RTL.
7. [x] TUI Lyrics screen/panel bound into ratatui layout and the action registry.
8. [x] Manual offset persistence in `lyrics_offsets`.
9. [x] CLI commands for show, follow, fetch, export, and offset. Manual provider selection intentionally deferred.
10. [x] MCP `lyrics` tool.
11. [x] Cache status reports lyrics cache and offset counts. Provider
    config reporting is intentionally omitted while provider selection is automatic.

## Verification

- Spotify track with known lyrics on `--backend embedded`: synced lyrics scroll in time.
- Same track with Spotify mercury unavailable: falls back to LRCLIB, still synced when LRCLIB has it.
- Arabic/Hebrew track: RTL rendering correct.
- Track with no Spotify lyrics, no LRCLIB entry: "No lyrics available" shown without errors.
- Offline/restart cache path: cached lyrics render without refetching after daemon restart; missing ones show the no-lyrics state.
- 100 rapid track changes (test playlist): no race conditions, cache fills correctly, no orphan rows.
- `spotuify lyrics export <uri>` produces a valid LRC file that mpv or VLC can render.
- `spotuify lyrics follow --lines 3` renders a previous/current/next window for the current track, and `--format jsonl` emits one object per active-line change.
- Manual offset `+200ms` persists across daemon restart.
- `spotuify-lyrics` wiremock test covers exact-match 429 + `Retry-After: 0` followed by success.
- CLI tests cover `lyrics export --output` parsing and LRC timestamp rendering.
- Store tests cover lyrics cache/offset round-trips and migrations.
- TUI tests cover opening the Lyrics screen through keyboard/action flow.
- Daemon tests cover LRCLIB fallback through the real `LyricsGet` handler and cached lyrics surviving daemon restart without refetching.

## Definition of done

Lyrics appear in the Player TUI panel, scroll with the song, support RTL
languages, fall back gracefully between providers, cache locally, and survive
daemon restart. CLI exposes the same lyrics for scripts, including terminal
follow mode, LRC export, and current-track force refresh through
`refresh-media`. MCP tool returns lyrics for agent consumption.
