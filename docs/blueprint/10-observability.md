# spotuify - Observability

## Philosophy

Music apps fail in messy ways: auth, network, rate limits, device visibility, Spotify server errors, terminal rendering, and local cache drift.

Observability is user experience.

## Doctor

`spotuify doctor` should check:

- config path and parsed config
- auth token status
- keychain access latency
- daemon status
- socket health
- spotifyd process state
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
