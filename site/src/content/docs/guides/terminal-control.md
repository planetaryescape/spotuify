---
title: "Terminal Control"
description: "Control playback from shells, editors, launchers, and scripts."
---

Use this page when you do not want the TUI. The commands are meant for aliases, status bars, editor commands, and agents.

## Play and move on

```bash
spotuify play "imagine dragons" --type track
```

What you get: the first matching track starts on the active or preferred device.

## Common controls

```bash
spotuify toggle
spotuify next
spotuify previous
spotuify seek +15s
spotuify volume 70
spotuify shuffle toggle
spotuify repeat context
```

What you get: small mutation receipts. Add `--format json` when another program reads the result.

## Shell aliases

Small commands make good aliases:

```bash
alias splay='spotuify play'
alias snext='spotuify next'
alias spause='spotuify pause'
alias sstatus='spotuify status --format json'
```

Long pipelines usually want a shell function. Put this in `~/.zshrc`, `~/.bashrc`, or your shell's equivalent:

```bash
freedom() {
  spotuify search "songs about freedom" --type track --format ids \
    | fzf \
    | xargs spotuify play-uri
}
```

Then open a terminal and run:

```bash
freedom
```

Use the same pattern for agent prompts, playlist recipes, or any search you repeat often. `spotuify` stays boring and pipeable; your shell gives the workflow a short name.

## Editor commands

For Neovim, bind a command to one-shot playback:

```vim
command! -nargs=+ SPlay !spotuify play "<args>"
command! SNext !spotuify next
```

Then:

```vim
:SPlay inspirational music
```

## Status bar data

```bash
spotuify status --format json \
  | jq -r '.item.name + " - " + .item.subtitle'
```

What you get: a compact now-playing string for tmux, SketchyBar, Waybar, or a custom prompt.

## Follow lyrics

```bash
spotuify play "never too much" --type track
spotuify lyrics follow --lines 3
```

What you get: a small previous/current/next lyrics window that advances with the current track. Use `Ctrl-C` to leave the lyrics follower; playback keeps going.

For scripts, switch to JSONL:

```bash
spotuify lyrics follow --format jsonl
```

`lyrics follow` needs synced lyrics. If a track has only plain lyrics, run `spotuify lyrics show`.

## Recover from network changes

```bash
spotuify reconnect
spotuify status
```

Use `reconnect` after a VPN flap, sleep/wake, or a Spotify session that went stale.

## See Also

- [Search and Play](/guides/search-and-play/)
- [Recipes](/guides/recipes/)
- [CLI Concepts](/reference/cli/concepts/)
- [Playback Commands](/reference/cli/play/)
