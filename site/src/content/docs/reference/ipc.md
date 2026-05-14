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
| `LyricsGet` | `spotuify lyrics show` |
| `SetVizEnabled` | `spotuify viz enable/disable` |

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
