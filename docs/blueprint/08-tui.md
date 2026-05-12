# spotuify - TUI

## Principle

The TUI is not the app. It is a high-bandwidth human controller for the daemon.

It should be beautiful enough to enjoy, but reliability and clarity beat decoration.

## Target top-level tabs

```text
1 Player | 2 Search | 3 Library | 4 Playlists | 5 Queue | 6 Devices | 7 Diagnostics
```

## Layout modes

### Player first

Default view should privilege playback:

- current track
- artist/album/context
- album art
- progress
- device
- queue preview
- key hints

### Search view

- search input
- type/source filters
- grouped results
- preview panel
- multi-select state

### Playlist view

- playlist list
- playlist detail
- track list
- selected/multi-selected actions

### Diagnostics view

- daemon health
- auth status
- device visibility
- recent API errors
- recent actions
- log path
- sync/index state

## Keyboard model

Navigation:

```text
j / Down        move down
k / Up          move up
G               bottom
Ctrl-d          half page down
Ctrl-u          half page up
Tab             next pane or filter chip
Shift-Tab       previous pane or filter chip
Esc / b         back or cancel
q               quit TUI only
```

Global actions:

```text
Space           play/pause
n               next
p               previous
Left / Right    seek
s               shuffle
r               repeat cycle
/               global search
Ctrl-f          filter current list
Ctrl-p          command palette
?               contextual help
u               refresh current view
```

Selection actions:

```text
m               mark/unmark item
M               mark range
l               like selected/current
Enter           play/open selected
```

## Action registry

Every action has:

- action ID
- label
- shortcut
- contexts
- enabled predicate
- disabled reason
- CLI equivalent when available

The hint bar, command palette, help modal, and tests should all use this registry.

## Contextual hints

Hint bar shows at most five actions, sorted by relevance.

Examples:

| Context | Hints |
|---|---|
| Player | `Space Play/Pause`, `n Next`, `p Prev`, `s Shuffle`, `r Repeat` |
| Search input | `Enter Search`, `Tab Type`, `Esc Cancel`, `Ctrl-f Filter` |
| Search results | `Enter Play`, `m Mark`, `e Queue`, `l Like`, `a Add` |
| Multi-select | `e Queue Selected`, `l Like Selected`, `a Add Selected`, `Esc Clear` |
| Devices | `Enter Transfer`, `u Refresh`, `d Details` |

## Command palette

`Ctrl-p` opens a searchable command palette:

- only valid commands for current context are shown
- disabled commands explain why
- commands include shortcut and CLI equivalent
- recent commands rank higher
- categories: player, search, library, playlist, queue, device, diagnostics

## Help

`?` opens searchable help. It should start with tasks:

- How do I play a playlist?
- How do I search only liked tracks?
- How do I queue multiple tracks?
- How do I fix no active device?

Raw keymaps are secondary.

## Empty and loading states

No blank panels.

Every empty state should answer:

- What is happening?
- What can I press next?
- Is this empty because no data exists, sync is loading, or an error happened?

Examples:

- `No playlist selected. Press Enter to open a playlist.`
- `Searching Spotify... local results shown while remote refresh runs.`
- `No active device. Press 6 for Devices or run spotuify devices.`
