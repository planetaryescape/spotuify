---
title: "Themes"
description: "Recolour the TUI with a built-in theme or your own TOML file."
---

The TUI ships with one palette and nine alternatives. Switching is one
command, and adding your own is one file.

```bash
spotuify theme list          # every theme, active one marked with *
spotuify theme winamp        # switch
spotuify theme               # what is active, and its colours
spotuify theme path          # where your own themes go
```

In the TUI, `t` opens a picker. Arrow keys repaint the whole interface as
you move, so you see the theme before you keep it; `Enter` keeps it, `Esc`
puts back what you had, `/` filters by name.

The daemon owns the choice. Switching from the CLI repaints an open TUI
immediately: the daemon persists `tui.theme` and broadcasts the resolved
colours to every connected client.

Editing `tui.theme` in the config file by hand works too; it takes effect on
`spotuify reload`, which repaints running clients the same way.

## The built-ins

`terminal-default` is the default and carries no colours: the TUI keeps the
palette it ships with. The rest come from
[cliamp](https://github.com/bjarneo/cliamp) (MIT):

`catppuccin`, `dracula`, `everforest`, `gruvbox`, `kanagawa`, `nord`,
`rose-pine`, `tokyo-night`, `winamp`.

Every one of them holds at least a 4.5:1 contrast ratio for text on its own
background, enforced by a test, so a theme you cannot read never ships.

## Writing your own

Drop a `.toml` file in the directory `spotuify theme path` prints
(`~/.config/spotuify/themes` on Linux,
`~/Library/Application Support/spotuify/themes` on macOS). The file name is
the theme name.

```toml
# ~/.config/spotuify/themes/my-theme.toml
bg        = "#000000"   # optional; omit to keep your terminal's background
accent    = "#00FF00"   # selection, focused borders, section headers
bright_fg = "#FFFFFF"   # primary text
fg        = "#969696"   # secondary text
green     = "#29CE10"   # success, progress fill, low spectrum band
yellow    = "#D6B521"   # warnings, mid spectrum band
red       = "#EF3110"   # errors, high spectrum band
```

Six of the seven are required; `bg` is not. Values must be `#RRGGBB`: no
short forms, no colour names. Keys spotuify does not know are ignored, so a
theme written for another player still loads.

A file that sets only some of the six is an error naming the first role it
missed, not a theme that silently falls back to built-in colours. Files over
64 KiB are skipped: a theme is seven lines.

That is the cliamp theme format, unchanged, so any theme written for cliamp
works here as-is.

The surfaces between background and text (panel fills, borders, chip
backgrounds, the unfilled part of the seek bar) are derived by blending
`bg` toward `fg` at the ratios the built-in palette uses. You do not name
them, and you cannot accidentally produce chrome you cannot see.

The media-kind glyphs (podcast / album / artist) keep fixed hues in every
theme. They are a legend, not decoration: the colour tells you the category,
so it has to mean the same thing everywhere.

### Overriding a built-in

A file named after a built-in replaces it. Save your edits as `nord.toml`
and `spotuify theme nord` uses yours; `spotuify theme list` shows `user`
instead of `builtin` in that row. Delete the file to get the original back.

`terminal-default`, `list`, and `path` are reserved. The last two are
`spotuify theme`'s own subcommands, so a theme with either name could be
listed but never applied; naming a file `list.toml` gets a warning instead
of a theme you cannot select.

### When a file is broken

A theme file that will not parse, or that is missing a colour, is skipped
with a warning in the daemon log, and the rest of the list still works. Fix it
and run `spotuify theme list` again; there is no daemon restart.

If `tui.theme` names a file you later delete, the TUI falls back to the
built-in palette rather than failing to start.

## Album-adaptive accents

When cover art is loaded, spotuify derives the accent from the artwork and
that wins over the theme's `accent`, the same behaviour as before themes
existed. The rest of the theme still applies. Your theme's `accent` is what
shows when there is no art.

## Also available to

- **MCP**: `themes_list`, `theme_set { name }`.
- **Config**: `spotuify config set tui.theme nord`, same validation.
