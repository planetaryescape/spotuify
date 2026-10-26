# Verified follow-ups have one owner per issue

One markdown file per issue, named `NNN-short-slug.md`. These are scoped findings from the 2026-10-26 audit, not additional feature requests.

- [001](001-playlist-loading-outcomes.md): settle background playlist loading with contents or an explicit failure.
- [002](002-transfer-readiness.md): verify device readiness and delayed transfer outcomes before healing playback.
- [003](003-log-storage-budget.md): cap stored log bytes as well as file count.
- [004](004-search-state-and-compact-player.md): review empty/error categories, key semantics and compact player hierarchy.

- [005](005-contextual-play-cli-gap.md): implemented CLI/MCP contextual track-play parity; final verification in the handover.
- [006](006-playlist-queue-reconnect.md): closed; installed 0.1.111 preserves playlist queue progression through reconnect.
- [007](007-stale-playback-poll.md): a late Web API playback poll can temporarily restore an older track in daemon status.

Current implementation and evidence: [recovery handover](../implementation/2026-10-26-recovery-handover.md).
