---
title: "Agents and MCP"
description: "Integrate spotuify with coding agents: install the agent skill or run the MCP server."
---

Agents use the same surfaces humans use. There is no hidden agent-only path. There are two ways to wire spotuify into an agent, and they compose:

1. The **agent skill** teaches an agent to drive the `spotuify` CLI well (output flags, preview-first mutations, guardrails). Works with any agent that loads skills and can run shell commands.
2. The **MCP server** exposes the daemon as structured tools for agents that speak MCP.

## Install the agent skill

The skill is a single `SKILL.md`. Drop it into your agent's skills directory.

```bash
mkdir -p ~/.claude/skills/spotuify
curl -fsSL https://raw.githubusercontent.com/planetaryescape/spotuify/main/skills/spotuify/SKILL.md \
  -o ~/.claude/skills/spotuify/SKILL.md
```

Prefer a packaged bundle? Download [`spotuify.skill`](/spotuify.skill) and unzip it into the same directory:

```bash
curl -fsSL https://spotuify.vercel.app/spotuify.skill -o /tmp/spotuify.skill
unzip -o /tmp/spotuify.skill -d ~/.claude/skills/spotuify
```

Once installed, the agent knows the command surface, always reads `--format json`, and previews mutations with `--dry-run` before applying them. It also knows the behaviors that trip up agents: the queue is a set (re-adding a queued track is skipped, and queue adds are not undoable), `audio-output` switches live without a daemon restart, and discovery runs through `artist related` / `radio start`.

## Run the MCP server

```bash
spotuify mcp                          # JSON-RPC 2.0 over stdio (default transport)
spotuify mcp --http 127.0.0.1:8765    # loopback Streamable HTTP
```

The HTTP transport binds to loopback only and requires a token:

```bash
export SPOTUIFY_MCP_TOKEN="$(openssl rand -hex 32)"
spotuify mcp --http 127.0.0.1:8765
```

## Connect your agent

Most clients launch the stdio transport for you. Register `spotuify mcp` as the command.

Claude Code:

```bash
claude mcp add spotuify -- spotuify mcp
```

Claude Desktop (`claude_desktop_config.json`) or Cursor (`~/.cursor/mcp.json`):

```json
{
  "mcpServers": {
    "spotuify": { "command": "spotuify", "args": ["mcp"] }
  }
}
```

For an HTTP-based client, start `spotuify mcp --http 127.0.0.1:8765` yourself and point the client at `http://127.0.0.1:8765` with the `SPOTUIFY_MCP_TOKEN` as a bearer token.

## MCP tools

Tools mirror the CLI (37 in total). Reads and transport are safe by default; persistent changes are preview-first and need confirmation in the tool args.

| Tool | Kind | Notes |
| --- | --- | --- |
| `search` | read | local or remote search |
| `now_playing` | read | current playback |
| `devices_list` / `queue_show` / `playlists_list` / `playlist_tracks` / `library_list` | read | current state |
| `playlist_plan` / `playlist_resolve_tracks` | read | plan a playlist, resolve to URIs |
| `lyrics` / `related_artists` | discovery | synced lyrics; Mercury-backed related artists |
| `analytics_top` / `analytics_habits` / `analytics_search` / `analytics_rediscovery` | analytics | local listening data |
| `play` / `play_uri` / `pause` / `resume` / `next` / `previous` / `seek` / `volume` / `shuffle` / `repeat` | transport | playback control |
| `queue_add` / `transfer_device` / `radio_start` | transport | queue, device, Mercury radio |
| `playlist_create` / `playlist_add` / `playlist_remove` / `playlist_unfollow` / `playlist_set_image` | destructive | preview unless confirmed |
| `library_save` / `library_unsave` | destructive | like/unlike |
| `ops_log` / `undo_last` | ops | inspect the op log; reversal safety net |

## MCP resources

```text
spotuify://playback
spotuify://devices
spotuify://playlists
spotuify://now_playing/lyrics
spotuify://doctor
```

Read a resource when the agent needs current state instead of issuing another command.

Over the stdio transport the server also pushes `notifications/resources/updated`
for resources the client has `resources/subscribe`d to (`spotuify://playback`,
`spotuify://devices`, `spotuify://playlists`), so an agent can react to live
changes without polling. The HTTP transport has no SSE, so push is stdio-only;
HTTP clients re-read on their own cadence.

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

## Guardrails

- Use `--format json` or `--format jsonl` for agent reads.
- Use `--dry-run` for broad playlist changes; show the preview before applying.
- Prefer URIs and IDs over display names.
- Do not claim lyrics or themes unless you checked a lyrics provider or another source.
- Do not run `--yes` without explicit user approval.

## See Also

- [Queue and Playlists](/guides/queue-and-playlists/)
- [JSON Output](/reference/json-output/)
- [IPC Protocol](/reference/ipc/)
