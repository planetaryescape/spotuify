---
title: "Install"
description: "Install spotuify, set config, login, and verify playback."
---

Install `spotuify`, give it Spotify app credentials, then run `doctor` before you trust playback.

## Requirements

- Spotify account. Premium is required for local playback through the embedded librespot device (`spotuify-hume`).
- A Spotify Developer app with a redirect URI such as `http://127.0.0.1:8888/callback`.
- A terminal. Kitty or iTerm2 gives better cover art, but the app has text fallbacks.

```bash
spotuify --help
```

## Homebrew

```bash
brew tap planetaryescape/tap
brew install spotuify
spotuify --help
```

## Cargo

```bash
cargo install --git https://github.com/planetaryescape/spotuify --locked
spotuify --help
```

From this repo:

```bash
cargo build --release
./target/release/spotuify --help
```

## Configure Spotify

Create the config file and set the keys you need.

```bash
spotuify config init
spotuify config path
spotuify config set client_id "$SPOTIFY_CLIENT_ID"
spotuify config set redirect_uri "http://127.0.0.1:8888/callback"
```

If your app still needs a client secret, set it too:

```bash
spotuify config set client_secret "$SPOTIFY_CLIENT_SECRET"
```

## Login

```bash
spotuify login
spotuify doctor
```

What you get: an OAuth token stored in the OS credential vault and a doctor report that tells you whether config, auth, daemon, device visibility, and Spotify API access work.

## Start the daemon

```bash
spotuify daemon start
spotuify daemon status --format json
```

Install the platform user service when you want the daemon to survive shell sessions.

```bash
spotuify daemon install-service
```

## First sound

```bash
spotuify devices
spotuify play "imagine dragons" --type track
```

If playback fails with no active device, activate or transfer to the device you want:

```bash
spotuify transfer spotuify-hume
spotuify play "imagine dragons"
```

## See Also

- [First Run](/getting-started/first-run/)
- [Player and Daemon](/guides/player-and-daemon/)
- [Troubleshooting](/reference/troubleshooting/)
