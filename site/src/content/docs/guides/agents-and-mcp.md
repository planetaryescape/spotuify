---
title: "Agents and MCP"
description: "Use spotuify safely from coding agents and MCP clients."
---

Agents should use the same surfaces humans use. There is no hidden agent-only path.

## Safe playlist loop

```bash
spotuify playlist plan "exile and returning home" --format json > plan.json
spotuify resolve-tracks --from plan.json --format jsonl > candidates.jsonl
spotuify playlist create "Exile and Return" --from candidates.jsonl --dry-run
```

Commit only after approval:

```bash
spotuify playlist create "Exile and Return" --from candidates.jsonl --yes --format json
```

## Prompt shape

```text
Make me a playlist for a hard debugging session.
Use spotuify playlist plan, resolve-tracks, and playlist create --dry-run.
Show me the preview. Do not use --yes until I approve.
```

## MCP tools

The MCP server exposes the daemon as tools. Read and transport tools are safe by default; persistent changes require confirmation.

```bash
spotuify-mcp
```

Representative tools:

| Tool | Kind | Notes |
| --- | --- | --- |
| `search` | read | local/remote search |
| `now_playing` | read | current playback |
| `play` | transport | plays a query |
| `queue_add` | destructive | requires confirmation in tool args |
| `playlist_create` | destructive | preview unless confirmed |
| `analytics_top` | analytics | local listening data |
| `undo_last` | ops | reversal safety net |

## MCP resources

```text
spotuify://playback
spotuify://devices
spotuify://playlists
spotuify://now_playing/lyrics
spotuify://doctor
```

Use resources when an agent needs current state instead of issuing another command.

## Guardrails

- Use `--format json` or `--format jsonl` for agent reads.
- Use `--dry-run` for broad playlist changes.
- Prefer URIs and IDs over display names.
- Do not claim lyrics or themes unless you checked a lyrics provider or another source.
- Do not run `--yes` without explicit user approval.

## See Also

- [Queue and Playlists](/guides/queue-and-playlists/)
- [JSON Output](/reference/json-output/)
- [IPC Protocol](/reference/ipc/)
