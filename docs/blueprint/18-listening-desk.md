# 18: Give the music one readable place

The blueprint assumes the concept **Listening desk** (Q1, D-043). It follows BK's complaint about cramped columns and the existing product brief's “serious terminal instrument”. The root `DESIGN.md` describes the website; it is not a TUI specification.

## The selected music is the hero

People open `spotuify` while doing something else. They want to find a song, know where it will play, and return to their work. A stable list and a quiet player anchor should make that feel immediate and predictable. The screen must not require learning six simultaneous focus regions to choose one song.

Listening desk uses one clear working area above a persistent now-playing strip. Category labels feel like dividers in a music collection. The selected row carries the strongest emphasis. Artwork, visualisation and secondary metadata yield space before the song title does.

## The same few nouns describe every surface

```text
Search  -- open collection --> Playlist / Album / Artist / Show
   |                                  |
   +------------ select track --------+
                         |
                    Play or Queue
                         |
                    Now playing -- choose --> Devices
                         |
                       Queue

Escape returns to the previous view with query, filter and selection intact.
Diagnostics explains a failure without replacing the music navigation model.
```

Search finds music. Playlists and Library hold collections. Queue holds upcoming items. Devices chooses where playback happens. Now playing answers what is playing and where. CLI and MCP use these same resource names and daemon states.

## One action must have one visible meaning

Arrow/page/top/bottom movement follows the displayed category; Tab and type shortcuts switch categories (D-054). Keep `e` as queue; label it “Queue playlist” when the selected thing is a playlist. Do not infer that BK wanted a remapping from his description of the failure (D-046). Enter plays the selected track; opening a collection uses the existing browse action. Hints, help and the action dispatcher must agree about their current target.

For each main journey:

1. Search retains the query while pages arrive. Switching type keeps the same visible-index model used by sorting and filtering. Enter plays the highlighted result.
2. Opening a playlist retains its identity. Selecting a row and pressing Enter sends that row's URI with playlist context. Returning restores the previous search.
3. Queueing a playlist resolves its accessible tracks before mutation. Denied access leaves the selection in place and explains that Spotify would not provide the contents.
4. A device transfer stays pending until its terminal receipt. A Connect handoff revokes local recovery intent. Another device's playback must never become an excuse to resume locally.
5. An error names the action and resource, preserves the current list, and offers an applicable next step. A diagnostic report shares the daemon's findings (D-048).

## Restraint follows the available cells

| Element | Treatment | Reason |
|---|---|---|
| Primary list | Full available width; selected row visible | Music titles remain identifiable |
| Type navigation | Text labels and counts; wraps to additional rows | Category discovery survives narrow terminals |
| Secondary metadata | Artist before album; truncate by terminal cell width | Long album names cannot consume the row |
| Now playing | Persistent compact strip | Context survives navigation |
| Artwork / optional rail | Existing responsive rules; subordinate to list | They must not make a working list unreadable |
| Error | Existing banner with action and recovery text | No modal maze or silent failure |
| Secondary actions | Context hints / palette / help | Equal emphasis on every action makes scanning harder |

Use the user's terminal font and existing theme tokens. Selected/focused text uses the theme's selected foreground/background and a textual cursor cue. Metadata uses muted foreground; errors use the error token plus explanatory words. Preserve every shipped theme; colour alone never encodes state. Do not introduce fixed web fonts, hard-coded RGB palettes or new theme settings.

## Change should explain itself

Selection moves on the next frame, with a target of under 16 ms for local key handling. That is a budget, not a measured claim. Network progress is asynchronous; the list stays usable while it waits. No decorative animation is needed. Current reduced-motion and visualiser preferences continue to apply.

| Moment | Intended feeling | Visible response |
|---|---|---|
| First open | Ready to listen | Existing navigation and current playback are readable |
| Search waiting | My query is still here | Loading state preserves query and selection |
| Choose category | I am browsing one useful list | Highlight moves and the list occupies the working area |
| Queue accepted | Work is in progress | Pending receipt differs from confirmed completion |
| Queue denied | I know what failed | Playlist and permission cause remain in the error |
| Device handoff | My choice sticks | Local ownership clears and recovery stays idle |
| Nothing found | I can revise the query | Empty state differs from an upstream failure |

## The compact screen has one focal point

Illustrative layout; exact titles and theme colours come from the renderer:

```text
Search > hillsong
Tracks  Albums  Artists  Playlists
> New Wine                 Hillsong Worship
  Who You Say I Am         Hillsong Worship
  What A Beautiful Name    Hillsong Worship

Enter Play    e Queue    Tab Next type
New Wine  /  spotuify-hume       01:12 / 04:02
```

A wider screen gives this same list more room for metadata. It does not automatically add five competing result panes. Q1 keeps that wider-screen choice visible for BK's review.

## Verification includes the experience

Read text captures at 40, 60, 80 and 140 columns. Exercise wrapping categories, long Unicode names, empty results, filtering, sorting, scrolling and mouse category selection. Compare at least dark and light shipped themes. Review the first-open, error and device-pending moments against this concept. Automated captures establish layout and target consistency; BK's use establishes whether the experience feels right. Broader changes to the player, rail or navigation need a comp reviewed against this concept before implementation.
