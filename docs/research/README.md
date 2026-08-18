# spotuify - Research

Research material that informs the blueprint and implementation plan. Snapshots of competitor codebases and the wider Rust Spotify-tool ecosystem at the time of capture. Treated as leads, not ground truth — the linked source code is the canonical reference.

## Documents

| # | Document | Purpose |
|---|---|---|
| 00 | [Competitor Analysis (synthesis)](competitor-analysis.md) | Cross-cutting comparison, patterns adopted/avoided, differentiation matrix |
| 01 | [ncspot Deep Study](ncspot.md) | librespot-embedded Cursive TUI, longest-lived (2018-) |
| 02 | [spotify-player Deep Study](spotify-player.md) | Ratatui TUI with optional daemon, librespot embed |
| 03 | [spotatui Deep Study](spotatui.md) | 2025 revival of abandoned spotify-tui with native streaming |
| 04 | [Apple Music Feasibility](apple-music-feasibility.md) | Why spotuify stays Spotify-only — no librespot equivalent for FairPlay, plus our missing provider seam. Settled as [D026](../blueprint/13-decision-log.md). Captured 2026-07-16 |

## Capture date

Reports captured **2026-05-13**. Versions sampled:

- ncspot: v1.3.3 (CHANGELOG 2026-02-06)
- spotify-player: v0.23.0
- spotatui: v0.38.2 (May 2026)

Re-validate before treating any claim as current — these repos move.

The public comparison pages were revalidated on **2026-08-12** against ncspot
v1.3.4 and spotify-player v0.24.1. The deep studies remain dated source
snapshots; update notes at the top of each report call out known upstream
changes without rewriting the historical evidence.

## How research is used

- Each finding is referenced in implementation phase docs via "Evidence base" sections.
- Patterns we adopt: cited file:line references in implementation work items.
- Patterns we reject: documented with rationale in the synthesis.
- Repos cloned to `/tmp/spotuify-research/` during the study; not committed.
