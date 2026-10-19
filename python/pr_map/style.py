"""Colours, line styles and text fitting of the full map (app.py). The sidebar minimap
(pr-map-rs/src/style.rs) uses the same palette, and the backend assigns session and category
colours (model["colors"])."""

import unicodedata

from . import prmap

BG, PANEL, BAR, FG, DIM, MUTED, GRID = ("#1a1b26", "#1f2335", "#16161e", "#c0caf5", "#a9b1d6",
                                         "#565f89", "#3b4261")
ACCENT, WARN, YOU = "#7dcfff", "#ff9e64", "#ff007c"
STAGE_COLORS = {"planning": "#bb9af7", "developing": "#e0af68", "evaluating": "#7dcfff",
                "fix": "#f7768e", "ready": "#9ece6a", "abandoned": MUTED}
UNTAGGED = "#4c5578"
STATUS = {"working": ("●", "#9ece6a"), "blocked": ("▲", "#f7768e"), "done": ("◆", DIM),
          "idle": ("○", MUTED), "unknown": ("?", MUTED)}


def blend(color, base, t):
    """`color` moved toward `base` by t: 0 keeps it, 1 gives `base`."""
    a = [int(color[i:i + 2], 16) for i in (1, 3, 5)]
    b = [int(base[i:i + 2], 16) for i in (1, 3, 5)]
    return "#" + "".join(f"{round(x + (y - x) * t):02x}" for x, y in zip(a, b))


def category(node):
    return node["title"]["category"] or node["title"]["domain"]


def cell_width(ch):
    """Terminal cells a character takes: two for wide (CJK) ones, none for combining marks."""
    return 0 if unicodedata.combining(ch) else 2 if unicodedata.east_asian_width(ch) in "WF" else 1


def cells(text):
    return sum(cell_width(ch) for ch in text)


def fit(text, width):
    """text cut to `width` terminal cells, ending in '…' when cut."""
    if cells(text) <= width:
        return text
    out, used = "", 0
    for ch in text:
        if used + cell_width(ch) > width - 1:
            break
        out, used = out + ch, used + cell_width(ch)
    return out + "…" if width > 0 else ""


def edge_style(node, cat_colors):
    """(colour, horizontal glyph, vertical glyph) of the line leading to a station.

    Dotted for a local branch, grey for a closed PR, dashed for a draft; the category colour
    turns WARN when the PR does not contain its base's head.
    """
    if node["kind"] == "local":
        return MUTED, "·", "┊"
    if prmap.is_closed(node):
        return MUTED, "─", "│"
    color = WARN if node.get("synced") is False else cat_colors.get(category(node), UNTAGGED)
    return (color, "╌", "╎") if node["pr"]["isDraft"] else (color, "─", "│")
