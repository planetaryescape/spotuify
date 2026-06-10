---
title: "IPC Protocol"
description: "Document daemon request, response, event, and error contracts."
---

The daemon protocol is length-delimited JSON over local IPC. Unix builds use Unix-domain sockets. Windows builds use Tokio named pipes. CLI, TUI, scripts, and MCP bridge through the same frame codec and request/response shape.

## Transport

| Platform | Transport | Notes |
| --- | --- | --- |
| macOS | Unix-domain socket | Resolved under the app support/runtime path for the active instance. |
| Linux | Unix-domain socket | Prefers `$XDG_RUNTIME_DIR`, then `/run/user/$uid`, then a private `/tmp/spotuify-$uid` fallback. |
| Windows | Named pipe | Uses a `\\.\pipe\...` path and keeps the next pipe instance ready before handing the accepted stream to a task. |

The transport is intentionally below the protocol. A Windows client still sends the same JSON request envelope as a macOS or Linux client.

## Envelope

```json
{
  "id": 1,
  "payload": {
    "type": "Request",
    "cmd": "playback-get"
  }
}
```

## Core requests

```bash
spotuify status --format json
spotuify devices --format json
spotuify search "quiet storm" --format json
spotuify queue --format json
```

Representative request variants:

| Request | CLI surface |
| --- | --- |
| `ClientSeed` | TUI/event clients only; cached startup seed |
| `PlaybackGet` | `spotuify status` |
| `PlaybackCommand` | `pause`, `resume`, `toggle`, `next`, `previous`, `seek`, `volume`, `shuffle`, `repeat` |
| `DevicesList` | `spotuify devices` |
| `DeviceTransfer` | `spotuify transfer` |
| `Search` | `spotuify search` |
| `QueueGet` | `spotuify queue` |
| `QueueAdd` | `spotuify queue add` |
| `PlaylistsList` | `spotuify playlists` |
| `PlaylistTracks` | `spotuify playlist tracks` |
| `PlaylistAddItems` | `spotuify playlist add` |
| `ArtistAlbums` | `spotuify artist albums` |
| `FollowedArtists` | `spotuify artist followed` |
| `LibrarySave` | `spotuify like`, `spotuify save` |
| `ShowEpisodes` | `spotuify show episodes` |
| `EpisodeFeed` | `spotuify episodes` |
| `CoverArt` | TUI art fetch, `spotuify refresh-media` |
| `LyricsGet` | `spotuify lyrics show`, `spotuify lyrics follow`, `spotuify refresh-media` |
| `SubscribeEvents` | `spotuify lyrics follow`, TUI/event clients |
| `SetVizEnabled` | `spotuify viz enable/disable` |
| `ReminderCreate` / `RemindersList` / `ReminderCancel` | `spotuify reminder ...` |
| `NotificationsList` / `NotificationAct` | `spotuify notifications ...` |
| `CheckUpdate` | `spotuify update`, TUI/app update banners |

`ClientSeed` is deliberately client-specific. It hydrates event-driven clients from cached playback, queue, devices, recent items, and visualizer state. It must not trigger Spotify refreshes; live refreshes belong to daemon warm/sync loops or explicit CLI requests.

`refresh-media` is a CLI convenience over `PlaybackGet`, `CoverArt`, and a
force-refresh `LyricsGet` for the current track. It does not clear existing
client media while the new fetch is in flight.

`lyrics follow` is a watch client over existing protocol calls. It subscribes
to `PlaybackChanged`, fetches lyrics with `LyricsGet` on track change, and
advances the active lyric line locally from playback time.

`ArtistAlbums` returns the full discography in one response. The daemon tags
each album with `album_group` (album, single, compilation, or appears-on) and
`in_library` by intersecting against the cached saved-album set. Clients
section and filter from that single payload, so the "in library" toggle never
needs a refetch. `FollowedArtists` is cache-backed and falls back to a live
fetch when the cache is cold. See [JSON Output](/reference/json-output/) for the
tagged row shape.

`EpisodeFeed` merges the first page of episodes from followed shows, caches the
feed for quick repeat reads, and supports `--refresh` when you want a live
re-fetch. `CheckUpdate` returns the cached GitHub release observation and an
upgrade hint for the current install method; the background daemon loop refreshes
that observation on startup and every few hours.

## Admin requests

```bash
spotuify daemon status --format json
spotuify doctor --format json
spotuify cache status --format json
spotuify reindex --format json
```

Representative request variants:

| Request | CLI surface |
| --- | --- |
| `GetDaemonStatus` | `spotuify daemon status` |
| `GetDoctorReport` | `spotuify doctor` |
| `Reindex` | `spotuify reindex` |
| `CacheStatus` | `spotuify cache status` |
| `Sync` | `spotuify sync` |
| `LogsTail` | `spotuify logs tail` |
| `Reload` | `spotuify reload` |
| `Reconnect` | `spotuify reconnect` |

## Response shape

```json
{
  "Ok": {
    "data": {
      "kind": "Playback",
      "playback": {}
    }
  }
}
```

Errors are typed:

```json
{
  "Error": {
    "message": "no active device",
    "kind": "provider",
    "code": "provider",
    "retryable": false
  }
}
```

Error kinds:

```text
auth
invalid_request
network
provider
rate_limited
unsupported
internal
```

## Events

The daemon broadcasts state changes so clients do not have to poll forever.

```text
playback-changed
queue-changed
devices-changed
playlists-changed
library-changed
search-updated
search-page
search-complete
search-failed
event-stream-lagged
sync-started
sync-finished
mutation-finished
rate-limited
auth-error
mutation-accepted
mutation-finalized
schema-compat
player-ready
player-degraded
premium-required
session-disconnected
player-failed
listen-qualified
operation-recorded
operation-undone
config-reloaded
spectrum-frame
viz-source-changed
reminder-due
reminders-changed
update-available
```

## See Also

- [Architecture](/guides/architecture/)
- [JSON Output](/reference/json-output/)
- [CLI Reference](/reference/cli/)
