# spotuify

spotuify is Spotify as a daemon on your machine. One process owns playback (embedded librespot, registered as a Spotify Connect device), the metadata cache, and a local search index. Four clients sit on top of one Unix socket: a keyboard-native TUI, a CLI that prints `json`, `jsonl`, `csv`, or `ids` for pipes, an MCP server so a coding agent can run your music the way you do, and a macOS menubar app. Search, play, queue, switch devices, build playlists, read synced lyrics, see album art, all without leaving the shell. Quit any client; the music keeps playing.

<p align="center"><img src="site/public/spotuify-demo.gif" alt="spotuify terminal demo: search, play, queue, and device control" /></p>

Run `spotuify` and you're in. If you're not configured yet, it creates a config file and tells you what to add. The default path uses your own Spotify Developer app with the PKCE browser flow; the experimental first-party/keymaster path is still opt-in.

GA scope: `spotuify` is BYO Spotify app GA for terminal users who are comfortable creating their own Spotify Developer app. It is not broad consumer no-developer setup yet; that would require a reviewed/shared Spotify app or a product decision to make first-party/keymaster auth the default. If writes return `403`, your app is probably still in Spotify Development Mode; apply for Extended Quota Mode in the Spotify dashboard.

## Why another Spotify TUI?

Fair question. `spotify-player`, `ncspot`, and the original `spotify-tui` already proved a terminal Spotify client is worth living in, and spotuify builds on what they shipped: embedded `librespot` for playback, a keyboard TUI, local library search. It pushes hard in one direction the others treat as a side feature. The CLI is the product, not a wrapper around the TUI.

- **Pipeable everywhere, not just on one command.** Every read, list, status, and search surface speaks `--format json`, `jsonl`, `csv`, or `ids`. `spotuify search "lo-fi beats" --type playlist --format ids` returns bare URIs you can pipe into anything, in any language. Other terminal clients give you JSON on a command or two, or an interactive UI you can't script at all.
- **Your agents can run it.** Tell an agent what you're in the mood for and it curates through the same ordinary commands you type (or the built-in MCP server): plan candidates, resolve tracks, preview the playlist, create it once you approve. Writes are preview-first: `--dry-run` shows what would change, `--yes` commits, and `spotuify ops undo` reverses the last one. A back button for your library, which you want the moment an agent is the one clicking.
- **The music keeps playing after you close the window.** A background daemon owns playback, queue, and devices; the TUI, CLI, and agents are all just views of it. Quit the TUI and the song keeps going. Run a command from another shell and it shows up instantly.
- **Search runs off a local cache.** A SQLite store plus a rebuildable index answer library and search queries from disk, so navigation is instant and an agent gets the same results twice. The cache is metadata only: track, album, playlist, and listening records. The audio itself always streams from Spotify, same as any Connect device.

Want the most polished desktop experience? Use the official app. Want Spotify as something you can type at, pipe, script, and hand to an agent? That's this.

## Features

- Browser login through Spotify OAuth PKCE.
- OAuth tokens stored in private auth files under the app config directory so detached daemons never wait on OS credential prompts.
- Owned config file at `~/.config/spotuify/spotuify.toml`.
- Config commands for `path`, `init`, `get`, and `set`.
- Embedded librespot registers spotuify as a Spotify Connect device at daemon start.
- Playback controls: play, pause, next, previous, seek, volume, shuffle, repeat.
- Playback actions take the daemon hot path first; embedded play/pause/next/seek/volume try local transport before waiting on Spotify Web API reconciliation.
- Search across tracks, episodes, shows/podcasts, albums, artists, and playlists.
- Queue viewing and add-to-queue support, with background warming for queued track metadata, cover art, lyrics, and next-track audio.
- Playlist browsing and quick add-current-to-playlist flow.
- Artist discography browser: list followed artists and browse an artist's full catalog grouped into albums, singles & EPs, compilations, and appears-on, with an in-library-only filter (`spotuify artist albums <uri> --library-only`, `spotuify artist followed`, or the TUI `L` toggle).
- Device list and Spotify Connect transfer.
- Cover art rendering through Kitty, iTerm2, Sixel, or half-block fallback.
- Manual current-track media refresh via `spotuify refresh-media` or `U` in the TUI.
- Synced lyrics in the TUI and terminal: `spotuify lyrics show`, `spotuify lyrics follow`, LRC export, and per-track offset tuning.
- Fully keyboard navigable with vim-style movement, pane switching, help overlay, paging, and back navigation.
- Local analytics: `listen_facts` plus `spotuify analytics top` / `habits` / `rediscovery` for Wrapped-style insights, Last.fm historical import, and shell-hook recipes for live ListenBrainz, Last.fm, and Discord integrations.
- Operation log + undo: mutating commands are recorded; `spotuify ops undo --dry-run` previews and `spotuify ops undo --yes` applies reversible undo. MCP exposes `undo_last` as a safety net for agent runs.
- MCP server over stdio or loopback HTTP for agents.
- Audio visualization through embedded sink taps or loopback capture.

