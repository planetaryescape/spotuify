---
title: "spotuify login"
description: "Authorize Spotify and store a refresh token in macOS Keychain"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Authorize Spotify and store a refresh token in macOS Keychain

## Examples

```bash
spotuify login
spotuify login --redirect-uri http://127.0.0.1:8888/callback
```

## Help

```text
Authorize Spotify and store a refresh token in macOS Keychain

Usage: spotuify login [OPTIONS]

Options:
      --log-format <LOG_FORMAT>      Phase 13 (P13-A) - pick the daemon log format for this run. Also honoured via `SPOTUIFY_LOG_FORMAT` [possible values: text, json]
      --redirect-uri <REDIRECT_URI>  Override the redirect URI registered in Spotify's Developer Dashboard
      --no-daemon-start              Phase 13 (P13-H) - if set, the CLI never auto-starts the daemon. Errors with a clear hint when the daemon socket is missing
  -o, --set <key.path=value>         Phase 13 (P13-H) - one-shot TOML override (e.g. `-o player.bitrate=160`). Repeatable. Applies for this invocation only; the config file on disk is unchanged
  -h, --help                         Print help
```
