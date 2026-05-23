---
title: "Keybindings"
description: "Document TUI navigation, playback, search, selection, and help keys."
---

Keybindings come from the TUI action registry. When a key has a CLI equivalent, the help text should show it.

## Navigation

| Key | Action |
| --- | --- |
| `1` | Player |
| `2` | Search |
| `3` | Library |
| `4` | Playlists |
| `5` | Queue |
| `6` | Devices |
| `7` | Diagnostics |
| `8` | Lyrics |
| `Q` | show/hide queue rail |
| `L` | show/hide lyrics rail |
| `H` | show/hide contextual hints rail |
| `F` | expand/collapse active queue or lyrics rail |
| `j` / Down | move down |
| `k` / Up | move up |
| `Ctrl-d` | half page down |
| `Ctrl-u` | half page up |
| `b` / Esc | back or cancel |
| `q` | quit TUI |

```bash
spotuify
```

## Playback

| Key | CLI equivalent |
| --- | --- |
| Space | `spotuify toggle` |
| `n` | `spotuify next` |
| `p` | `spotuify previous` |
| Left | `spotuify seek -15s` |
| Right | `spotuify seek +15s` |
| `+` / `=` | `spotuify volume +5` |
| `-` | `spotuify volume -5` |
| `s` | `spotuify shuffle toggle` |
| `r` | `spotuify repeat context` |
| `z` | switch compact/large player |
| `v` | toggle visualizer |
| `V` | cycle visualizer source |
| `O` | choose local audio output device (`spotuify audio-output NAME`) |

```bash
spotuify toggle
spotuify next
```

`O` opens a picker of the Mac audio outputs the embedded `spotuify-hume` player can render to; selecting one writes `player.audio_output_device` and restarts the player. The CLI equivalent:

```bash
spotuify audio-outputs                          # list outputs
spotuify audio-output "MacBook Pro Speakers"    # set + reconnect
```

## Search and filters

| Key | Action |
| --- | --- |
| `/` | global search |
| `Enter` | submit search or play/open selected |
| `Ctrl-f` | filter current list |
| `Esc` | cancel input |

```bash
spotuify search "luther vandross"
```

## Selection

| Key | Action |
| --- | --- |
| `m` | mark or unmark item |
| `M` | mark range |
| `e` | queue selected |
| `l` | like selected/current |
| `a` / `A` | open playlist picker for selected/current |
| `x` / Enter on devices | transfer playback |

```bash
spotuify queue add spotify:track:...
spotuify playlist add "Coding" spotify:track:... --dry-run
```

## Help and palette

| Key | Action |
| --- | --- |
| `?` | searchable help |
| `Ctrl-p` | command palette |
| `u` | refresh current view |
| `u` on Diagnostics | undo last reversible operation |
| `R` (when the update banner shows) | restart the daemon onto a freshly-installed build |

```bash
spotuify doctor
```

## Diagnostics

| Key | Action |
| --- | --- |
| `Ctrl-f` | filter recent logs |
| `j` / Down | scroll log matches |
| `k` / Up | scroll log matches |

```bash
spotuify logs tail 200
```

## See Also

- [TUI](/reference/tui/)
- [CLI Concepts](/reference/cli/concepts/)
- [Terminal Control](/guides/terminal-control/)
