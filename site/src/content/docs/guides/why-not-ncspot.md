---
title: "Why not just use ncspot?"
description: "See what spotuify learned from ncspot and where each player fits."
---

If you want a focused Spotify player in your terminal, use [`ncspot`](https://github.com/hrkfdn/ncspot). It may be the better fit.

`ncspot` is mature, small, and widely packaged. Years of Spotify protocol changes, audio problems, and platform quirks have shaped it. It proved that a native terminal Spotify player could be good enough to use every day.

`spotuify` builds on that work. We wanted to keep the terminal player, then move playback and state behind it so a shell command, an agent, or another app could use the same running system.

## What we learned from ncspot

Some of `spotuify`'s best ideas came from studying `ncspot`:

| What `ncspot` demonstrated | How `spotuify` carries it forward |
| --- | --- |
| Embed librespot instead of depending on the official desktop client | The daemon embeds librespot and registers as a Spotify Connect device |
| Drive playback from player events | Every client receives daemon-owned playback events instead of inventing its own state |
| Preload the next track for gapless playback | The embedded player keeps the same preload path |
| Derive position from a playback clock | The daemon shares one clock across the TUI, CLI, MCP, and menubar clients |
| Use playlist `snapshot_id` values to avoid unnecessary refetches | Sync uses them as freshness gates and safe-mutation context |
| Provide `reload` and `reconnect` escape hatches | Both are first-class `spotuify` commands |
| Put IPC sockets in the runtime directory | `spotuify` follows the same operational lesson |
| Restore the terminal and write diagnostics after a crash | `spotuify` keeps file-backed logs, panic handling, `doctor`, and bug reports |

We studied these patterns, credited them, and reused them. The detailed notes are in [Research Notes](/guides/research/).

## The fork in the road

The projects put the application boundary in different places.

```text
ncspot

terminal UI + player + library + command socket
                    one process
```

`ncspot` is one terminal application. Its UI, playback engine, queue, and library live together. A Unix socket lets scripts control the running process and receive now-playing updates.

```text
spotuify

TUI · CLI · MCP · menubar
           │
           ▼
        daemon
           │
           ├── embedded player
           ├── SQLite
           └── Tantivy
```

`spotuify` puts the player and durable state in a daemon. The TUI is a client. Close it and the track keeps playing. The CLI, MCP server, and menubar app reconnect to the same queue, playback clock, library, and search index.

This costs more code, more dependencies, a larger binary, and another process to look after. It only pays off if you use the other clients.

## Why spotuify took the longer route

The daemon gives each client the same product surface.

- Reusable TUI actions also exist as `spotuify <command>`. Read commands can return table, JSON, JSONL, CSV, or IDs output for `jq`, `fzf`, `xargs`, status bars, and scripts.
- SQLite stores cached metadata and listening history. Tantivy searches that local data without sending every query to Spotify.
- The MCP server gives agents typed music tools. They do not need to scrape a terminal screen.
- Playlist and library writes can show a dry run first. Successful writes go into an operation log, with undo where Spotify permits it.
- The TUI, CLI, MCP server, and macOS app read the same playback state.

For example, the CLI can feed another Unix tool without opening the TUI:

```bash
spotuify search "burial" --format ids | spotuify queue add --ids -
spotuify status --format json | jq -r '.item.name'
```

## Where ncspot is better

`ncspot` has advantages that `spotuify` does not erase:

- It is the smaller system. There is no separate daemon, SQLite database, Tantivy index, MCP server, or native companion app to run.
- It has been around since 2018. `spotuify` cannot manufacture that amount of time in users' terminals.
- It is packaged for more places, including BSDs, Flathub, Snap, Homebrew, Scoop, and WinGet.
- It owns its queue inside the player. You can clear the queue, remove a selected track, or save the queue. `spotuify` does not pretend Spotify's public queue API can remove or reorder entries.
- Its setup is shorter. `spotuify` currently asks you to create a Spotify developer app because its CLI, cache, playlists, and analytics use the Web API.
- It has fewer moving parts. If you only use the TUI, `spotuify`'s extra architecture is work with no payoff.

If `ncspot` already fits your workflow, keep it. We are not trying to talk you out of a program that works.

## Where spotuify goes further

Choose `spotuify` when you want to use Spotify outside one terminal UI:

```bash
spotuify search "burial" --format ids | spotuify queue add --ids -
spotuify status --format json | jq -r '.item.name'
spotuify analytics rediscovery --gap 90d
spotuify ops undo --dry-run
spotuify mcp
```

We also think `spotuify` is prettier. That is subjective. The TUI leaves room for album art, synced lyrics, contextual actions, and a visualizer. People have specifically told us they like those bouncing bars moving with the music. To us, it feels like a current music app that happens to run in a terminal.

![The spotuify player screen with album art, synced lyrics, queue, playback controls, and the spectrum visualizer.](/spotuify-player.png)

`ncspot` takes its visual cues from ncurses MPD clients. It is sparse, quick, and puts more tracks on screen. Some people will prefer that. `spotuify` spends more screen space and rendering work on the thing playing now. See the [TUI reference](/reference/tui/) for the full layout.

The trade-off is straightforward:

- `ncspot` keeps the player and interface together.
- `spotuify` makes the player a service shared by interfaces.

We did not build `spotuify` because `ncspot` should have been bigger. We built it because we wanted a different boundary around the player. `ncspot` showed us how much of the hard playback work should be done.

## See also

- [Player and Daemon](/guides/player-and-daemon/)
- [Architecture](/guides/architecture/)
- [Terminal Control](/guides/terminal-control/)
- [Agents and MCP](/guides/agents-and-mcp/)
