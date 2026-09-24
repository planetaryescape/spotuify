# spotuify - Observability

## Philosophy

Music apps fail in messy ways: auth, network, rate limits, device visibility, Spotify server errors, terminal rendering, and local cache drift.

Observability is user experience.

## Doctor

`spotuify doctor` should check:

- config path and parsed config
- auth token status
- auth file status
- daemon status
- socket health
- embedded player state
- local audio health: session connection, playback ownership, PCM sample
  progress, last stall, reconnect attempts, and current backoff
- preferred device visibility
- Spotify playback endpoint
- devices endpoint
- queue endpoint
- playlists endpoint
- recent tracks endpoint
- cache status
- search index status
- log path

It should never hang indefinitely. Every external dependency gets a bounded timeout.

## Diagnostics commands

```text
spotuify doctor --format json
spotuify daemon status --format json
spotuify sync status --format json
spotuify cache status --format json
spotuify search status --format json
spotuify logs tail 200
spotuify bug-report --sanitize
```

For local playback, `audio_health.samples_advancing` is the user-visible
postcondition. A provider response or `is_playing: true` only proves control
state. It does not prove that decoded audio reached the sink. Release playback
checks must wait past the 6-second watchdog window and confirm the same track is
still playing with samples advancing.

On macOS, stop the daemon and run `/usr/bin/afplay` when both the session and
playback clock look healthy but samples stay flat. If the system player hangs
too, the fault is below `spotuify` in CoreAudio. Fix that layer before retrying
the embedded player.

## TUI diagnostics

Diagnostics tab should show:

- daemon lifecycle
- auth state
- preferred device state
- last successful sync
- last API errors
- rate-limit status
- local row counts
- index freshness
- recent action trace
- recent mutation receipts

## Action trace

Daemon records a bounded action trace:

- timestamp
- request ID
- client type
- command/action
- duration
- result
- error class

Debug export should be JSONL.

## Bug report

`spotuify bug-report --sanitize` should collect:

- version
- platform
- config without secrets
- daemon status
- doctor summary
- recent logs
- recent API errors
- recent action trace
- cache/index metadata

It must never include access tokens, refresh tokens, client secret, or credential file contents.
