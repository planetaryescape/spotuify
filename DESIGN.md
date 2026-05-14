# DESIGN.md

## System Name

spotuify docs

## Register

brand

## Visual Direction

Listening desk in a terminal. The site should feel like a polished command surface: dark enough for late-night coding, sharp enough for reference reading, and strange enough not to look like a stock docs template.

## Color Strategy

Full palette, restrained usage:

- `oxide`: warm near-black background.
- `phosphor`: Spotify-adjacent green used for active command moments.
- `signal`: amber for prompts, callouts, and command rails.
- `cyan`: device, daemon, and network accents.
- `paper`: tinted text, never pure white.

Use OKLCH values in CSS. Green is not the only cue.

## Typography

- Display/body: Bricolage Grotesque.
- Mono/code: Sometype Mono.
- Body copy stays at 65 to 75 characters.
- Hero type is large and blunt, but reference pages stay readable.

## Layout

- Homepage: asymmetric hero with a terminal/product visual.
- Docs pages: readable Starlight structure with customized chrome.
- No nested cards.
- Cards only where they group repeated commands or surfaces.
- Use ruled grids, command strips, terminal panes, and equalizer bars as the visual grammar.

## Components

- Command strip: single-line command blocks with prompt glyph.
- Terminal board: homepage visual showing real `spotuify` usage.
- Surface grid: compact docs navigation, not marketing cards.
- Reference pages: generated CLI help with examples above raw help text.

## Motion

Subtle only. Use short fade/translate reveals and hover shifts. Respect `prefers-reduced-motion`.

## Accessibility

Maintain AA contrast on dark and light themes. Use visible focus rings. Keep code blocks scrollable on small screens. Avoid communicating status by color alone.
