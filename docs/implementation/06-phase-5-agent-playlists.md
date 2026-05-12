# Phase 5 - Agent Playlists

## Goal

Let agents research a theme, resolve tracks, preview a playlist, and create it safely through spotuify CLI.

## Deliverables

- Playlist plan JSON schema.
- Candidate track resolution command.
- Playlist dry-run preview.
- Playlist commit command.
- Mutation receipts.
- Recipes for agents.

## Commands

```text
spotuify playlist plan "brief" --format json
spotuify resolve-tracks --from plan.json --format jsonl
spotuify playlist create "Name" --from candidates.jsonl --dry-run
spotuify playlist create "Name" --from candidates.jsonl --yes
```

## Plan schema fields

- title
- description
- target length
- mood
- theme notes
- candidate searches
- sequencing notes
- exclusions

## Resolution requirements

- Deduplicate exact tracks.
- Prefer playable tracks.
- Preserve alternatives.
- Explain confidence.
- Return unresolved items explicitly.

## Safety requirements

- No playlist creation without dry-run unless `--yes` is passed.
- Dry-run and commit use same resolved candidate set.
- Receipt includes playlist ID/URI and added item count.

## Definition of done

An agent can create a playlist from a user brief with a previewable, repeatable CLI workflow.