## Requirements

- A Spotify Premium account (required for librespot streaming).
- A Spotify Developer app for the default BYO Spotify app GA auth path. Use redirect URI `http://127.0.0.1:8888/callback`; apply for Extended Quota Mode if you want playlist/library writes beyond Spotify's Development Mode limits.
- A terminal with good image support for best visuals. Kitty works well; other terminals fall back through `ratatui-image` support.

## Install

Prebuilt binaries ship for macOS (Apple Silicon and Intel), Linux x86_64, and Windows x64 on each [GitHub Release](https://github.com/planetaryescape/spotuify/releases); the Homebrew tap is the quickest macOS path. Other targets (Linux musl or arm, other distros) build from source with `cargo install` or Nix. Pick your platform:

### macOS (Apple Silicon or Intel)

```sh
brew tap planetaryescape/spotuify
brew trust --formula planetaryescape/spotuify/spotuify
brew install planetaryescape/spotuify/spotuify
spotuify daemon install-service   # registers a launchd LaunchAgent
spotuify                          # first run kicks off onboarding
```

To update an existing Homebrew install:

```sh
brew update
brew upgrade planetaryescape/spotuify/spotuify
```

Release archives include SHA256 checksums and GitHub artifact provenance attestations. macOS binaries are not notarized today, so Gatekeeper may still block the first launch:

```sh
xattr -d com.apple.quarantine /opt/homebrew/bin/spotuify
```

### macOS app (.dmg)

Prefer a native window? The latest GitHub Release can also include `Spotuify.dmg`, a SwiftUI app that bundles the `spotuify` daemon+CLI binary and installs it to `~/.local/bin/spotuify` on first launch:

```sh
curl -fsSLO https://github.com/planetaryescape/spotuify/releases/latest/download/Spotuify.dmg
curl -fsSLO https://github.com/planetaryescape/spotuify/releases/latest/download/Spotuify.dmg.sha256
shasum -a 256 -c Spotuify.dmg.sha256
```

The DMG is built locally with `clients/macos/scripts/build-dmg.sh` and attached to the release manually because the app currently needs the macOS 26 SDK. The build script signs and notarizes only when the release machine has a Developer ID identity and `SPOTUIFY_NOTARY_PROFILE` configured; otherwise macOS may ask for the usual first-launch approval.

### Linux (x86_64)

Install the latest prebuilt archive with checksum verification:

```sh
curl -fsSLO https://raw.githubusercontent.com/planetaryescape/spotuify/main/install.sh
bash install.sh
spotuify
```

Grab the `linux-x86_64` tarball from [Releases](https://github.com/planetaryescape/spotuify/releases) and put `spotuify` on your `PATH`:

```sh
tar xzf spotuify-v*-linux-x86_64.tar.gz
install -Dm755 spotuify ~/.local/bin/spotuify
spotuify daemon install-service       # registers a systemd --user unit
spotuify
```

Spotify auth files live under the private app config directory on every platform. On Unix, `spotuify` writes the auth directory with mode `0700` and auth files with mode `0600`.

### Linux (other arch / distro) or from source

```sh
cargo install --git https://github.com/planetaryescape/spotuify --locked spotuify
spotuify daemon install-service
```

### Windows

Download `spotuify-v*-windows-x86_64.zip` from [Releases](https://github.com/planetaryescape/spotuify/releases), unzip it, and put `spotuify.exe` on your `PATH`:

```sh
spotuify.exe --help
spotuify daemon install-service          # registers a Task Scheduler logon trigger
```

Windows x64 binaries are beta until login, daemon startup, playback, and Task Scheduler install are verified on a real Windows machine. Source installs still work:

```sh
cargo install --git https://github.com/planetaryescape/spotuify --locked spotuify
```

Daemon-mode media-key handling on Windows is currently limited: SMTC requires a foreground window handle, so background-only operation cannot register media keys. Workaround: keep the TUI process alive.

### Nix

```sh
nix run github:planetaryescape/spotuify
# or in a flake:
inputs.spotuify.url = "github:planetaryescape/spotuify";
```

### From source (any platform)

Plain first-time `brew install spotuify` only works after `brew tap planetaryescape/spotuify`. Without that tap, Homebrew searches `homebrew/core`, where `spotuify` is not published. Homebrew's tap-trust checks can also ignore third-party taps unless the formula is trusted; use `brew trust --formula planetaryescape/spotuify/spotuify` after tapping.

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
2. Release Please opens or updates a release PR with the next version, `CHANGELOG.md`, `.release-please-manifest.json`, and `Cargo.toml`.
3. The release-lockfile workflow runs on that release PR and commits `Cargo.lock` if `cargo update --workspace` changes the workspace package versions.
4. Merge the Release Please PR once CI is green.
5. Release Please creates the GitHub release and tag.
6. The tag-driven release workflow builds Linux x86_64, macOS arm64, macOS Intel, and Windows x64 binaries, uploads them to the GitHub release, generates a Homebrew formula, and updates the tap.
7. The macOS app DMG is a separate local artifact: build it with `clients/macos/scripts/build-dmg.sh`, then attach `Spotuify.dmg`, `Spotuify.dmg.sha256`, `Spotuify-<version>.dmg`, and `Spotuify-<version>.dmg.sha256` to the same release.

Required GitHub setup:

```text
Secret: RELEASE_PLEASE_TOKEN
Secret: HOMEBREW_TAP_TOKEN
```

`RELEASE_PLEASE_TOKEN` should be a PAT with `contents:write` and workflow permission so Release Please tags trigger the downstream release workflow. The workflows fall back to `GITHUB_TOKEN`, but tags created with the default token do not trigger other workflows.

`HOMEBREW_TAP_TOKEN` must be a token with write access to the Homebrew tap repository:

```text
planetaryescape/homebrew-spotuify
```

The tap repo should contain a `Formula/` directory. The release workflow will create or update `Formula/spotuify.rb`.

Manual release rebuild:

```text
GitHub Actions -> Release -> Run workflow -> select an existing v* tag
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
2. Asks you to add a Spotify `client_id` from your Spotify Developer app.
3. Opens your browser to log in to Spotify.
4. Stores the resulting OAuth token under `<config_dir>/auth/token.json`.
5. The daemon refreshes the access token as needed and starts syncing.

Use redirect URI `http://127.0.0.1:8888/callback` in the Spotify dashboard. A client secret is optional for PKCE. Premium is required for playback.

This is the BYO Spotify app GA path, not broad consumer no-developer setup. Apply for Extended Quota Mode in the Spotify dashboard if playlist/library writes return `403`.

After setup succeeds, plain `spotuify` opens the TUI directly on later runs:

```sh
spotuify
```

## Logging in

After config is present, plain `spotuify` and `spotuify onboard` open the browser for you. To rerun the login by hand:

```sh
spotuify login
```

That opens Spotify in your browser, and once you approve, spotuify stores the OAuth token under `<config_dir>/auth/token.json`.

### Spotify app credentials

The default auth path uses your own Spotify Developer app. Create one at https://developer.spotify.com/dashboard with redirect URI `http://127.0.0.1:8888/callback`, then set `client_id` in config or export `SPOTUIFY_CLIENT_ID`:

```sh
export SPOTUIFY_CLIENT_ID=your-app-client-id
spotuify login
```

Apps in Spotify's Development Mode can be limited by Spotify policy. Apply for Extended Quota Mode if writes such as playlist creation or library saves return `403`.

## Configuration

Default config path:

```text
~/.config/spotuify/spotuify.toml
```

Default config template:

```toml
# spotuify config
# Copy your Spotify app credentials from https://developer.spotify.com/dashboard.
# Apply for Extended Quota Mode on the dashboard to lift the dev-app
# 25-user cap and unlock playlist/library writes.
client_id = ""
# Optional, only for your own Spotify app. Prefer SPOTUIFY_CLIENT_SECRET
# when you do not want a client secret written to disk.
# client_secret = ""
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

[analytics]
# Optional defaults for historical Last.fm import.
# lastfm_api_key = ""
# lastfm_user = ""
```

Supported environment overrides:

```sh
SPOTUIFY_CONFIG=/path/to/spotuify.toml
SPOTUIFY_CLIENT_ID=...
SPOTUIFY_CLIENT_SECRET=...
SPOTUIFY_REDIRECT_URI=http://127.0.0.1:8888/callback
SPOTUIFY_LASTFM_API_KEY=...
SPOTUIFY_LASTFM_USER=...
```

Config commands:

```sh
spotuify config path
spotuify config init
spotuify config get client_id
spotuify config get redirect_uri
spotuify config get player.backend
# Prints <redacted> unless you pass --reveal-secret.
spotuify config get client_secret
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

`analytics.hook_command` is executed by the shell exactly as configured. Track data is passed through `SPOTUIFY_*` environment variables, not interpolated into the command string.

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
spotuify analytics import lastfm --user your-lastfm-user --from 2024-01-01 --format json
spotuify analytics import lastfm --user your-lastfm-user --from 2024-01-01 --apply --format json
spotuify analytics import unresolved 018f... --format json
spotuify analytics import undo 018f... --dry-run
spotuify lyrics show --track spotify:track:...
spotuify lyrics follow --lines 3
spotuify refresh-media
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
- `spotuify onboard` runs the same setup flow intentionally: a browser login, then the first sync.
- `spotuify login` opens the browser to log in and stores the OAuth token under `<config_dir>/auth/token.json`.
- `spotuify logout` removes the stored auth files.
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
1                 home
2                 search
3                 library
4                 playlists
5                 queue
6                 devices
7                 diagnostics
8                 lyrics
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
U                 refresh current cover art and lyrics
```

Playback keys:

```text
Space             play or pause
Space             when idle/ended, play the selected Home, Search, Library, or Playlist item
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
Enter             play selected home, search, library, or playlist item
e                 add selected item to queue
A or a            add current track or episode to selected playlist
```

Playlist keys:

```text
Enter             open selected playlist
Enter             play selected playlist or selected playlist track
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

After you log in, the daemon mints a Web API token from your session and pulls your Spotify state into the local cache:

- Current playback state.
- Visible Spotify Connect devices.
- Queue state.
- User playlists.

Run `spotuify doctor` any time to confirm auth, daemon, device visibility, and Spotify API access are working.

## Import Last.fm history

If you already scrobble to Last.fm, `spotuify` can backfill that history into local analytics. A scrobble is one timestamped listen. Last.fm stores those listens, but it does not store the full playback timeline, so imported listens are marked `lastfm_scrobble_import` and use an estimated audible duration.

Preview first:

```sh
export SPOTUIFY_LASTFM_API_KEY=lastfm-api-key
spotuify analytics import lastfm --user your-lastfm-user --from 2024-01-01 --format json
```

Apply after the counts look right:

```sh
spotuify analytics import lastfm --user your-lastfm-user --from 2024-01-01 --apply --format json
spotuify analytics import unresolved 018f... --format json
spotuify analytics import undo 018f... --dry-run
```

Raw Last.fm rows stay in the local audit table. Undo removes promoted `listen_facts` and preserves the raw scrobble history.

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

Tools exposed (37):

- Read: `search`, `now_playing`, `devices_list`, `queue_show`, `playlists_list`, `playlist_tracks`, `library_list`, `playlist_plan`, `playlist_resolve_tracks`
- Transport: `play`, `play_uri`, `pause`, `resume`, `next`, `previous`, `seek`, `volume`, `shuffle`, `repeat`, `radio_start`
- Destructive (require `confirm: true`): `queue_add`, `transfer_device`, `playlist_create`, `playlist_add`, `playlist_remove`, `playlist_unfollow`, `playlist_set_image`, `library_save`, `library_unsave`
- Discovery: `lyrics`, `related_artists`
- Analytics: `analytics_top`, `analytics_habits`, `analytics_search`, `analytics_rediscovery`
- Read: `search`, `now_playing`, `devices_list`, `queue_show`, `playlists_list`, `playlist_tracks`, `library_list`
- Transport: `play`, `play_uri`, `pause`, `resume`, `next`, `previous`, `seek`, `volume`, `shuffle`, `repeat`
- Destructive (require `confirm: true`): `queue_add`, `transfer_device`, `playlist_create`, `playlist_add`, `playlist_remove`, `library_save`, `library_unsave`
- Lyrics: `lyrics`
- Analytics: `analytics_top`, `analytics_habits`, `analytics_search`, `analytics_rediscovery`, `analytics_import_lastfm`, `analytics_import_status`, `analytics_import_unresolved`, `analytics_import_undo`
- Ops: `ops_log`, `undo_last` (`undo_last` is the safety net)

Resources (5):

- `spotuify://playback` — current playback state
- `spotuify://devices` — visible Spotify Connect devices
- `spotuify://playlists` — user playlists
- `spotuify://now_playing/lyrics` — synced lyrics for the current track
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

Not logged in:

```text
not logged in; run `spotuify login`
```

Fix:

```sh
spotuify login
```

That opens the browser login. Plain `spotuify` does the same automatically after config is present. If playlist writes return `403`, the Spotify app is probably still in Development Mode; apply for Extended Quota Mode in the Spotify dashboard.

No token or expired token:

```sh
spotuify
```

That opens the browser login automatically when no token is stored. Use `spotuify login` if you only want to re-run the login without the full setup flow.

No devices visible:

```sh
spotuify doctor
spotuify devices --format json
```

Then check that the embedded librespot device is visible. It registers at daemon start under `player.device_name` (`spotuify-hume` on this machine). Premium account required.

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

If librespot cannot bind an audio backend, the daemon reports a player failure/degraded state through doctor/status and logs. Rebuild with one of `--features alsa-backend / pipewire-backend / rodio-backend / portaudio-backend` for your platform.

## Security Notes

- The default dev-app OAuth token is stored under `<config_dir>/auth/token.json` with mode `0600` on Unix. It is guarded by `<config_dir>/auth/token.lock` so daemon and CLI refreshes do not race.
- First-party/keymaster auth is experimental and opt-in via `SPOTUIFY_USE_FIRST_PARTY=1`; that path stores only refresh token + scopes under `<config_dir>/auth/first-party.json`.
- `spotuify logout` removes the stored auth files.
- To re-authenticate, run `spotuify login` again.
- `spotuify auth bearer` and `spotuify config get client_secret` require `--reveal-secret` before printing secrets.
- Config files are written with mode `0600` on Unix. Prefer `SPOTUIFY_CLIENT_SECRET` when you do not want a client secret written to disk.

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

Before calling a release GA-ready, run the live opt-in gate against the exact
binary you plan to ship:

```sh
SPOTUIFY_BIN=spotuify scripts/ga-live-smoke.sh
SPOTUIFY_ALLOW_PROD_INSTANCE_FROM_TARGET=1 SPOTUIFY_INSTANCE=spotuify SPOTUIFY_BIN=./target/release/spotuify scripts/ga-live-smoke.sh
SPOTUIFY_GA_LIVE_PLAYBACK=1 SPOTUIFY_BIN=spotuify scripts/ga-live-smoke.sh
SPOTUIFY_GA_LIVE_PLAYLIST=1 SPOTUIFY_BIN=spotuify scripts/ga-live-smoke.sh
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
