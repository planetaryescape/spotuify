# spotuify

`spotuify` is a keyboard-native Spotify TUI for macOS and Spotify Connect devices. It uses Spotify's Web API for playback control, macOS Keychain for OAuth tokens, `spotifyd` for local playback when available, and `ratatui-image` for album and podcast artwork in capable terminals.

The goal is simple: run `spotuify`. If credentials or OAuth are missing, setup starts automatically. Paste your Spotify app credentials, authorize in the browser, and land in a synced terminal UI without hand-editing config files.

## Features

- Guided first-run onboarding with Spotify Dashboard instructions.
- OAuth PKCE login with tokens stored in macOS Keychain under service `spotuify`.
- Owned config file at `~/.config/spotuify/spotuify.toml`.
- Config commands for `path`, `init`, `get`, and `set`.
- Automatic `spotifyd` startup when installed and enabled.
- Spotify playback controls: play, pause, next, previous, seek, volume, shuffle, repeat.
- Search across tracks, albums, playlists, and podcast episodes.
- Queue viewing and add-to-queue support.
- Playlist browsing and quick add-current-to-playlist flow.
- Device list and Spotify Connect transfer.
- Cover art rendering through Kitty, iTerm2, Sixel, or half-block fallback.
- Fully keyboard navigable with vim-style movement, pane switching, help overlay, paging, and back navigation.

## Requirements

- macOS.
- Rust toolchain with `cargo`.
- Spotify Premium for playback control and `spotifyd` playback.
- A Spotify account.
- A terminal with good image support for best visuals. Kitty works well. Other terminals fall back through `ratatui-image` support.
- Optional: `spotifyd` for local headless playback.

Install `spotifyd` with Homebrew if you want automatic local playback:

```sh
brew install spotifyd
```

`spotuify` still works with any visible Spotify Connect device if `spotifyd` is not installed.

## Install

With Homebrew, after the tap is published:

```sh
brew install bhekanik/tap/spotuify
```

Or tap once, then use the short formula name:

```sh
brew tap bhekanik/tap
brew install spotuify
```

Plain first-time `brew install spotuify` requires acceptance into `homebrew/core`. The release workflow below publishes to a tap, which is the standard path for immediate installs.

From this repository:

```sh
cargo build --release
./target/release/spotuify --help
```

Install into your Cargo bin path:

```sh
cargo install --path .
spotuify --help
```

If `~/.cargo/bin` is not on your `PATH`, either add it or run `./target/release/spotuify` directly from the repo.

## Releases

Releases are managed by Release Please and GitHub Actions.

The flow:

1. Merge normal feature/fix PRs into `main` using conventional commit messages such as `fix: improve diagnostics` or `feat: add playlist view`.
2. Release Please opens or updates a release PR with the next version, `CHANGELOG.md`, `Cargo.toml`, and `Cargo.lock` updates.
3. Merge the Release Please PR.
4. Release Please creates the GitHub release and tag.
5. The release workflow builds macOS arm64 and Intel binaries, uploads them to the GitHub release, generates a Homebrew formula, and updates the tap.

Required GitHub setup:

```text
Secret: HOMEBREW_TAP_TOKEN
Variable: HOMEBREW_TAP_REPOSITORY
```

`HOMEBREW_TAP_TOKEN` must be a token with write access to the Homebrew tap repository.

`HOMEBREW_TAP_REPOSITORY` should be the tap repo in `owner/repo` form, for example:

```text
bhekanik/homebrew-tap
```

If `HOMEBREW_TAP_REPOSITORY` is omitted, the workflow defaults to:

```text
<github-owner>/homebrew-tap
```

The tap repo should contain a `Formula/` directory. The release workflow will create or update `Formula/spotuify.rb`.

Manual release rebuild:

```text
GitHub Actions -> Release -> Run workflow -> tag v0.1.0
```

## First Run

Run the app:

```sh
spotuify
```

That is the low-friction path. If config or OAuth are missing, `spotuify` starts setup automatically, syncs Spotify data, then opens the TUI.

You can also run setup intentionally:

```sh
spotuify onboard
```

The onboarding flow does this in order:

1. Creates `~/.config/spotuify/spotuify.toml` if it does not exist.
2. Reuses saved Spotify app credentials if they already exist.
3. Opens the Spotify Developer Dashboard when credentials are missing.
4. Shows the exact Spotify app settings you need.
5. Prompts for `Client ID`.
6. Prompts for `Client Secret`.
7. Prompts for the redirect URI, defaulting to `http://127.0.0.1:8888/callback`.
8. Saves credentials into `spotuify.toml`.
9. Starts OAuth login in your browser.
10. Stores the resulting refresh token in macOS Keychain.
11. Immediately syncs playback, devices, queue, and playlists.

