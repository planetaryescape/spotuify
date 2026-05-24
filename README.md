# spotuify

`spotuify` is a keyboard-native Spotify TUI, CLI, and MCP server. It uses embedded `librespot` for Connect playback (sub-100ms control), your operating system's credential store for OAuth tokens (macOS Keychain, Linux Secret Service, Windows Credential Manager), Spotify's Web API for metadata + library, and `ratatui-image` for album and podcast artwork in capable terminals.

The goal is simple: run `spotuify`. If credentials or OAuth are missing, setup starts automatically. Paste your Spotify app credentials, authorize in the browser, and land in a synced terminal UI without hand-editing config files.

<p align="center"><img src="assets/spotuify-demo.gif" alt="spotuify terminal demo: search, play, queue, and device control" /></p>

## Features

- Guided first-run onboarding with Spotify Dashboard instructions.
- OAuth PKCE login with tokens stored in the platform's native credential vault under service `spotuify`.
- Owned config file at `~/.config/spotuify/spotuify.toml`.
- Config commands for `path`, `init`, `get`, and `set`.
- Embedded librespot registers spotuify as a Spotify Connect device at daemon start.
- Playback controls: play, pause, next, previous, seek, volume, shuffle, repeat.
- Search across tracks, albums, playlists, and podcast episodes.
- Queue viewing and add-to-queue support.
- Playlist browsing and quick add-current-to-playlist flow.
- Device list and Spotify Connect transfer.
- Cover art rendering through Kitty, iTerm2, Sixel, or half-block fallback.
- Fully keyboard navigable with vim-style movement, pane switching, help overlay, paging, and back navigation.
- Local analytics: `listen_facts` plus `spotuify analytics top` / `habits` / `rediscovery` for Wrapped-style insights, with shell-hook recipes for ListenBrainz, Last.fm, and Discord.
- Operation log + undo: mutating commands are recorded; `spotuify ops undo --dry-run` previews and `spotuify ops undo --yes` applies reversible undo. MCP exposes `undo_last` as a safety net for agent runs.
- MCP server over stdio or loopback HTTP for agents.
- Audio visualization through embedded sink taps or loopback capture.

## Requirements

- A Spotify Premium account (required for librespot streaming).
- A terminal with good image support for best visuals. Kitty works well; other terminals fall back through `ratatui-image` support.

## Install

