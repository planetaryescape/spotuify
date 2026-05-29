# Phase 4 - TUI Redesign

## Goal

Replace the current basic TUI with a player-first, daemon-backed, mxr-style user experience.

## Deliverables

- Shell layout: main content, persistent bottom player, optional right rail.
- Top tabs: Home, Search, Library, Playlists, Queue, Devices, Diagnostics.
- Shared action registry.
- Contextual hint bar.
- Command palette.
- Searchable help.
- Multi-select model.
- Bulk queue/like/add actions.
- Rich empty/loading/error states.
- Diagnostics page.
- Queue, lyrics, and keymap right-rail panels available from any screen.
- Grouped search rendering by media kind.
- Playlist picker modal for add-to-playlist.
- Fullscreen queue and lyrics overlays.
- Append-only queue expansion for playlist and album selections.
- Mouse hitboxes for tabs, rows, progress seeking, right-rail controls,
  bottom player transport, and volume scroll.
- Filterable/scrollable diagnostics logs.

## Implementation order

1. [x] Add action registry independent of rendering.
2. [x] Replace hardcoded hint text with registry-driven hints.
3. [x] Add command palette over registry.
4. [x] Split player/search/library/playlists/queue/devices/diagnostics state.
5. [x] Add current-list filter separate from global search.
6. [x] Add multi-select.
7. [x] Add diagnostics page.
8. [x] Add large/small player modes.
9. [x] Add persistent bottom player plus queue/lyrics/hints rail toggles.
10. [x] Render search results in media-kind groups.
11. [x] Add explicit playlist-picker modal for add-to-playlist.
12. [x] Add full-screen lyrics and queue overlays.
13. [x] Add mouse hitboxes for player controls, tabs, rows, progress, and rails.
14. [x] Expand queueing of playlist/album selections into append-only track batches.
15. [x] Make diagnostics logs filterable and keyboard-scrollable.

## UX rules

- Text input captures keys before global actions.
- Hidden panes are not focusable.
- Hint bar shows no more than five actions.
- Command palette hides irrelevant actions.
- Empty states teach next action.
- Blocking errors use modal; transient status uses status bar.
- Queue and lyrics must be viewable without leaving the current screen.
- Home must be actionable on cold start: saved music/podcasts or recent plays
  are selectable, and the queue is a secondary panel when live.
- Library and diagnostics load automatically; empty states must not instruct
  users to run manual sync for normal startup.
- Diagnostics logs use the normal list filter and movement keys.

## Verification

- TUI tests drive action dispatch without Spotify network.
- Context changes produce expected top hints.
- Text input does not trigger global actions.
- Multi-select actions dispatch correct daemon requests.
- `spotuify-tui` tests cover action registry completeness, command-palette
  filtering/selection, current-list filtering, and multi-select queue/add
  request construction.
- Render tests cover bottom-player placement, right-rail queue visibility,
  grouped search panels, and automatic-load empty-state copy.
- Input tests cover playlist-picker dispatch, fullscreen rail toggles, mouse
  tab switching, row selection, progress seek mapping, rail controls, bottom
  player play/pause and volume mapping, diagnostics log filtering, and queue
  expansion request construction.

## Definition of done

The TUI feels like a reliable music player and controller, not a debug
panel around Spotify API calls. Current implementation has the shell,
rail, visualizer placement, grouped search, playlist picker, fullscreen
rail overlays, append-only playlist/album queue expansion, diagnostics log
filtering, and mouse controls for tabs, rows, progress, rails, transport,
and volume.
