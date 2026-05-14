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

```bash
alias splay='spotuify play'
alias snext='spotuify next'
alias spause='spotuify pause'
alias sstatus='spotuify status --format json'
```

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

## Recover from network changes

```bash
spotuify reconnect
spotuify status
```

Use `reconnect` after a VPN flap, sleep/wake, or a Spotify session that went stale.

## See Also

- [Search and Play](/guides/search-and-play/)
- [CLI Concepts](/reference/cli/concepts/)
- [Playback Commands](/reference/cli/play/)
