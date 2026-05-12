# spotuify - Player Contract

## Principle

This is a music app. The player must be impeccable.

The user should be able to open the TUI, start a playlist, close the terminal, and keep listening. The user should be able to run `spotuify next` from a random shell and trust it.

## Playback device model

spotuify controls Spotify Connect devices. The preferred target is `spotifyd` configured as `spotuify-hume` on this machine.

The daemon should ensure the preferred device is running and visible before playback mutations that require a device.

## Required player actions

- play selected item
- play query result
- play URI
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

Spotify Web API does not provide an official lyrics endpoint. Lyrics are a future optional provider feature, not a core Spotify-provider guarantee.

Possible providers later:

- synced lyrics provider if legally usable
- local `.lrc` files
- no lyrics with clear unsupported message

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

## Device failure UX

If playback fails with no active device:

1. daemon runs device refresh
2. daemon attempts preferred device activation
3. if still unavailable, error says exactly what is visible
4. user sees `spotuify devices` remediation

No raw `404 No active device found` should be the final UX.
