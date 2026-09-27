# Spotuify for macOS: design rubric and direction

This file sets the quality bar for the native app. Every screen is scored
against the rubric below before it ships. The direction section says how this
app meets the bar.

## The rubric

Score each criterion 0–4. A release needs no criterion below 3 and a total of
at least 44/52.

| # | Criterion | What a 4 looks like | Failure that caps it at 1 |
|---|-----------|---------------------|---------------------------|
| 1 | **The record is the hero** | Cover art is shown whole, large, and sharp. It sets the colour of the whole window. Chrome sits back from it. | The cover is cropped, stretched, or smaller than the controls around it. |
| 2 | **One-glance state** | From any screen, in under a second, you can tell what is playing, whether it is playing, where it plays, and how far in it is. | You have to navigate to find out what is playing or which device is live. |
| 3 | **Transport feels physical** | Play/pause answers at once. Hit targets are at least 28pt. Scrubbing shows the target time. Every transport action has a shortcut. | A control waits for the network before it moves, or a target is smaller than its glyph. |
| 4 | **The room takes the record's colour** | Palette comes from the artwork, crossfades between tracks, and stays legible (text ≥ 4.5:1, controls ≥ 3:1) on any cover, including white and black ones. | Colour is fixed or random, or text disappears on some covers. |
| 5 | **Type has a voice and a hierarchy** | Display serif for titles, system sans for UI, tabular numerals for time. Primary text never truncates while secondary text has room to spare. | A title truncates while a secondary column sits half empty. |
| 6 | **Controls are ranked by use** | Transport is always visible. Lyrics, queue, device and volume are one click away. EQ, bookmark and speed live in an overflow. At most seven controls sit at the same weight. | Ten controls at one visual weight in one bar. |
| 7 | **Browsing is a pleasure** | Grids let art breathe. Lists scan by title first. The playing row is marked with a live indicator. Hover shows actions without shifting layout. | Nothing shows which row is playing, or hover moves the layout. |
| 8 | **Motion has one focal point** | Track change is one crossfade. The cover eases back when paused. Direct manipulation uses springs. Reduce Motion is honoured everywhere. | Several things animate at once, or motion ignores Reduce Motion. |
| 9 | **A good Mac citizen** | Full-height glass sidebar, no empty title strip, standard shortcuts, menu bar and mini player, VoiceOver labels on every control, works in light and dark. | A dead title bar strip, or controls VoiceOver cannot name. |
| 10 | **Edges are designed** | Empty, loading, offline, no lyrics, no artwork, long titles, and one-item lists each have a deliberate state. | "1 songs", a bare spinner, or a grey box where art should be. |
| 11 | **One system** | One accent, one radius scale, one spacing scale, one icon weight. The accent reaches every surface, sidebar included. | The sidebar uses a different accent from the player. |
| 12 | **Notices never cover music** | Updates, errors and toasts appear where they do not hide controls or content. | A banner covers the mode switch or a page header. |
| 13 | **It has its own face** | A screenshot with the logo cropped out is still recognisably spotuify: custom room, type pairing, controls and navigation. System components appear only where the OS owns the job (menus, text input, sheets). | It looks like a stock macOS template: glass List sidebar, segmented toolbar control, bordered buttons. |

## Direction: liner notes in a listening room

The window is a dark room lit by the record that is playing, and the
interface is written like the liner notes on its sleeve. It does not borrow
the stock macOS look: no glass List sidebar, no segmented toolbar control, no
bordered buttons. Those are fine defaults for utilities; a music player should
have a face.

- **Room.** Every page sits on the same floor: the record's colour pushed
  almost to black, a faint glow of the cover in the top corner, and film grain
  over everything. The stage turns the glow up to a full blurred wash. A fixed
  light theme swaps the room for warm paper and ink.
- **Voice.** Fraunces for anything with a name (tracks, albums, page titles).
  A monospaced small-caps voice for facts (times, counts, eyebrows, section
  numbers) — spotuify is a terminal tool at heart, and this is where that shows.
  SF for everything you read in passing.
- **Navigation.** A typographic column, numbered like a track list:
  `01 LISTEN`, `02 LIBRARY`, `03 YOU`. The current page gets ink and an accent
  tick; the rest stay muted. A live meter sits beside Now Playing.
- **Stage.** The whole square cover on the wash. Wide windows put the record on
  the left and the liner notes (or a companion: lyrics, queue, visualizer) on
  the right. Companions are chosen with text tabs and a sliding underline. The
  cover eases back to 88% while paused.
- **Deck.** The player bar is a solid raised strip with a hairline, not a glass
  capsule: track on the left, transport over the seek line in the centre,
  companions and volume on the right, rare things in an overflow.
- **Pages.** A hero with the art, a mono eyebrow, a big Fraunces title, and one
  round accent play button. Track lists are numbered; the number turns into a
  live meter on the playing row and a play glyph on hover.
- **Tokens.** Colours come from the room: `base`, `raised`, `ink`, `inkMuted`,
  `inkFaint`, `hairline`, `accent`. Spacing 4/8/12/16/24/32/48. Radii 6
  (thumbnails), 10 (rows), 14 (tiles), full capsule for pills and buttons.
