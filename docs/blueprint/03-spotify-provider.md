# spotuify - Spotify Provider

## Provider role

The Spotify provider is the adapter between spotuify's internal model and Spotify's Web API plus Spotify Connect behavior.

It should isolate Spotify-specific quirks from the daemon, CLI, and TUI.

## API categories

### Auth

- OAuth PKCE for local CLI/TUI use.
- Refresh token stored in system keyring.
- Scope requests should be minimal and explained.

### Catalog

- tracks
- albums
- artists
- playlists
- shows
- episodes
- audiobooks
- chapters

### User library

- saved tracks
- saved albums
- saved episodes
- saved shows
- saved audiobooks
- followed artists
- followed playlists

### Playlists

- list current user playlists
- create playlist
- update metadata
- list items
- add items
- remove items
- reorder items
- replace items
- cover image support later

### Player

- playback state
- currently playing
- devices
- transfer playback
- play/resume
- pause
- next/previous
- seek
- shuffle
- repeat
- volume
- queue read
- queue add

### Personalization

- recently played
- top tracks
- top artists

## Known Spotify limitations

- The Web API does not stream audio.
- A real Spotify Connect device must exist for playback.
- Queue removal and queue reorder are not exposed by the Web API.
- Official lyrics are not exposed by the Web API. Lyrics require an optional external provider or no feature.
- Playback control requires Premium.
- Some endpoints and fields have changed under Spotify's 2026 developer access changes.
- Search `limit` must respect Spotify's current max.
- Rate limits must respect `Retry-After`.

## Device strategy

Preferred order:

1. Active unrestricted device.
2. Configured device name, currently `spotuify-hume` for this machine.
3. Device name containing `spotifyd` as fallback.
4. Sole unrestricted device.
5. Helpful error with `spotuify devices` output.

## spotifyd/librespot role

spotifyd is the long-lived playback device. spotuify is the controller.

Closing the TUI must never kill spotifyd. `spotuify daemon` may manage spotifyd lifecycle, but it must treat the player process as persistent infrastructure.

## Error normalization

Provider errors should map into typed categories:

- `AuthExpired`
- `AuthDenied`
- `PremiumRequired`
- `NoActiveDevice`
- `DeviceUnavailable`
- `RateLimited`
- `NetworkTimeout`
- `SpotifyServerError`
- `DecodeError`
- `UnsupportedCapability`

CLI and TUI should render these with remediation commands.
