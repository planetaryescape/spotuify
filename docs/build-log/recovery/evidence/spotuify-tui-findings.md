# Search needs one readable list and playlist playback must honor the cursor

Baseline: freshly fetched `origin/main` at `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e`, worktree `/tmp/spotuify-recovery-clones/spotuify`. Files changed: `crates/spotuify-tui/src/app.rs`, `crates/spotuify-tui/src/ui.rs`. No commits.

## Verified defects and corrections

| Finding | Baseline evidence | Correction |
|---|---|---|
| Search divides even tiny windows into several cards. At up to three categories it always uses one horizontal row, and at six it uses three columns over two rows below 144 columns. No minimum width is enforced despite the comment claiming 22. | `ui.rs:3668-3701` | One full-width active-category list. Text type tabs wrap when there is room; short windows show the active type and `Tab types` shortcut. Tab/backtab and existing g-kind shortcuts retain their meaning. Mouse type tabs select the correct global visible index. |
| Long album/context text reserves its entire width before the artist is allocated room, so the artist can disappear. | `ui.rs:3879-3891` | Bound the context suffix to half the available detail row; keep the artist beside it. |
| g-kind indexes raw results even after sorting/filtering changes the selection space. | `app.rs:6212-6218`; visible-space definition `app.rs:2036-2105` | Resolve against `visible_items()` exactly as Tab and mouse already do. |
| Enter on an opened normal playlist starts the whole playlist and ignores the chosen track. | `app.rs:7244-7254` | Build a contextual PlayUri request for the selected visible track with playlist URI as context; retain Liked Songs context handling; keep invalid playlist-reference errors. |
| Search playlist rows advertise a generic Queue Selected hint while e targets the whole playlist. | `app.rs:6306`, `app.rs:2342-2346`, `ui.rs:5124-5132`; playlist-list contextual label existed in `tui_actions.rs:978-985` | Search playlist hint now says Queue playlist. e still queues; it was not remapped to browse. |

The user's playlist e failure is a daemon/provider access failure investigated by the runtime agent. This TUI patch does not claim to fix provider access.

## Verification

- `CARGO_TARGET_DIR=target-cli CARGO_BUILD_JOBS=4 cargo nextest run -p spotuify-tui --no-fail-fast`: 336/337 passed; sole failing test asserted the discarded grid's simultaneous card contents. `/tmp/spotuify-tui-tests.log`.
- Updated the grid expectation to verify one list with type tabs, and selecting each type reveals its list.
- Focused follow-up: seven tests passed, including category tabs/mouse, g-kind, playlist contextual request, and search text renders at 40x24, 60x24, 80x24, 140x32. `/tmp/spotuify-tui-captures.log`.
- Captures: `/tmp/spotuify-search-{40x24,60x24,80x24,140x32}.txt`. These are actual TestBackend renders, not HTML facsimiles. After capturing, only the duplicated slash hint and lowercase category heading were improved; root's canonical test run covers final text and can recapture from the new test binary.
- `rustfmt --edition 2021` on the two files; `git diff --check -- crates/spotuify-tui` passed.
- Initial compile found a missing qualification of TuiAction in the hint renderer; corrected before the whole suite above.
- No live playback changes made by this worker. The contextual playlist request regression verifies the TUI request shape, not upstream Spotify outcome. Root owns CLI/daemon integration gates.

## Research supports one focus and explicit actions

Primary upstream references reviewed on 2026-10-26:

- https://github.com/aome510/spotify-player/blob/master/README.md#commands documents selection, contextual actions, queueing, device choice and search as distinct actions. The Actions section limits the menu to actions available for the selected item. The Search Page section documents input focus then result-window focus. Adopt explicit state/action naming and selection-scoped menus. Do not copy its multipane geometry as the answer to BK's narrow-window complaint.
- https://ratatui.rs/examples/widgets/tabs/ demonstrates one selected tab and a single content area. The built-in Tabs widget is horizontal; our few lines of wrapping text and existing hit-map registration handle the required width adaptation. This is a layout adaptation, not a new state owner or navigation framework.
- https://github.com/aome510/spotify-player/issues/1000 is a concrete upstream report of filtering trapping keyboard input and preventing actions. It is a reported failure, not proof that all versions behave this way. Keep spotuify's existing Esc/Enter exit from filter input and verify shortcut behavior against filtered selection.

## Further design work stays in the blueprint

