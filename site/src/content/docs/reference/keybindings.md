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
| `s` | `spotuify shuffle toggle` |
| `r` | `spotuify repeat context` |

```bash
spotuify toggle
spotuify next
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
| `a` | add selected/current to playlist |
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

```bash
spotuify doctor
```

## See Also

- [TUI](/reference/tui/)
- [CLI Concepts](/reference/cli/concepts/)
- [Terminal Control](/guides/terminal-control/)
