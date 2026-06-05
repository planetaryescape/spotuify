---
title: "IPC Protocol"
description: "Document daemon request, response, event, and error contracts."
---

The daemon protocol is length-delimited JSON over a local socket. CLI, TUI, scripts, and MCP bridge through this shape.

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
| `LibrarySave` | `spotuify like`, `spotuify save` |
| `CoverArt` | TUI art fetch, `spotuify refresh-media` |
| `LyricsGet` | `spotuify lyrics show`, `spotuify lyrics follow`, `spotuify refresh-media` |
| `SubscribeEvents` | `spotuify lyrics follow`, TUI/event clients |
| `SetVizEnabled` | `spotuify viz enable/disable` |

`ClientSeed` is deliberately client-specific. It hydrates event-driven clients from cached playback, queue, devices, recent items, and visualizer state. It must not trigger Spotify refreshes; live refreshes belong to daemon warm/sync loops or explicit CLI requests.

`refresh-media` is a CLI convenience over `PlaybackGet`, `CoverArt`, and a
force-refresh `LyricsGet` for the current track. It does not clear existing
client media while the new fetch is in flight.

`lyrics follow` is a watch client over existing protocol calls. It subscribes
to `PlaybackChanged`, fetches lyrics with `LyricsGet` on track change, and
advances the active lyric line locally from playback time.

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
sync-started
sync-finished
mutation-finished
rate-limited
spectrum-frame
```

## See Also

- [Architecture](/guides/architecture/)
- [JSON Output](/reference/json-output/)
- [CLI Reference](/reference/cli/)