After setup succeeds, plain `spotuify` opens the TUI directly on later runs:

```sh
spotuify
```

## Spotify OAuth App Setup

Plain `spotuify` walks you through this when needed, but these are the full steps if you want to understand what is happening.

Open the Spotify Developer Dashboard:

```text
https://developer.spotify.com/dashboard
```

Create a new app:

```text
App name: spotuify
App description: Terminal Spotify client
Website: leave blank or use your own site
Redirect URI: http://127.0.0.1:8888/callback
API/SDK: Web API
```

Important redirect URI details:

- Put `http://127.0.0.1:8888/callback` in Redirect URIs under App -> Settings.
- Do not put it in the Website field.
- Do not use `localhost`.
- Do not add a trailing slash.
- Spotify permits HTTP for explicit loopback IP redirect URIs.

If Spotify rejects the fixed-port URI, add this redirect URI instead:

```text
http://127.0.0.1/callback
```

Then run login with the matching redirect URI:

```sh
spotuify login --redirect-uri http://127.0.0.1/callback
```

After the app is created, copy values from Basic Information:

```text
Client ID
Client Secret
```

Paste them into the onboarding prompts. `spotuify` writes them to `~/.config/spotuify/spotuify.toml`.

## Configuration

Default config path:

```text
~/.config/spotuify/spotuify.toml
```

Default config template:

```toml
# spotuify config
client_id = ""
client_secret = ""
redirect_uri = "http://127.0.0.1:8888/callback"

[spotifyd]
autostart = true
# config_path = "~/.config/spotifyd/spotifyd.conf"
# device_name = "spotuify"
```

Supported environment overrides:

```sh
SPOTUIFY_CONFIG=/path/to/spotuify.toml
SPOTUIFY_CLIENT_ID=...
SPOTUIFY_CLIENT_SECRET=...
SPOTUIFY_REDIRECT_URI=http://127.0.0.1:8888/callback
```

Config commands:

```sh
spotuify config path
spotuify config init
spotuify config get client_id
spotuify config get redirect_uri
spotuify config get spotifyd.autostart
spotuify config set client_id "..."
spotuify config set client_secret "..."
spotuify config set redirect_uri "http://127.0.0.1:8888/callback"
spotuify config set spotifyd.autostart true
spotuify config set spotifyd.config_path "~/.config/spotifyd/spotifyd.conf"
spotuify config set spotifyd.device_name "spotuify"
```

Valid config keys:

```text
client_id
client_secret
redirect_uri
spotifyd.config_path
spotifyd.device_name
spotifyd.autostart
```

## Commands

```sh
spotuify
spotuify onboard
spotuify login
spotuify logout
spotuify doctor
spotuify logs path
spotuify logs tail
spotuify config path
spotuify config init
spotuify config get <key>
spotuify config set <key> <value>
```

Command behavior:

- `spotuify` opens the TUI. If config or OAuth are missing, it starts setup first, syncs, then opens the TUI.
- `spotuify onboard` runs the same setup flow intentionally. It reuses saved credentials when they already exist.
- `spotuify login` reruns OAuth using existing config.
- `spotuify logout` removes the Keychain token.
- `spotuify doctor` checks config, token status, API access timings, visible devices, recent playback, queue, playlists, logs, and `spotifyd` state.
- `spotuify logs path` prints the log file path.
- `spotuify logs tail` prints recent log lines.
- `spotuify config ...` reads or writes `spotuify.toml` without requiring valid Spotify credentials.

## Keyboard

Global keys:

```text
?                 open or close help
q                 quit
1                 search
2                 queue
3                 playlists
4                 devices
Tab               next pane
Shift-Tab         previous pane
j / Down          move down
k / Up            move up
gg                jump to top
G                 jump to bottom
Ctrl-d / PageDown page down
Ctrl-u / PageUp   page up
Enter             activate selected item
Esc / b           back from playlist tracks
u                 refresh Spotify data
```

Playback keys:

```text
Space             play or pause
n                 next
p                 previous
Left              seek backward 15 seconds
Right             seek forward 15 seconds
+ / =             volume up
-                 volume down
s                 toggle shuffle
r                 cycle repeat mode
l                 save current track or episode
```

Search and library keys:

```text
/                 focus search input
Enter             search when input is focused
Enter             play selected search result
e                 add selected item to queue
A or a            add current track or episode to selected playlist
```

Playlist keys:

```text
Enter             open selected playlist
Enter             play selected playlist track
a                 add current item to selected playlist
Esc / b           return from playlist tracks to playlist list
```

Device keys:

```text
Enter             transfer playback to selected device
x                 transfer playback to selected device
```

## Spotifyd

