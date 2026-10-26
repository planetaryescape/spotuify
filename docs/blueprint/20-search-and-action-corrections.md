# 20: Search should stay readable and actions should follow selection

Apply D-043, D-045, D-046 and D-054. Keep the existing action registry and daemon requests. Navigation is client-local; the daemon remains the owner of search results and mutations.

## Show one category at a time

Replace the tiled result-group layout with a full-width list for the active media kind. A wrapping text strip exposes available categories and counts. Preserve `Tab`, existing `g` category shortcuts, mouse selection and per-kind paging. The active type is derived from the selected visible item; do not introduce a second cursor that can disagree with it.

```text
query -> streamed results -> visible filtered/sorted list
                                  |
                             selected index
                                  |
              category tabs / row rendering / actions / paging
```

Every consumer of selection uses this same visible-list order. In particular, `g` kind navigation must not return an index into unsorted raw `search_results`. An empty filtered kind must not jump to a hidden row. A new query resets state through the existing query lifecycle, not by changing playback.

Test at 40,60,80 and 140 columns. Category labels may wrap; the song list keeps the remaining width. At wide sizes, show useful artist/album detail without adding independent search focus regions. These layout choices are the current Listening desk assumption (Q1).

## The row must remain identifiable

Remove decorative emoji from search category headings. Keep title and artist readable before album metadata. For artist detail, prevent a long album title from consuming nearly all the available row. Use terminal cell widths and the project's existing truncation helpers.

The selected row remains visually strongest. Counts and secondary metadata use muted styling. Existing selected and focus theme tokens carry state consistently in light and dark themes. No new colour system is needed.

## Play the track the user selected

Inside an opened playlist, Enter sends `PlayUri` for the highlighted visible track plus the playlist context. This preserves the collection for continuation while making the user's immediate selection authoritative. The previous baseline sent the playlist itself and discarded the selected row.

Filtering and sorting must affect both rendering and playback target. A playlist with no selected playable item does not accidentally restart the whole collection. Use the existing contextual playback request; no new protocol variant is needed.

## Explain queueing where it is offered

`e` continues to queue. When a playlist is selected, its contextual hint says “Queue playlist”. Browse remains the existing separate action. If Spotify denies access to contents, keep the playlist selected and report the permission failure from [19](19-playback-and-provider-corrections.md). Do not promise access because metadata appeared in search.

| User intent | Existing surface | Outcome to verify |
|---|---|---|
| Find a track | `search ... --format json`, TUI search | Same resource identity reaches play/queue |
| Browse a playlist | `playlist tracks <URI>`, existing TUI browse action | Contents or explicit access failure |
| Play a selected playlist row | `play-uri --context`, MCP `context_uri`, TUI existing `PlayUri` context | Highlighted track starts in playlist context |
| Append playlist contents | `queue add <URI> --wait --format json`, TUI `e` | Confirmed append or terminal failure |
| Change device | `transfer <device> --format json`, device picker | Transfer result names the target; no recovery takeover |

CLI machine formats remain `table|json|jsonl|csv|ids` where already supported. Preserve the existing exit-code mapping and mutation finalisation; this correction does not redesign them. MCP uses the same daemon workflows and accepts the optional `context_uri` for play. The installed skill and generated reference need changing only if their published command descriptions become inaccurate.

## Prove target consistency before visual approval

The automated gate checks narrow layouts, category mouse targets, visible-index navigation under filter/sort, playlist row contextual play, and queue hints. Capture actual renderer output, not a hand-drawn mock. Human review checks hierarchy and discoverability against [18](18-listening-desk.md); S4 records the remaining visual judgement.

## Keep the remaining interaction choices explicit

Up/down, page movement and top/bottom follow the active category. Tab, kind shortcuts and type clicks explicitly switch category. The selected index still points into the same visible filtered/sorted list; projecting movement through the active category keeps the rendered rows and next action consistent (D-054). The current correction does not add loading/error tabs for types with no results, redesign the persistent player's height, or unify Enter's existing artist/playlist behaviour. Track those choices with Q1 and the verified issues.

The baseline `app.rs` has 15,087 lines and `ui.rs` 7,652. If BK authorises structural work, split by search, playlist and device jobs, move code verbatim, preserve exports and prove a clean-checkout run before separate behaviour changes. That layout proposal does not authorise a refactor in this slice.