Spotuify ships pre-built binaries for macOS (Apple Silicon + Intel), Linux gnu / musl, and Windows on each [GitHub Release](https://github.com/planetaryescape/spotuify/releases). Pick the right path for your platform:

### macOS (Apple Silicon or Intel)

```sh
brew install planetaryescape/tap/spotuify
spotuify daemon install-service   # registers a launchd LaunchAgent
spotuify                          # first run kicks off onboarding
```

Or tap once, then use the short formula name:

```sh
brew tap planetaryescape/tap
brew install spotuify
```

Binaries are unsigned today. If Gatekeeper blocks the first launch:

```sh
xattr -d com.apple.quarantine /opt/homebrew/bin/spotuify
```

### Linux (Debian / Ubuntu)

```sh
sudo dpkg -i spotuify_*_amd64.deb     # from a GH Releases asset
spotuify daemon install-service       # registers a systemd --user unit
spotuify
```

Spotuify uses Secret Service (GNOME Keyring / KWallet) for credential storage. Headless encrypted-file credential fallback is planned but not exposed as a stable login flag yet.

### Linux (Arch / Fedora / other)

```sh
cargo install --git https://github.com/planetaryescape/spotuify --locked
spotuify daemon install-service
```

### Windows

```sh
scoop install planetaryescape/spotuify   # (Scoop bucket — pending)
# or grab the .zip from GitHub Releases and add it to %PATH%
spotuify daemon install-service          # registers a Task Scheduler logon trigger
```

Daemon-mode media-key handling on Windows is currently limited: SMTC requires a foreground window handle, so background-only operation cannot register media keys. Workaround: keep the TUI process alive.

### Nix

```sh
nix run github:planetaryescape/spotuify
# or in a flake:
inputs.spotuify.url = "github:planetaryescape/spotuify";
```

### From source (any platform)

Plain first-time `brew install spotuify` requires acceptance into `homebrew/core`. The release workflow below publishes to a tap, which is the standard path for immediate installs.

From this repository:

```sh
cargo build
./target/debug/spotuify --help
```

Install into your Cargo bin path:

```sh
cargo install --path .
spotuify --help
```

If `~/.cargo/bin` is not on your `PATH`, either add it or run `./target/release/spotuify` directly from the repo.

For platform-specific embedded librespot builds, pick the right audio backend feature flag:

```sh
# Linux (alsa, routes through pipewire-alsa shim on modern distros):
cargo install --git https://github.com/planetaryescape/spotuify --locked \
              --features 'embedded-playback,system-integrations,loopback-cpal,alsa-backend'

# Linux musl (pure-Rust rodio backend):
cargo install --git https://github.com/planetaryescape/spotuify --locked \
              --no-default-features \
              --features 'embedded-playback,system-integrations,loopback-cpal,rodio-backend'

# macOS (PortAudio bridge to CoreAudio):
cargo install --git https://github.com/planetaryescape/spotuify --locked \
              --features 'embedded-playback,system-integrations,loopback-cpal,portaudio-backend'

# Windows (rodio writes to WASAPI):
cargo install --git https://github.com/planetaryescape/spotuify --locked \
              --features 'embedded-playback,system-integrations,loopback-cpal,rodio-backend'
```

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
planetaryescape/homebrew-tap
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

[player]
backend = "embedded"
bitrate = 320

[notifications]
enabled = false
summary = "{track}"
body = "{artist} - {album}"
on_track_change = true
on_pause = false
on_resume = false
on_skip = false
on_error = true
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
spotuify config get player.backend
spotuify config set client_id "..."
spotuify config set client_secret "..."
spotuify config set redirect_uri "http://127.0.0.1:8888/callback"
spotuify config set player.device_name "spotuify"
spotuify config set notifications.enabled true
```

Valid config keys:

```text
client_id
client_secret
redirect_uri
player.backend
player.bitrate
player.device_name
player.normalization
player.audio_cache_mib
player.pulse_props
player.event_hook          # legacy alias for analytics.hook_command
analytics.hook_command
analytics.hook_timeout_ms
cache.cover_cache_mb
cache.cover_cache_ttl_days
notifications.enabled
notifications.summary
notifications.body
notifications.on_track_change
notifications.on_pause
notifications.on_resume
notifications.on_skip
notifications.on_error
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
spotuify status --format json
spotuify devices
spotuify search "query" --type track --source local
spotuify play "query"
spotuify play-uri spotify:track:...
spotuify pause
spotuify resume
spotuify next
spotuify previous
spotuify seek +30s
spotuify volume 50
spotuify shuffle on
spotuify repeat track
spotuify queue
spotuify queue add spotify:track:...
spotuify playlists
spotuify playlist plan "brief" --format json
spotuify playlist create "Name" --from candidates.jsonl --dry-run
spotuify library tracks
spotuify analytics top --kind tracks --format json
spotuify lyrics show --track spotify:track:...
spotuify viz enable
spotuify hooks test
spotuify mpris status
spotuify sync search-cache --prune --older-than 7d
spotuify cache status
spotuify cache reset --confirm
spotuify ops log
spotuify ops undo --dry-run
spotuify reload
spotuify reconnect
spotuify bug-report --include-logs 200
spotuify generate completions zsh
spotuify mcp
```

Command behavior:

- `spotuify` opens the TUI. If config or OAuth are missing, it starts setup first, syncs, then opens the TUI.
- `spotuify onboard` runs the same setup flow intentionally. It reuses saved credentials when they already exist.
- `spotuify login` reruns OAuth using existing config.
- `spotuify logout` removes the Keychain token.
- `spotuify doctor` checks config, token status, API access timings, visible devices, recent playback, queue, playlists, logs, cache version, lyrics, MCP, and player backend state.
- `spotuify logs path` prints the log file path.
- `spotuify logs tail --follow --format json` streams structured log lines.
- `spotuify config ...` reads or writes `spotuify.toml` without requiring valid Spotify credentials.
- One-shot commands talk to the daemon over local IPC. Use `--no-daemon-start` when scripts should fail instead of starting the daemon.

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

## Playback backend

`spotuify` registers itself as a Spotify Connect device through an in-process librespot session at daemon start — no separate subprocess. Premium account required (librespot streaming constraint).

Prefer a specific device name visible to other Spotify clients:

```sh
spotuify config set player.device_name "spotuify"
```

Audio cache (disk), bitrate, normalization, and PulseAudio property hints are all on the `[player]` config section.

## What Sync Means

During onboarding, after OAuth completes, `spotuify` immediately calls Spotify and fetches:

- Current playback state.
- Visible Spotify Connect devices.
- Queue state.
- User playlists.

This verifies the token, scopes, and API access before you enter the TUI. If one non-critical endpoint is unavailable, onboarding prints the skipped endpoint and continues so you can still open the app.

## Use with an LLM agent (MCP)

`spotuify` exposes its daemon as a Model Context Protocol server so LLM clients (Claude Code, Cursor, Continue, agent harnesses) can use it as a tool.

```bash
# Claude Code
claude mcp add spotuify --command spotuify --args mcp

# Cursor: add to .cursor/mcp.json
{
  "mcpServers": {
    "spotuify": { "command": "spotuify", "args": ["mcp"] }
  }
}

# Continue: add to ~/.continue/config.json under mcpServers
"spotuify": { "command": "spotuify", "args": ["mcp"] }
```

Tools exposed:

- Read: `search`, `now_playing`, `devices_list`, `queue_show`, `playlists_list`, `playlist_tracks`, `library_list`
- Transport: `play`, `play_uri`, `pause`, `resume`, `next`, `previous`, `seek`, `volume`, `shuffle`, `repeat`
- Destructive (require `confirm: true`): `queue_add`, `transfer_device`, `playlist_create`, `playlist_add`, `playlist_remove`, `library_save`, `library_unsave`
- Lyrics: `lyrics`
- Analytics: `analytics_top`, `analytics_habits`, `analytics_search`, `analytics_rediscovery`
- Ops: `ops_log`, `undo_last` (`undo_last` is the safety net)

Resources:

- `spotuify://playback` — current playback state
- `spotuify://devices` — visible Spotify Connect devices
- `spotuify://playlists` — user playlists
- `spotuify://doctor` — latest health-check report

Destructive tools called without `confirm: true` return a preview the LLM can show to the user. The LLM is expected to relay it and ask before retrying with `confirm: true`. Patterns adopted from spotify-player commit #966.

## How spotuify differs

| If you want... | `spotuify` chooses... |
|---|---|
| A terminal-first controller for scripts and agents | CLI and MCP surfaces first; the TUI is another client |
| Playback that keeps running after the UI exits | Daemon-backed control through local IPC |
| A local library/search runtime | SQLite cache plus rebuildable search index |
| Maximum desktop integration polish today | Use an official Spotify client or a desktop-first app instead |
| The smallest possible binary with no daemon | `spotuify` is not optimizing for that trade-off |

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

Then check that at least one Spotify Connect device is active. The embedded librespot device registers at daemon start; check `spotuify daemon status` for the player-ready event. Premium account required.

Art does not render:

Use Kitty or another terminal supported by `ratatui-image`. The app falls back to block rendering when terminal image protocols are unavailable.

Visualizer shows no bars on macOS loopback:

macOS does not expose system-output loopback devices by default. Use the embedded playback backend for sink-tap visualization, or install BlackHole/Loopback Audio and select it as the loopback input source. Without a virtual loopback device, `spotuify` may fall back to the microphone input so the visualizer stays quiet while music plays.

Windows daemon media keys:

Windows media-key integration needs a UI/window handle. The headless daemon still handles CLI, TUI, and MCP playback commands, but global Windows media keys may require a foreground TUI session until the hidden-window driver is complete.

Embedded librespot fails to register:

```sh
spotuify doctor
spotuify daemon status
```

The daemon panics at startup when librespot can't bind an audio backend. Rebuild with one of `--features alsa-backend / pipewire-backend / rodio-backend / portaudio-backend` for your platform.

## Security Notes

- OAuth refresh tokens are stored in macOS Keychain under service `spotuify`.
- Spotify app credentials are stored in `~/.config/spotuify/spotuify.toml` unless provided through environment variables.
- `spotuify logout` removes the stored OAuth token from Keychain.
- To rotate credentials, create a new Spotify app secret in the dashboard and rerun `spotuify onboard` or `spotuify config set client_secret "..."`.

## Development

Common checks:

```sh
cargo fmt --check
scripts/cargo-test -p spotuify-spotify --tests
cargo clippy -p spotuify-spotify --all-targets -- -D warnings
```

Use package-scoped checks while iterating. Full workspace clippy/tests and
release builds are release gates, not the default edit/verify loop. To smoke an
already-built binary with the fake provider:

```sh
scripts/smoke.sh
```

To have smoke build the release binary first:

```sh
SPOTUIFY_SMOKE_BUILD=1 scripts/smoke.sh
```

Run locally:

```sh
cargo run
cargo run -- onboard
cargo run -- doctor
```

## Project Layout

```text
src/main.rs                    unified binary entrypoint and legacy adapters
crates/spotuify-protocol       daemon IPC protocol and shared wire types
crates/spotuify-daemon         daemon state, handlers, diagnostics, sync coordination
crates/spotuify-cli            reusable CLI argument/output helpers
crates/spotuify-tui            Ratatui app state, actions, and rendering
crates/spotuify-spotify        Spotify Web API, OAuth, config, rate limits
crates/spotuify-player         embedded librespot backend (sole supported runtime)
crates/spotuify-store          SQLite cache, migrations, operation log
crates/spotuify-search         local search index
crates/spotuify-mcp            MCP stdio/HTTP server
crates/spotuify-lyrics         Spotify/LRCLIB lyrics providers
crates/spotuify-system         cover cache, notifications, media-control helpers
crates/spotuify-audio          audio analyzer and visualization sources
```
