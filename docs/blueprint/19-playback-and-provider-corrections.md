# 19: Recovery must respect playback ownership

Apply D-044, D-047, D-048 and D-049. Preserve existing IPC and database schemas; these corrections belong below client rendering.

## Deactivation is not a lost connection

The pinned librespot fork emits `SessionDisconnected` from Spirc's `handle_disconnect` when a foreign device takes over. A separate wrapper reports actual Spirc task exit. The baseline translated both into spotuify's session-loss recovery path. The source chain is pinned in [the investigation](../research/2026-10-26-reliability-audit.md).

| Event | Local ownership | Recovery | Global playback |
|---|---|---|---|
| Embedded playback starts outside a deactivated interval | Claim local | Existing policy | Apply local player truth |
| Connect deactivates local device | Clear immediately | Cancel delayed recovery | Pause local clock only if it still describes the local device |
| A track loader finishes after deactivation | Keep local inactive | Never restore intent from stale events | Preserve foreign playback until a new Connect activation |
| Actual session loss / player failure | Preserve established intent | Existing bounded backoff, recheck ownership after waiting | Resume only when current intent still permits it |
| Another provider/device owns playback | Keep local inactive | Never reclaim from stale snapshot | Preserve foreign playback truth |

Translate Connect handoff into internal `PlayerEvent::Deactivated`. Keep the existing client event envelope and use `PlaybackChanged` action `deactivated`. Check ownership again after recovery backoff and inside the actor before teardown and before resume; intent can change while either task waits. Carry the existing mutation generation so a newer command supersedes an older automatic recovery. Explicit reconnect remains a user command. A backend error and a handoff are separate states even if upstream reuses a name.

Pinned-fork review found a second boundary: the player processes one command, then polls its track loader. Between the handoff notification and queued Stop, that loader can emit both TrackChanged and Playing. The upstream SessionConnected signal comes from handle_activate and supplies the reactivation boundary. Preserve it internally as Activated; after Deactivated, wait for that signal before accepting playback events again. Registration alone is not activation. The daemon must still retain own-device volume bookkeeping without applying it to foreign playback.

The recovery acceptance test uses a mock player and call log to prove that deactivation does not issue reconnect/resume. The live acceptance test moves music to an external device and watches for local recovery beyond the configured watchdog window. S3 remains open until that live observation succeeds.

## A transfer error must name the selected device

The flow refreshes devices before resolving ID or exact name. Retain that behaviour: Spotify documents that device IDs are not guaranteed to remain stable. Reject a device marked restricted before issuing the transfer. Send a single selected device ID; Spotify supports only one per transfer request.

A plain upstream 404 is a failed transfer, not proof of a particular hardware or account cause. Report the selected device and suggest opening Spotify there, refreshing devices and retrying. For Echo identifiers, mention starting the speaker in Alexa or Spotify as a possible recovery step, without claiming that every Echo requires it. Preserve 401, 403 and 429 as distinct failures.

Do not report successful playback from an accepted HTTP response alone. Compare refreshed playback ownership and sustained progress. Spotify explicitly says ordering across Player API endpoints is not guaranteed. The exact cause of the original transfer 404 remains S2.

## Playlist metadata and playlist contents are different capabilities

Search can expose a playlist whose contents the current API credential cannot read. The observed request to `/playlists/<id>/items` returned 403. An unqualified “Forbidden” does not prove the playlist is private.

Keep explicit region, subscription, temporary and private reasons when Spotify supplies them. An unclassified 403 stays a typed permission failure that names the playlist and suggests checking app permissions and Development Mode restrictions. Do not turn it into an empty playlist, successful queue mutation or automatic credential escalation (D-047).

The official February migration guide describes restricted playlist-content access. The announcement's March 9 update postponed endpoint changes for existing integrations. Treat the observed account response as evidence for this case, and avoid claiming every credential has identical access.

For queueing a playlist, use the existing collection expansion path and its bounded failure. If expansion is denied, leave the queue unchanged and show the terminal failure. An explicit playlist play action may still be offered; it has its own outcome and does not establish that enumerating the contents is supported. Q2 records that product assumption.

## Direct CLI references must reach the provider

`spotuify playlist tracks <URI>` and `spotuify playlist play <URI>` must accept canonical playlist references and Spotify share links without requiring the playlist to appear in the user's saved list. Validate resource kind and provider ownership, then dispatch the existing daemon request. Continue resolving human playlist names from saved playlists. Leave playlist mutation selection and confirmation unchanged.

```sh
spotuify playlist tracks spotify:playlist:<id> --format json
spotuify playlist play https://open.spotify.com/playlist/<id> --format json
```

These commands promise routing and accurate errors; they do not grant API access Spotify denies. A bare ID still uses the existing resolution path. Add coverage for direct URI/share links, provider conflicts, wrong resource kinds and existing name resolution.

## Doctor must inspect the process that owns the runtime

When the daemon socket is reachable, CLI doctor requests `GetDoctorReport` and receives provider checks, device diagnostics and event findings from that process. Preserve an explicit local fallback for an offline daemon. An unreachable daemon must not be silently auto-started merely to make diagnostics appear green.

Elect health after all findings have been appended, including player event findings and the daemon's audio state. `healthy`, `health_class` and findings must agree. Keep bounded request timeouts and rate-limit handling. A skipped check is not evidence that a provider worked.

```sh
spotuify doctor --format json
spotuify ops log --limit 10 --format json
spotuify ops show <operation-id> --format json
```

Do not delete operation history or reset cooldowns to improve the result of these commands.

## Tail reads have a fixed inspection budget

`spotuify logs tail [lines]` reads a snapshot of at most 1 MiB plus one boundary byte. Zero lines and missing files return empty output. If the requested complete lines do not fit, the command returns an explicit error asking for fewer lines or direct file inspection. It never silently labels a cut record as complete. Daily-file retention still needs the separate [disk budget correction](../issues/003-log-storage-budget.md).

## Context playback uses the same request on every client

The correction adds `play-uri --context <collection>` to express the TUI's existing selected-track request. A context requires a track or episode target, a supported collection kind and the same provider. Supported collection kinds are album, artist, playlist and show; `spotuify:collection:liked` resolves the selected provider's cached Liked Songs. Omitted context preserves existing behaviour. MCP `play` and `play_uri` accept the equivalent optional `context_uri`.

```sh
spotuify play-uri spotify:track:<track-id> --context spotify:playlist:<playlist-id> --format json
spotuify queue add spotify:playlist:<playlist-id> --wait --format json
```

Queue Add accepts tracks, episodes, albums and playlists. Collection expansion requires the provider's matching item-read capability as well as queue-add support. `--many` continues to use the existing aggregate request. The daemon still owns expansion and mutation receipts; clients do not fetch/expand the collections themselves.
