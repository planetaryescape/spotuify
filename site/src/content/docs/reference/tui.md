---
title: "TUI"
description: "Document the player-first terminal UI, screens, and diagnostics."
---

The TUI is a high-bandwidth human controller for the daemon. It should feel good, but it must not own durable truth.

## Open it

```bash
spotuify
```

Quit with `q`. Playback continues through the daemon.

## Screens

| Key | Screen | Job |
| --- | --- | --- |
| `1` | Home | saved music, podcasts, and queue preview |
| `2` | Search | global music search |
| `3` | Library | cached library |
| `4` | Playlists | playlists and tracks |
| `5` | Queue | current queue |
| `6` | Devices | Spotify Connect devices |
| `7` | Diagnostics | daemon, auth, cache, logs |
| `8` | Lyrics | synced lyrics |

The Home screen is actionable on startup: it fills from cached saved tracks,
albums, podcasts, recent plays, and the live queue when a session exists. If
nothing is currently playing, Space starts the selected Home item.

The player bar stays visible at the bottom. Use `z` to switch player size,
`L` to show or hide lyrics on the right, `Q` to show or hide the queue on the
right, and `F` to expand the active rail to fullscreen.

The Lyrics screen and rail auto-scroll like a teleprompter: the active line
stays centered and the rest scrolls past it, so you read from the middle of
the pane, not the bottom.

Press `O` to choose which local audio output the embedded player renders to
(see [Keybindings](/reference/keybindings/)).

```bash
spotuify status
```

## Command palette

```text
Ctrl-p
```

The palette filters actions by the current context. Disabled actions should explain why.

```bash
spotuify
```

## Help

```text
?
```

Help starts with tasks, not raw key tables:

```text
How do I play a playlist?
How do I queue multiple tracks?
How do I fix no active device?
```

```bash
spotuify
```

## Diagnostics

Diagnostics loads doctor, cache, operation history, and recent logs
automatically. Use `Ctrl-f` to filter the recent logs and `j`/`k` or the arrow
keys to scroll matches.

If the TUI looks wrong, check the daemon from another terminal:

```bash
spotuify doctor
spotuify daemon status
spotuify logs tail 200
```

## Mouse

Mouse is optional. The keyboard remains the complete control surface. You can
click tabs to switch screens, click rows to select, click the progress bar to
seek, click rail headers to expand or hide them, click the bottom-player
transport to play/pause, and scroll on the bottom player to change volume.

```bash
spotuify
```

## See Also

- [Keybindings](/reference/keybindings/)
- [Player and Daemon](/guides/player-and-daemon/)
- [Troubleshooting](/reference/troubleshooting/)
