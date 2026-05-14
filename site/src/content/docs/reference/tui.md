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
| `1` | Player | now playing, progress, device, queue preview |
| `2` | Search | global music search |
| `3` | Library | cached library |
| `4` | Playlists | playlists and tracks |
| `5` | Queue | current queue |
| `6` | Devices | Spotify Connect devices |
| `7` | Diagnostics | daemon, auth, cache, logs |

## Command palette

```text
Ctrl-p
```

The palette filters actions by the current context. Disabled actions should explain why.

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

## Diagnostics

If the TUI looks wrong, check the daemon from another terminal:

```bash
spotuify doctor
spotuify daemon status
spotuify logs tail 200
```

## See Also

- [Keybindings](/reference/keybindings/)
- [Player and Daemon](/guides/player-and-daemon/)
- [Troubleshooting](/reference/troubleshooting/)
