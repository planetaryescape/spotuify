# Verified follow-ups have one owner per issue

One markdown file per issue, named `NNN-short-slug.md`. These are scoped findings from the 2026-10-26 audit, not additional feature requests.

- [001](001-playlist-loading-outcomes.md): settle background playlist loading with contents or an explicit failure.
- [002](002-transfer-readiness.md): verify device readiness and delayed transfer outcomes before healing playback.
- [003](003-log-storage-budget.md): cap stored log bytes as well as file count.
- [004](004-search-state-and-compact-player.md): review empty/error categories, key semantics and compact player hierarchy.

- [005](005-contextual-play-cli-gap.md): implemented CLI/MCP contextual track-play parity; final verification in the handover.

Current implementation and evidence: [recovery handover](../implementation/2026-10-26-recovery-handover.md).
