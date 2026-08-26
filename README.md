# spotuify

Spotify, built for the command line. A daemon owns playback; a keyboard-native
TUI, a pipeable CLI, an MCP server for agents, and a macOS menubar app are all
just clients. Quit any of them and the music keeps playing.

<p align="center"><img src="site/public/spotuify-readme-demo.gif" alt="spotuify CLI demo: local search, play, JSON status through jq, piping search results into the queue, operation log" /></p>
<p align="center"><a href="https://spotuify.app">spotuify.app</a> — docs, guides, and the full TUI demo (album art, live spectrum, synced lyrics)</p>

## Install

```sh
brew tap planetaryescape/spotuify
brew trust --formula planetaryescape/spotuify/spotuify
brew install planetaryescape/spotuify/spotuify
spotuify        # first run kicks off onboarding
```

Linux, Windows, Nix, the macOS .dmg, checksums, and from-source:
[install guide](https://spotuify.app/getting-started/install/).

You need Spotify Premium (librespot streaming requires it) and your own
[Spotify Developer app](https://spotuify.app/getting-started/first-run/) —
onboarding walks you through it. That makes spotuify GA for terminal users
comfortable creating a developer app, not one-click consumer setup. If writes
return `403`, your app is still in Development Mode; request Extended Quota
Mode in the Spotify dashboard.

## What you can do

Paste any of these and it just runs.

```sh
spotuify play "imagine dragons"        # first match plays on the active device
spotuify next                          # toggle · seek +15s · volume 70
```

```sh
# a terminal picker that starts the track you choose
spotuify search "luther vandross" --type track --format ids \
  | fzf | xargs spotuify play-uri
```

```sh
# five search hits queued through the daemon
spotuify search "burial" --type track --limit 5 --format ids \
  | spotuify queue add --ids -
```

```sh
# see the exact playlist write before Spotify is touched; --yes commits
spotuify playlist plan "focus coding" --format json > p.json
spotuify resolve-tracks --from p.json --format jsonl > c.jsonl
spotuify playlist create "Focus" --from c.jsonl --dry-run
```

```sh
# a now-playing string for tmux, SketchyBar, or Waybar
spotuify status --format json | jq -r '.item.name + " — " + .item.subtitle'
```

```sh
# top listens and forgotten favourites, answered from local SQLite
spotuify analytics top --kind artists --since 30d
spotuify analytics rediscovery --gap 90d
```

```sh
# playlist and library writes land in a reversible op log
spotuify ops log --limit 3
spotuify ops undo --dry-run
```

```sh
spotuify mcp    # 41 MCP tools for agents, backed by the daemon playing the audio
```

More recipes: [Terminal Control](https://spotuify.app/guides/terminal-control/) ·
[Recipes](https://spotuify.app/guides/recipes/) ·
[Queue and Playlists](https://spotuify.app/guides/queue-and-playlists/) ·
[Agents and MCP](https://spotuify.app/guides/agents-and-mcp/)

## One daemon, four clients

The daemon embeds librespot and registers as the Spotify Connect device — it
is not remote-controlling some other player, it *is* the player. It also keeps
a SQLite metadata cache and a Tantivy search index. Everything else is a view
over one Unix socket:

```
TUI · CLI · MCP · menubar  ──socket──▶  daemon  ──▶  SQLite + Tantivy
                                          │
                                          ▼
                              Spotify (Web API + Connect)
```

- **`spotuify`** — the TUI: ten screens, album art and a live spectrum in the
  terminal, synced lyrics, vim keys. Quitting it changes nothing about the
  music.
- **`spotuify <command>`** — the contract: if the TUI can do it, the CLI can.
  58 commands, output as table, `json`, `jsonl`, `csv`, or `ids`; fzf, jq, and
  xargs are part of the product.
- **`spotuify mcp`** — tell your agent what you're in the mood for; it runs
  the same commands you do, previews the playlist, and `ops undo` is its
  safety net.
- **menubar** — a native SwiftUI macOS app on the same socket, for the moments
  you want a window. [Download the DMG](https://spotuify.app/getting-started/install/#macos-app-dmg).

## How spotuify differs

| If you want... | `spotuify` chooses... |
|---|---|
| A terminal-first controller for scripts and agents | CLI and MCP surfaces first; the TUI is another client |
| Playback that keeps running after the UI exits | Daemon-backed control through local IPC |
| A local library/search runtime | SQLite cache plus rebuildable search index |
| Maximum desktop integration polish today | Use an official Spotify client or a desktop-first app instead |
| The smallest possible binary with no daemon | `spotuify` is not optimizing for that trade-off |

Honest limits: Spotify's public API has no queue-remove or queue-reorder, so
the queue view doesn't pretend those exist. Playback control needs Premium.

## Docs

- [Quick Start](https://spotuify.app/getting-started/quick-start/) and [First Run](https://spotuify.app/getting-started/first-run/)
- [CLI Reference](https://spotuify.app/reference/cli/) — every command · [JSON Output](https://spotuify.app/reference/json-output/)
- [Configuration](https://spotuify.app/reference/config/) · [Keybindings](https://spotuify.app/reference/keybindings/)
- [Import Last.fm history](https://spotuify.app/guides/import-lastfm-history/) · [Architecture](https://spotuify.app/guides/architecture/)
- [Changelog](https://spotuify.app/changelog/)
- [Troubleshooting](https://spotuify.app/reference/troubleshooting/), or run `spotuify doctor`

## Security

Auth tokens live in private files under the app config directory (`0600` on
Unix, guarded by a lock file; the auth directory is `0700`). `spotuify logout`
removes them. Secrets never print without `--reveal-secret`. Prefer
`SPOTUIFY_CLIENT_SECRET` if you don't want a client secret on disk.

## Development

```sh
cargo fmt --check
scripts/cargo-nextest -p <crate>     # inner loop
scripts/smoke.sh                     # fake-provider smoke, no live API
```

Before calling a release GA-ready, run the live smokes against a real
account: `SPOTUIFY_GA_LIVE_PLAYBACK=1 scripts/ga-live-smoke.sh` (playback
mutations) and `SPOTUIFY_GA_LIVE_PLAYLIST=1 scripts/ga-live-smoke.sh`
(playlist mutations).

See [CONTRIBUTING.md](CONTRIBUTING.md), [ARCHITECTURE.md](ARCHITECTURE.md),
and the blueprint under [docs/](docs/). The README demo regenerates with
`vhs scripts/readme-demo.tape`; the TUI hero video is recorded per
[docs/hero-video-script.md](docs/hero-video-script.md).

## Status

Active and dogfooded daily: real and usable, released often, not finished.
`spotuify` is BYO Spotify app GA: the supported setup is for users who create
their own Spotify Developer app. It is not broad consumer no-developer setup
yet. Apps in Spotify's Development Mode can be limited by Spotify policy;
apply for Extended Quota Mode if playlist or library writes return `403`.
[Releases](https://github.com/planetaryescape/spotuify/releases) ·
[Roadmap](https://spotuify.app/guides/roadmap/)

## License

[MIT](LICENSE)
