# spotuify - Agent Workflows

## Principle

Agents should use the same CLI humans use. No hidden agent-only API.

## Safe agent loop

1. Research or receive user intent.
2. Generate a structured plan.
3. Resolve candidate Spotify items.
4. Dry-run mutation.
5. Show preview to user.
6. Commit with `--yes` only after approval.
7. Return receipt and playlist URI.

## Researched playlist workflow

User prompt:

```text
Make me a playlist about exile and returning home.
```

Agent workflow:

```text
spotuify playlist plan "exile and returning home" --format json > plan.json
spotuify resolve-tracks --from plan.json --format jsonl > candidates.jsonl
spotuify playlist create "Exile and Return" --from candidates.jsonl --dry-run
spotuify playlist create "Exile and Return" --from candidates.jsonl --yes
```

## Plan schema

A playlist plan should include:

- title
- description
- target mood
- eras or genres
- candidate concepts
- explicit exclusions
- desired length
- sequencing notes
- candidate queries

## Candidate resolution

Track resolution should output:

- query used
- chosen URI
- confidence
- reason
- alternatives
- duplicate status
- explicit flag if available
- source: local, Spotify, cached search

## Preview output

Dry-run should include:

- playlist metadata
- ordered tracks
- unresolved concepts
- duplicates removed
- warnings
- exact mutation that would be sent

## Agent guardrails

- Agents should not create broad playlists without preview.
- Agents should prefer local/cache results for user-known library unless user requests exploration.
- Agents should avoid claims about lyrics/themes unless they researched outside Spotify metadata.
- Agents should not use Spotify content to train models.
