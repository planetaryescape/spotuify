# Phase 4 - TUI Redesign

## Goal

Replace the current basic TUI with a player-first, daemon-backed, mxr-style user experience.

## Deliverables

- Top tabs: Player, Search, Library, Playlists, Queue, Devices, Diagnostics.
- Shared action registry.
- Contextual hint bar.
- Command palette.
- Searchable help.
- Multi-select model.
- Bulk queue/like/add actions.
- Rich empty/loading/error states.
- Diagnostics page.

## Implementation order

1. Add action registry independent of rendering.
2. Replace hardcoded hint text with registry-driven hints.
3. Add command palette over registry.
4. Split player/search/library/playlists/queue/devices/diagnostics state.
5. Add current-list filter separate from global search.
6. Add multi-select.
7. Add diagnostics page.
8. Add large/small player modes.

## UX rules

- Text input captures keys before global actions.
- Hidden panes are not focusable.
- Hint bar shows no more than five actions.
- Command palette hides irrelevant actions.
- Empty states teach next action.
- Blocking errors use modal; transient status uses status bar.

## Verification

- TUI tests drive action dispatch without Spotify network.
- Context changes produce expected top hints.
- Text input does not trigger global actions.
- Multi-select actions dispatch correct daemon requests.

## Definition of done

The TUI feels like a reliable music player and controller, not a debug panel around Spotify API calls.
