#!/usr/bin/env python3
"""Turn `tmux capture-pane -e` dumps into one self-contained gallery page.

Driven by scripts/viz-gallery.sh through the environment:

    GALLERY_RAW      directory of <name>.ans captures plus an index.tsv
    GALLERY_OUT      HTML file to write
    GALLERY_VERSION  `spotuify --version` line, shown in the header
    GALLERY_SIZE     terminal size the captures were taken at

Only SGR is interpreted; other escape sequences are dropped. tmux emits
truecolor, 256-colour, and basic codes depending on what the app asked for, so
all three are handled.
"""

from __future__ import annotations

import html
import os
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

# xterm's 16 base colours. The gallery renders on a dark page, so these are the
# usual "dark background" values rather than the washed-out light-terminal set.
BASE_COLORS = [
    (0x00, 0x00, 0x00), (0xCD, 0x31, 0x31), (0x0D, 0xBC, 0x79), (0xE5, 0xE5, 0x10),
    (0x24, 0x72, 0xC8), (0xBC, 0x3F, 0xBC), (0x11, 0xA8, 0xCD), (0xE5, 0xE5, 0xE5),
    (0x66, 0x66, 0x66), (0xF1, 0x4C, 0x4C), (0x23, 0xD1, 0x8B), (0xF5, 0xF5, 0x43),
    (0x3B, 0x8E, 0xEA), (0xD6, 0x70, 0xD6), (0x29, 0xB8, 0xDB), (0xFF, 0xFF, 0xFF),
]

CUBE_STEPS = [0x00, 0x5F, 0x87, 0xAF, 0xD7, 0xFF]

DEFAULT_FG = "#d6dae0"
DEFAULT_BG = "#12151a"

# One pass over both kinds of escape: an SGR (captured, and the only one that
# changes anything) or something else (dropped). Splitting this into two passes
# is how the first version silently ate every colour — a generic "strip CSI"
# regex matches `...m` too.
TOKEN = re.compile(
    r"\x1b\[([0-9;:]*)m"  # SGR
    r"|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)"  # OSC
    r"|\x1b\[[0-9;:?<>!]*[@-~]"  # any other CSI
    r"|\x1b[@-Z\\-_]"  # two-character escape
)