- Baseline `app.rs` has 15,087 lines and `ui.rs` has 7,652, verified with `git show origin/main:<path> | wc -l`. Propose future splits by job (search, devices, playlist interactions, rendering) only in a separately authorized structural change; do not mix a verbatim move with these behavior corrections.
- Retained semantics: up/down traverse the existing global visible result order and can cross types; Tab jumps to the first row of the next available type. Blueprint should choose whether list movement stays within a type in the next UX rung instead of silently changing that contract here.
- Compact search preserves readable title and artist. The persistent player still occupies substantial vertical space because the current global chrome contract is unchanged; compact-player design belongs in the next UX rung.
- Search result playlist Enter still plays a playlist; it does not open a browse view. Artist Enter opens a view. A future contextual Open action needs a consistent contract across album, playlist and show, with CLI parity and a preserved back path. This is not what e currently means.
- Partial search errors on categories with no results are only visible through the existing error toast; these categories have no selectable tab. Better category loading/error placeholders need a separate small state design; do not claim this patch implements it.
- Ratatui text rendering was read directly. Human theme, terminal font/image protocol and subjective TUI review remain distinct from these cell-level checks.

## Cell-width follow-up from review

Only `render_media_rows` now uses `truncate_media_row`, a local helper using Ratatui 0.30's `Span::width()` and `styled_graphemes()` rather than codepoint counts. Subtitle/suffix budgets count terminal cells and do not split combining sequences. This small helper is needed because the file's existing `truncate` counts codepoints and changing that global helper would affect unrelated screens. No dependency or lock changes.

Added `search_rows_preserve_cjk_artists_and_combining_graphemes`: renders a real 32-cell row with 宇多田ヒカル, CJK album text, and decomposed Café; asserts actual buffer positions for the full artist, album start, combined e-acute, ellipsis, and right margin. `media_row_truncation_uses_cells_and_never_splits_graphemes` covers zero-cell output, too-small wide-character budgets, and combining clusters. Root owns running these through the queued canonical compile. rustfmt and whitespace checks passed; no new Cargo command launched by this worker.

## CLI parity follow-up

The root's fake CLI run reproduced a second defect: queue Add rejected album/playlist kinds before dispatch although the existing daemon expands both. Added album and playlist to the queue target resolver in `crates/spotuify-cli/src/commands.rs`. Queueing still requires transport queue_add; album collections additionally require album_tracks and playlists require item_read, with operation-specific unsupported errors. `--many` remains a single aggregate request; daemon batch-provider validation and collection expansion remain unchanged (`handlers/playback.rs` QueueAddMany path).

Added `spotuify play-uri URI --context CONTEXT` in `src/main.rs`, routing the existing protocol context_uri. New public `ipc_play_uri_with_context` preserves the existing public `ipc_play_uri` wrapper. Context references use the existing resolver and permit album, artist, playlist and show. Resolved context kinds are checked again after resolution to protect compatibility fallbacks; context and target schemes must match; playback is gated by the resource owner. The exact protocol sentinel `spotuify:collection:liked` is passed through as the provider-scoped Liked Songs context. No new protocol variant or provider state.

Added parser coverage and four CLI request-handler tests covering successful context requests (all four context kinds and Liked Songs), context wrong-kind/provider mismatch before mutation, single and aggregate album/playlist queue dispatch, and collection capability denial plus artist/show queue rejection. No Cargo launched because root's canonical checks own the shared target directory. Root must run these tests, rebuild, then verify real fake-provider CLI queue/context workflows before marking the parity gap complete. rustfmt and diff whitespace checks passed.

## Navigation follow-up supersedes the retained global traversal assumption

Independent review found that the single active-kind renderer made the retained global up/down traversal misleading: sorted interleaving could select a hidden artist between two displayed tracks. Corrected `App` main-search movement through an active-kind index map over the same filtered/sorted `visible_items()` used by the renderer. j/k, pages and list boundaries stay inside that type. Main Search now accepts Home/End as top/bottom aliases; Tab, g-kind and type-tab clicks explicitly switch types. Fullscreen panels and modal/overlay navigation bypass this mapping.

Added `search_list_navigation_stays_in_the_drawn_kind_after_sorting_and_filtering`: fourteen interleaved artist/track pairs sorted by name plus filtered-out content; checks j/k, clamped boundaries, page offsets, Home/End, exact subsequent play target and Enter action, Tab/g switches, and independent fullscreen queue navigation with mixed kinds. No Cargo run; root owns the final combined compilation. app.rs frozen after rustfmt and whitespace checks.
