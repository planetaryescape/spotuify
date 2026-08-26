# spotuify shell-hook recipes

Spotuify emits a `listen-qualified` event every time a track crosses the
qualification threshold (`audible_ms >= max(30s, min(50% of duration, 4min))`).
By pointing `analytics.hook_command` at one of these scripts in
`~/.config/spotuify/spotuify.toml`, you can bridge listens into your
external scrobbler of choice without bundling live scrobbling auth flows
inside spotuify.

```toml
[analytics]
hook_command = "/path/to/spotuify/docs/recipes/scrobble-listenbrainz.sh"
hook_timeout_ms = 5000
```

The daemon invokes the hook for playback events. Recipes that scrobble must
ignore every event except `listen-qualified`.

Qualified-listen invocations include these environment variables:

| Variable | Description |
| --- | --- |
| `SPOTUIFY_EVENT` | `listen-qualified` |
| `SPOTUIFY_URI` | URI of the qualifying track |
| `SPOTUIFY_TRACK_URI` | Compatibility alias for `SPOTUIFY_URI` |
| `SPOTUIFY_TRACK` | Cached track name |
| `SPOTUIFY_ARTIST` | Cached primary artist name |
| `SPOTUIFY_ALBUM` | Cached album name, when known |
| `SPOTUIFY_DURATION_MS` | Total track duration in ms |
| `SPOTUIFY_AUDIBLE_MS` | Audible time accrued (excludes paused intervals) |
| `SPOTUIFY_STARTED_AT_MS` | Playback start time as Unix epoch milliseconds |
| `SPOTUIFY_ARTIST_URI` | `spotify:artist:…` URI (may be empty) |
| `SPOTUIFY_ALBUM_URI` | `spotify:album:…` URI (may be empty) |

Hooks are fire-and-forget: spotuify spawns them in the background with a
configurable hard timeout (`hook_timeout_ms`, default 5s), and any
non-zero exit or timeout is logged at `warn` but does not affect
playback.

## Recipes in this directory

- `scrobble-listenbrainz.sh` posts to ListenBrainz `submit-listens`.
  It requires `LISTENBRAINZ_TOKEN`, `curl`, and `jq`.
- `scrobble-lastfm.sh` signs and posts a Last.fm `track.scrobble` request.
  It requires the Last.fm API key, API secret, session key, `curl`, `jq`,
  and `openssl`.
- `notify-discord-listening.sh` posts a now-playing embed to a Discord
  webhook (`DISCORD_WEBHOOK_URL`).

## Why this design

Bundling Last.fm or ListenBrainz authentication in spotuify would add stored
secrets and provider-specific API code. Shell hooks keep write credentials and
request signing outside the daemon.

Historical Last.fm import is different: it uses the read-only
`user.getRecentTracks` endpoint to backfill local analytics. Use the CLI
for that path:

```bash
spotuify analytics import lastfm --user your-lastfm-user --from 2024-01-01
spotuify analytics import lastfm --user your-lastfm-user --from 2024-01-01 --apply
```

If you write a useful hook, PRs adding new scripts here are welcome.
