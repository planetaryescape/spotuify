# spotuify - Spotify Provider

## Provider role

The Spotify provider is the adapter between spotuify's internal model and Spotify's Web API plus Spotify Connect behavior.

It should isolate Spotify-specific quirks from the daemon, CLI, and TUI.

## API categories

### Auth

- OAuth PKCE for local CLI/TUI use.
- Refresh token stored in the private auth file.
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
2. The daemon's own embedded-device id, when known.
3. Configured device name, currently `spotuify-hume` for this machine.
4. Device name containing `spotuify` or `librespot`.
5. Name-substring overlap with the configured preferred name.
6. Helpful error with `spotuify devices` output.

Do not fall back to an unrelated unrestricted device merely because it is visible. Playback is a mutation; if the preferred target is unavailable, fail with remediation rather than surprise-starting another room or account device.

## Embedded librespot role

The daemon owns an embedded librespot session and registers spotuify as a local Spotify Connect device.

Closing the TUI must never kill playback. `spotuify daemon` owns the player lifecycle and exposes device/playback state to CLI, TUI, MCP, and agents.

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

## Spotify Web API rules (for agents and contributors)

- **OpenAPI spec**: Refer to the Spotify OpenAPI specification at https://developer.spotify.com/reference/web-api/open-api-schema.yaml for all endpoint paths, parameters, and response schemas. Do not guess endpoints or field names.
- **Authorization**: Use the Authorization Code with PKCE flow for any user-specific data. If the app has a secure backend, the Authorization Code flow is also acceptable. Only use Client Credentials for public, non-user data. Never use the Implicit Grant flow (deprecated).
- **Redirect URIs**: Always use HTTPS redirect URIs (except `http://127.0.0.1` for local development). Never use `http://localhost` or wildcard URIs.
- **Scopes**: Request only the minimum scopes needed for the features being built. Do not request broad scopes preemptively.
- **Token management**: Store tokens securely. Never expose the Client Secret in client-side code. Implement token refresh logic so the app does not break when access tokens expire.
- **Rate limits**: Implement exponential backoff and respect the `Retry-After` header when receiving HTTP 429 responses. Do not retry immediately or in tight loops.
- **Deprecated endpoints**: Do not use deprecated endpoints. Prefer `/playlists/{id}/items` over `/playlists/{id}/tracks`, and use `/me/library` over the type-specific library endpoints.
- **Error handling**: Handle all HTTP error codes documented in the OpenAPI schema. Read the returned error message and use it to provide meaningful feedback.
- **Developer Terms**: Comply with the Spotify Developer Terms. Do not cache Spotify content beyond what is needed for immediate use, always attribute content to Spotify, and do not use the API to train machine learning models on Spotify data.
- **Genre fields**: Spotify carries `genres` on artist and album objects only, not on track objects. Do not expect genre data when fetching tracks directly.
