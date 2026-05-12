# spotuify docs

This directory captures the long-term product blueprint and the implementation plan for spotuify.

The codebase is still the source of truth for what exists today. The blueprint describes the intended architecture and product contract. The implementation docs describe how to get there without losing the working app along the way.

## Document sets

| Directory | Purpose |
|---|---|
| [blueprint](blueprint/README.md) | Product principles, system architecture, CLI/TUI parity, daemon/cache/search/player design |
| [implementation](implementation/README.md) | Current-state audit, phased delivery plan, tests, migration notes |

## Reading order

1. [Blueprint overview](blueprint/00-overview.md)
2. [Architecture](blueprint/01-architecture.md)
3. [CLI and shell integration](blueprint/06-cli.md)
4. [Reuse strategy](blueprint/14-reuse-strategy.md)
5. [Player contract](blueprint/07-player.md)
6. [Implementation current state](implementation/00-current-state.md)
7. [Implementation plan](implementation/README.md)

## Agent guidance

If a blueprint doc conflicts with code, inspect code before editing. If the conflict is intentional drift, update the implementation docs or decision log instead of guessing.

Prefer mxr-proven code over greenfield rewrites. If mxr already solved daemon lifecycle, IPC, output rendering, SQLite, Tantivy, action dispatch, or TUI async reconciliation, start by copying the relevant implementation and adapting domain structs.

Root guidance files:

- [Architecture summary](../ARCHITECTURE.md)
- [Agent context](../AGENTS.md)
- [Contributing](../CONTRIBUTING.md)