`spotuify` can start `spotifyd` automatically when all of these are true:

- `spotifyd.autostart = true`.
- `spotifyd` is installed.
- No `spotifyd` process is already running.

Default `spotifyd` config path:

```text
~/.config/spotifyd/spotifyd.conf
```

Set a custom path:

```sh
spotuify config set spotifyd.config_path "~/.config/spotifyd/spotifyd.conf"
```

Disable autostart:

```sh
spotuify config set spotifyd.autostart false
```

Prefer a specific Spotify Connect device name:

```sh
spotuify config set spotifyd.device_name "spotuify"
```

`spotuify` uses Spotify Connect device discovery. If `spotifyd` is unavailable, use any visible Spotify Connect device.

## What Sync Means

During onboarding, after OAuth completes, `spotuify` immediately calls Spotify and fetches:

- Current playback state.
- Visible Spotify Connect devices.
- Queue state.
- User playlists.

This verifies the token, scopes, and API access before you enter the TUI. If one non-critical endpoint is unavailable, onboarding prints the skipped endpoint and continues so you can still open the app.

## Spotify API Limits

Spotify's public Web API exposes queue viewing and add-to-queue, but not queue remove or queue reorder. `spotuify` shows the queue honestly and does not pretend those unsupported actions exist.

Playback control requires Spotify Premium. Without Premium, auth can succeed but playback API calls may fail.

## Troubleshooting

Run the doctor first:

```sh
spotuify doctor
```

Then check the log file:

```sh
spotuify logs path
spotuify logs tail
```

On macOS, logs are written to:

```text
~/Library/Logs/spotuify/spotuify.log
```

If the TUI looks frozen, press `Ctrl+C`. The event loop listens for OS Ctrl+C directly. Regular Spotify refresh runs in a background task, so slow Spotify endpoints should not block keyboard input.

Generic Spotify request failure:

```text
error: Spotify request failed
```

Run:

```sh
spotuify doctor
spotuify logs tail
```

`doctor` prints each API section with timing and the full endpoint/status error, for example playback, devices, queue, playlists, and recently played.

Spotify API timeouts:

```text
operation timed out
```

Check whether the default route to Spotify is broken:

```sh
curl -I --max-time 8 https://api.spotify.com/v1
curl -4 -I --max-time 8 https://api.spotify.com/v1
```

If the first command times out and the `-4` command returns `401`, IPv6 routing to Spotify is broken on your network. `spotuify` forces Spotify API calls over IPv4 to avoid that path.

Redirect URI mismatch:

```text
redirect_uri: Not matching configuration
```

Fix:

```sh
spotuify config get redirect_uri
```

Make sure the exact value is listed in Spotify Dashboard -> App -> Settings -> Redirect URIs. Use `127.0.0.1`, not `localhost`, and avoid trailing slashes.

Missing client ID:

```text
client_id missing
```

Fix:

```sh
spotuify
```

That restarts setup automatically. Or set it manually:

```sh
spotuify config set client_id "..."
```

No token or expired token:

```sh
spotuify
```

That restarts OAuth automatically when the Keychain token is missing. Use `spotuify login` if you only want to rerun OAuth without the full setup flow.

No devices visible:

```sh
spotuify doctor
```

Then check that at least one Spotify Connect device is active. If using `spotifyd`, verify its config and that your Spotify account has Premium.

Art does not render:

Use Kitty or another terminal supported by `ratatui-image`. The app falls back to block rendering when terminal image protocols are unavailable.

`spotifyd` does not start:

```sh
which spotifyd
spotuify config get spotifyd.autostart
spotuify config get spotifyd.config_path
```

If Homebrew installed it at `/opt/homebrew/bin/spotifyd`, `spotuify` can find it automatically.

## Security Notes

- OAuth refresh tokens are stored in macOS Keychain under service `spotuify`.
- Spotify app credentials are stored in `~/.config/spotuify/spotuify.toml` unless provided through environment variables.
- `spotuify logout` removes the stored OAuth token from Keychain.
- To rotate credentials, create a new Spotify app secret in the dashboard and rerun `spotuify onboard` or `spotuify config set client_secret "..."`.

## Development

Common checks:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

Run locally:

cargo run
cargo run -- onboard
cargo run -- doctor
```

## Project Layout

```text
src/main.rs      CLI, onboarding, config commands, doctor
src/config.rs    config file loading and get/set helpers
src/auth.rs      Spotify OAuth PKCE and Keychain token storage
src/spotify.rs   Spotify Web API client and response mapping
src/spotifyd.rs  spotifyd process detection and autostart
src/app.rs       TUI state, event loop, and key handling
src/ui.rs        ratatui rendering, help overlay, and hint bar
```
