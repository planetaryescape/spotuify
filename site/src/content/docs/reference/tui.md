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
nothing is currently playing or the current item has ended, Space starts the
selected Home item. The same idle/ended rule applies to selected Search,
Library, and Playlist rows.

The player bar stays visible at the bottom. Use `z` to switch player size,
`L` to show or hide lyrics on the right, `Q` to show or hide the queue on the
right, and `F` to expand the active rail to fullscreen.

The Lyrics screen and rail auto-scroll like a teleprompter: the active line
stays centered and the rest scrolls past it, so you read from the middle of
the pane, not the bottom.

Press `U` while a track is playing to refetch current cover art and lyrics.
The existing media stays visible until the replacement fetch returns.

Search and Library selection previews show artwork for albums, playlists,
shows, and episodes when Spotify returns an image URL.

Press `Enter` on an artist (from Search or the Library Artists view) to open the
discography overlay. Releases group into Albums, Singles & EPs, Compilations,
and Appears On on the left; the focused album's tracks show on the right. Press
`L` to toggle between all releases and only those in your library, `Tab` to swap
panes, `Enter` to play, and `Esc` to close. See
[Keybindings](/reference/keybindings/).

Press `O` to choose which local audio output the embedded player renders to
(see [Keybindings](/reference/keybindings/)).

Press `Delete` for the destructive action in the current context. On the
playlist list it unfollows the selected playlist. In Liked Songs detail it
unsaves the marked or selected tracks. In a loaded playlist detail it removes
the marked or selected exact occurrences, so duplicate tracks can be handled
independently.

Every path goes through a `y`/`n` confirmation. Playlist-detail confirmation
freezes the selected rows, and the TUI waits for daemon-owned state instead of
deleting rows locally. Exact occurrence removal has position-aware
`spotuify ops undo` support when the provider supplies playlist version tokens.
Playlist unfollow is not reversible.

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