def xterm256(index: int) -> tuple[int, int, int]:
    if index < 16:
        return BASE_COLORS[index]
    if index < 232:
        index -= 16
        return (
            CUBE_STEPS[index // 36 % 6],
            CUBE_STEPS[index // 6 % 6],
            CUBE_STEPS[index % 6],
        )
    level = 8 + (index - 232) * 10
    return (level, level, level)


def hexcolor(rgb: tuple[int, int, int]) -> str:
    return "#%02x%02x%02x" % rgb


class Style:
    __slots__ = ("fg", "bg", "bold", "dim", "italic", "underline", "reverse")

    def __init__(self) -> None:
        self.reset()

    def reset(self) -> None:
        self.fg: str | None = None
        self.bg: str | None = None
        self.bold = False
        self.dim = False
        self.italic = False
        self.underline = False
        self.reverse = False

    def key(self) -> tuple:
        return (self.fg, self.bg, self.bold, self.dim, self.italic, self.underline, self.reverse)

    def css(self) -> str:
        fg = self.fg or DEFAULT_FG
        bg = self.bg or DEFAULT_BG
        if self.reverse:
            fg, bg = bg, fg
        parts = [f"color:{fg}"]
        if bg != DEFAULT_BG:
            parts.append(f"background:{bg}")
        if self.bold:
            parts.append("font-weight:700")
        if self.dim:
            parts.append("opacity:.62")
        if self.italic:
            parts.append("font-style:italic")
        if self.underline:
            parts.append("text-decoration:underline")
        return ";".join(parts)


def apply_sgr(style: Style, params: str) -> None:
    # An empty parameter list means SGR 0.
    codes = [int(p) for p in re.split(r"[;:]", params) if p != ""] or [0]
    i = 0
    while i < len(codes):
        code = codes[i]
        if code == 0:
            style.reset()
        elif code == 1:
            style.bold = True
        elif code == 2:
            style.dim = True
        elif code == 3:
            style.italic = True
        elif code == 4:
            style.underline = True
        elif code == 7:
            style.reverse = True
        elif code in (21, 22):
            style.bold = style.dim = False
        elif code == 23:
            style.italic = False
        elif code == 24:
            style.underline = False
        elif code == 27:
            style.reverse = False
        elif 30 <= code <= 37:
            style.fg = hexcolor(BASE_COLORS[code - 30])
        elif 90 <= code <= 97:
            style.fg = hexcolor(BASE_COLORS[code - 90 + 8])
        elif 40 <= code <= 47:
            style.bg = hexcolor(BASE_COLORS[code - 40])
        elif 100 <= code <= 107:
            style.bg = hexcolor(BASE_COLORS[code - 100 + 8])
        elif code == 39:
            style.fg = None
        elif code == 49:
            style.bg = None
        elif code in (38, 48):
            color = None
            if i + 2 < len(codes) and codes[i + 1] == 5:
                color = hexcolor(xterm256(codes[i + 2]))
                i += 2
            elif i + 4 < len(codes) and codes[i + 1] == 2:
                color = hexcolor(tuple(codes[i + 2 : i + 5]))
                i += 4
            if color is not None:
                if code == 38:
                    style.fg = color
                else:
                    style.bg = color
        i += 1


def ansi_to_html(text: str) -> str:
    style = Style()
    out: list[str] = []
    open_key: tuple | None = None

    def close() -> None:
        nonlocal open_key
        if open_key is not None:
            out.append("</span>")
            open_key = None

    def emit(chunk: str) -> None:
        nonlocal open_key
        if not chunk:
            return
        key = style.key()
        if key != open_key:
            close()
            out.append(f'<span style="{style.css()}">')
            open_key = key
        out.append(html.escape(chunk))

    position = 0
    for match in TOKEN.finditer(text):
        emit(text[position : match.start()])
        if match.group(1) is not None:
            apply_sgr(style, match.group(1))
        position = match.end()
    emit(text[position:])
    close()
    return "".join(out)


PAGE = """<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>spotuify visualizer gallery</title>
<style>
  :root {{ color-scheme: dark; }}
  * {{ box-sizing: border-box; }}
  body {{
    margin: 0; background: #0b0d11; color: #d6dae0;
    font: 14px/1.5 ui-sans-serif, -apple-system, "Segoe UI", Roboto, sans-serif;
  }}
  header {{
    position: sticky; top: 0; z-index: 2; padding: 14px 22px;
    background: #0b0d11ee; border-bottom: 1px solid #232a33;
    backdrop-filter: blur(6px);
  }}
  header h1 {{ margin: 0 0 4px; font-size: 16px; letter-spacing: .01em; }}
  header .meta {{ color: #7f8a99; font-size: 12px; }}
  header .meta code {{ color: #9fb0c4; }}
  .layout {{ display: grid; grid-template-columns: 240px 1fr; align-items: start; }}
  nav {{
    position: sticky; top: 62px; max-height: calc(100vh - 62px); overflow: auto;
    padding: 18px 12px 40px; border-right: 1px solid #232a33;
  }}
  nav h2 {{
    margin: 16px 0 6px; font-size: 11px; text-transform: uppercase;
    letter-spacing: .09em; color: #6c7787;
  }}
  nav h2:first-child {{ margin-top: 0; }}
  nav a {{
    display: block; padding: 3px 8px; border-radius: 5px;
    color: #aab6c4; text-decoration: none; font-size: 13px;
  }}
  nav a:hover {{ background: #1a2029; color: #eaf0f7; }}
  main {{ padding: 18px 22px 120px; min-width: 0; }}
  section {{ margin: 0 0 26px; scroll-margin-top: 78px; }}
  section h3 {{
    margin: 0 0 8px; font-size: 13px; font-weight: 600; color: #eaf0f7;
    display: flex; align-items: baseline; gap: 10px;
  }}
  section h3 span {{ font-size: 11px; font-weight: 400; color: #6c7787; }}
  pre {{
    margin: 0; padding: 12px 14px; overflow-x: auto;
    background: {bg}; border: 1px solid #232a33; border-radius: 8px;
    font: 12px/1.18 "SF Mono", ui-monospace, Menlo, Consolas, monospace;
    white-space: pre; tab-size: 8;
  }}
  footer {{ padding: 0 22px 60px; color: #6c7787; font-size: 12px; }}
</style>
</head><body>
<header>
  <h1>spotuify visualizer gallery</h1>
  <div class="meta">
    <code>{version}</code> &middot; {size} terminal &middot; fake provider with
    <code>SPOTUIFY_VIZ_SYNTH=1</code> &middot; captured {captured}
  </div>
</header>
<div class="layout">
  <nav>{nav}</nav>
  <main>{panels}</main>
</div>
<footer>
  Captures are real <code>tmux capture-pane -e</code> output from the TUI, with
  SGR mapped to spans. Colours are xterm's dark-background palette, so they will
  differ slightly from a terminal with a custom palette.
</footer>
</body></html>
"""


GROUPS = {
    "panel-": "Styles — player panel",
    "full-": "Styles — fullscreen",
    "overlay-": "Overlays",
    "theme-": "Themes",
}


def group_of(name: str) -> str:
    for prefix, group in GROUPS.items():
        if name.startswith(prefix):
            return group
    return "Other"


def main() -> int:
    raw = Path(os.environ["GALLERY_RAW"])
    out = Path(os.environ["GALLERY_OUT"])
    version = os.environ.get("GALLERY_VERSION", "unknown")
    size = os.environ.get("GALLERY_SIZE", "?")

    index = raw / "index.tsv"
    if not index.exists():
        print(f"no captures in {raw}", file=sys.stderr)
        return 1

    entries: list[tuple[str, str]] = []
    for line in index.read_text().splitlines():
        if not line.strip():
            continue
        name, _, title = line.partition("\t")
        entries.append((name, title or name))

    nav: list[str] = []
    panels: list[str] = []
    current_group: str | None = None
    for name, title in entries:
        group = group_of(name)
        if group != current_group:
            nav.append(f"<h2>{html.escape(group)}</h2>")
            current_group = group
        nav.append(f'<a href="#{html.escape(name)}">{html.escape(title)}</a>')
        body = ansi_to_html((raw / f"{name}.ans").read_text(errors="replace"))
        panels.append(
            f'<section id="{html.escape(name)}">'
            f"<h3>{html.escape(title)}<span>{html.escape(name)}</span></h3>"
            f"<pre>{body}</pre></section>"
        )

    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(
        PAGE.format(
            version=html.escape(version),
            size=html.escape(size),
            captured=datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC"),
            nav="".join(nav),
            panels="".join(panels),
            bg=DEFAULT_BG,
        )
    )
    print(f"{len(entries)} panels -> {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
