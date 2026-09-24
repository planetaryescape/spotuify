# spotuify - Player Contract

## Principle

This is a music app. The player must be impeccable.

The user should be able to open the TUI, start a playlist, close the terminal, and keep listening. The user should be able to run `spotuify next` from a random shell and trust it.

## Playback device model

spotuify controls Spotify Connect devices. The preferred target is the daemon's embedded librespot device, configured as `spotuify-hume` on this machine.

The daemon should ensure the preferred device is running and visible before playback mutations that require a device. If it is not available, playback should fail with remediation instead of falling through to an unrelated visible device.

## Required player actions

- play selected item
- play query result
- play URI
- play album or playlist context while publishing a queue snapshot that shows
  the first track as current and the remaining tracks as up next
- pause
- resume
- toggle play/pause
- next
- previous
- seek forward/back
- seek absolute position
- set volume
- mute if supported
- shuffle on/off/toggle
- repeat off/context/track
- play playlist
- play album
- play artist radio-like context where supported
- queue one track
- queue selected tracks
- queue from stdin IDs
- save/like current track
- unlike current track
- follow artist
- show queue
- show current context

## Repeat semantics

Spotify repeat maps to:

- `off`: no repeat
- `context`: repeat album/playlist/context
- `track`: repeat one

TUI labels should say `repeat off`, `repeat context`, and `repeat one` rather than exposing only raw API terms.

## Lyrics

Spotify Web API does not provide an official lyrics endpoint. Lyrics are a
provider-backed feature below the daemon/player boundary, not a core Web API
guarantee.

Current provider shape:

- Spotify mercury via the embedded librespot session when available
- LRCLIB fallback when Spotify lyrics are missing
- cached lyrics and per-track offset persistence
- `spotuify lyrics show|follow|fetch|export|offset`
- `spotuify refresh-media` and TUI `U` for current-track force refresh
- no lyrics with a clear unsupported or unavailable message

## Small and big player

TUI should support:

- compact player: one-line current track, status, time, device
- default player: album art, track, artist, controls, progress
- large player: bigger album art, queue/context, lyrics if configured

## Album art

Album art should degrade gracefully:

- real image where terminal supports it
- block/halfblock fallback
- text fallback

Art loading should never block input.

Selection artwork is not only for albums and playlists. Search and Library
selection previews should also show artwork for saved shows and episodes when
Spotify provides an image URL.

## Device failure UX

If playback fails with no active device:

1. daemon runs device refresh
2. daemon attempts preferred device activation
3. if still unavailable, error says exactly what is visible
4. user sees `spotuify devices` remediation

No raw `404 No active device found` should be the final UX.

## Recovery and teardown invariants

- A successful play response is not proof of sound. For local playback, the
  track must still be playing and the PCM sample counter must advance after the
  audio watchdog window.
- The audio watchdog samples every 2 seconds and treats 6 seconds of flat PCM
  while the embedded device owns playback as a stall.
- Reconnect must tear down the old native player before registering a new sink.
- Teardown has a 2-second wait budget. Missing that budget does not prove the
  resource was reclaimed: the cleanup thread may still own PortAudio or
  CoreAudio state.
- After a teardown timeout or cleanup-thread spawn failure, the embedded backend
  stays blocked until a clean daemon restart. No recovery path may create a
  replacement sink in the same process.
