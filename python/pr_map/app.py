"""PR map TUI: a metro-style map of the open PR stacks with live sessions on it.

Usage:
  pr-map            open the map
  pr-map --popup    as above; jumping to a session also closes the map (herdr popup binding).
                    Opened by a click on the herdr fork's sidebar minimap, it starts on the
                    clicked station (HERDR_PANEL_COLUMN, HERDR_PANEL_LINE, HERDR_PANEL_WIDTH,
                    HERDR_PANEL_HEIGHT).
  pr-map --json     print the model and exit

The model comes from the Rust backend (`prmap`, see prmap.py).

The repository ◈ is the root: a bus down the left edge leads to the base branches the stacks
start from, and sessions working in the workspace on no PR branch stand on it. Each PR is a
station: '#N Stage', its name (the category from its title, else the branch name) and the
sessions working on it, each in its own colour. '▼ YOU ARE HERE' marks the station of the session the map was opened from (herdr's
HERDR_ACTIVE_PANE_ID). A line takes the colour of the PR category it leads to ([Domain: Category]
in the title; grey when untagged) and turns orange when the PR does not contain its base's head;
a dashed line leads to a draft, a dotted one to a local branch without a PR, and a grey line
ending in ✕ to a PR closed without merging. '!' marks a Ready PR that needs a rebase; the detail
panel lists all warnings. Stacks without a working session are folded to their first station.

Mouse: drag the map to pan, click a station to open it, click a session to highlight it and
double-click to jump to its pane.
Keys: arrows/hjkl move, tab or enter goes to the station's sessions, enter on a session focuses
its herdr pane, o folds or unfolds the stack, a unfolds all, f follows this pane's session,
/ searches, 1-9 select a session's station, r refetches PRs, q quits.
"""

import os
import time
from datetime import datetime
from zoneinfo import ZoneInfo

from rich.segment import Segment
from rich.style import Style
from rich.text import Text
from textual import work
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Horizontal, VerticalScroll
from textual.geometry import Region, Size
from textual.message import Message
from textual.scroll_view import ScrollView
from textual.strip import Strip
from textual.widgets import Input, OptionList, Static
from textual.widgets.option_list import Option

from . import layout
from . import prmap
from .style import (ACCENT, BAR, BG, DIM, FG, GRID, MUTED, PANEL, STAGE_COLORS, STATUS, UNTAGGED,
                   WARN, YOU, category, cell_width, cells, edge_style, fit)

TZ = ZoneInfo("America/New_York")
CW = 19           # cells per stack depth
LABEL_W = CW - 3  # station label width; the branch junction sits right after it
LANE_H = 4        # line, '#N Stage', name, sessions
GAP = 1           # blank rows between base branch families
ROOT_Y = 1        # the repository's row; the YOU flag can stand above it
TOP = 3           # first lane
BUS_W = 3         # '├─ ' before a base branch's name
ROOT = layout.ROOT


def age(t):
    if prmap.DEMO and t:
        return "2m"
    if not t:
        return ""
    s = time.time() - t
    return "now" if s < 60 else f"{int(s // 60)}m" if s < 3600 else \
        f"{int(s // 3600)}h" if s < 86400 else f"{int(s // 86400)}d"


def ago(t):
    a = age(t)
    return "just now" if a == "now" else f"{a} ago" if a else "never"


def pad(text, width):
    """text fitted to exactly `width` terminal cells."""
    text = fit(text, width)
    return text + " " * (width - cells(text))


def fit_middle(text, width):
    """Keeps both ends, which tell similar branch names apart."""
    if cells(text) <= width:
        return text
    head = tail = ""
    for ch in text:
        if cells(head + ch) > (width - 1) // 2:
            break
        head += ch
    for ch in reversed(text):
        if cells(head + ch + tail) + 1 > width:
            break
        tail = ch + tail
    return head + "…" + tail


def explain(s, branch):
    """Why session s is listed on branch: (time, 'changed 2m ago · Edit: path')."""
    if branch == ROOT:
        return None, "in the workspace, on no PR branch"
    if s["current"] == branch and s["how"] == "cwd":
        return None, "terminal is in this worktree"
    evidence = s["evidence"].get(branch, [])
    want = s["how"] if s["current"] == branch else "read" if s["recent"] == branch else None
    pick = next((e for e in reversed(evidence) if want in (None, e[1])),
                evidence[-1] if evidence else None)
    if not pick:
        return None, ""
    ts, grade, text = pick
    return ts, f"{grade} {ago(ts)} · {text}"


def outside(model):
    """'outside the workspace: billing-api (1) · notebooks (2)'."""
    return "outside the workspace: " + " · ".join(
        f"{k} ({len(v)})" for k, v in sorted(model["elsewhere"].items()))


# ---------------------------------------------------------------- map widget

class MetroMap(ScrollView, can_focus=True):
    """Draws the visible stacks as metro lines and keeps a cell -> target hit map."""

    class Selected(Message):
        def __init__(self, branch, pane=None, jump=False):
            super().__init__()
            self.branch, self.pane, self.jump = branch, pane, jump

    def __init__(self, **kwargs):
        super().__init__(**kwargs)
        self.model = None
        self.selected = None
        self.me = None              # pane of the session that opened the map
        self.show_all = False
        self.opened, self.closed = set(), set()   # stacks folded or unfolded by hand
        self.pos, self.kids, self.up, self.hidden = {}, {}, {}, {}
        self.rows, self.hit, self.xy, self.cat_colors, self.scolor = [], {}, {}, {}, {}
        self.width_cells = self.height_cells = 0
        self.drag, self.dragged = None, False

    # ---- layout

    def stack_of(self, branch):
        return layout.stack_of(self.model["nodes"], branch)

    def set_model(self, model, selected):
        self.model = model
        nodes = model["nodes"]
        selected = selected if selected in nodes or selected == ROOT else None
        active = layout.active_stacks(model)
        if selected:
            active.add(self.stack_of(selected))
        unfolded = lambda b: b not in self.closed and (self.show_all or b in active or b in self.opened)
        self.kids, self.hidden = layout.fold(layout.children(nodes), unfolded)
        self.up = {c: p for p, cs in self.kids.items() for c in cs}

        # Lanes are LANE_H rows apart, with GAP rows between base branch families. The
        # repository sits above them, at lane -1 of the keyboard grid.
        self.pos = layout.place(self.kids)
        roots = self.kids[ROOT]
        lanes = 1 + max((ln for ln, _ in self.pos.values()), default=-1)
        family = {ln: i for i, r in enumerate(roots) for ln in range(self.pos[r][0], lanes)}
        lane_y = [TOP + LANE_H * ln + GAP * family[ln] for ln in range(lanes)]
        self.pos[ROOT] = (-1, 0)
        self.name_w = BUS_W + min(20, max((len(b) for b in roots), default=4) + 2)
        self.xy = {b: (self.name_w + col * CW, lane_y[ln]) for b, (ln, col) in self.pos.items()
                   if b != ROOT}
        self.xy[ROOT] = (0, ROOT_Y)
        depth = max((c for _, c in self.pos.values()), default=0)
        self.width_cells = max(self.name_w + (depth + 1) * CW + 4, 80)
        self.height_cells = (lane_y[-1] + LANE_H if lane_y else TOP) + GAP
        self.cat_colors = model["colors"]["category"]
        self.scolor = model["colors"]["session"]
        if selected not in self.pos:
            selected = self.stack_of(selected) if selected else None
        self.selected = selected if selected in self.pos else "main" if "main" in self.pos else ROOT
        self._draw()
        self.virtual_size = Size(self.width_cells, self.height_cells)
        self.refresh()

    def color(self, b):
        return self.cat_colors.get(category(self.model["nodes"][b]), UNTAGGED)

    def edge_style(self, b):
        return edge_style(self.model["nodes"][b], self.cat_colors)

    def _draw(self):
        nodes, sessions = self.model["nodes"], self.model["sessions"]
        width, height = self.width_cells, self.height_cells
        grid = [[(" ", Style(bgcolor=BG)) for _ in range(width)] for _ in range(height)]
        hit = {}

        def put(x, y, text, style, target=None):
            """Writes text from cell x; a wide character takes two cells, the second empty."""
            for ch in text:
                for c in [ch, ""][:cell_width(ch)]:
                    if 0 <= y < height and 0 <= x < width:
                        grid[y][x] = (c, style)
                        if target:
                            hit[(y, x)] = target
                    x += 1

        # Edges first, stations on top. Children are drawn bottom-up so each trunk segment
        # takes the style of the nearest child below it.
        for p, kids in self.kids.items():
            if p == ROOT or not kids:
                continue
            xp, yp = self.xy[p]
            xj = xp + CW - 2
            first = Style(color=self.edge_style(kids[0])[0], bgcolor=BG, bold=True)
            for i in reversed(range(len(kids))):
                k = kids[i]
                xk, yk = self.xy[k]
                color, h, v = self.edge_style(k)
                line = Style(color=color, bgcolor=BG, bold=True)
                if yk == yp:
                    put(xp + 1, yp, h * (xk - xp - 1), line)
                else:
                    # The first child always continues the parent's row.
                    for yy in range(yp + 1, yk):
                        put(xj, yy, v, line)
                    put(xj, yk, "├" if i < len(kids) - 1 else "╰", line)
                    put(xj + 1, yk, h * (xk - xj - 1), line)
            if len(kids) > 1:
                # Last, so the first child's row line does not overwrite the junction.
                put(xj, yp, "┬", first)

        # The bus from the repository to the base branches.
        roots = self.kids[ROOT]
        bus = Style(color=MUTED, bgcolor=BG, bold=True)
        for yy in range(ROOT_Y + 1, self.xy[roots[-1]][1] if roots else 0):
            put(0, yy, "│", bus)
        for r in roots:
            x, y = self.xy[r]
            label = len(r[: self.name_w - BUS_W - 2]) + 2 if nodes[r]["kind"] == "base" else 0
            put(0, y, ("╰" if r == roots[-1] else "├") + "─" * (x - label - 1), bus)

        mine = next((s for s in sessions if s["pane"] == self.me), None)

        def chips(b, x, end, y, name_w=9):
            """Sessions on b other than mine as '●name' in their colour while names fit, then
            bare status icons, then a count."""
            here = sorted((s for s in sessions if layout.at(s, nodes) == b and s is not mine),
                          key=lambda s: -(s["since"] or 0))
            for j, s in enumerate(here):
                icon = STATUS.get(s["status"], STATUS["unknown"])[0]
                chip = f"{icon} {fit(s['label'], name_w)}"
                reserve = 3 if j < len(here) - 1 else 0
                style = Style(color=self.scolor[s["pane"]], bgcolor=BG, bold=True)
                if x + cells(chip) + reserve <= end:
                    put(x, y, chip, style, ("pane", b, s["pane"]))
                    x += cells(chip) + 1
                elif x + 1 + reserve <= end:
                    put(x, y, icon, style, ("pane", b, s["pane"]))
                    x += 1
                else:
                    put(x, y, f"+{len(here) - j}", Style(color=MUTED, bgcolor=BG), b)
                    break

        sel = self.selected == ROOT
        put(0, ROOT_Y, "◈", Style(color=ACCENT if sel else FG, bgcolor=BG, bold=True), ROOT)
        repo = layout.repo_name(self.model)
        put(2, ROOT_Y, repo, Style(color=FG, bgcolor=BG, bold=True, reverse=sel), ROOT)
        chips(ROOT, 4 + len(repo), self.width_cells - 2, ROOT_Y, 24)

        for b, (x, y) in self.xy.items():
            if b == ROOT:
                continue
            n = nodes[b]
            sel = b == self.selected
            if n["kind"] == "base":
                name = b[: self.name_w - BUS_W - 2]
                put(x - len(name) - 1, y, name,
                    Style(color=FG if b == "main" else DIM, bgcolor=BG, bold=True, reverse=sel), b)
                put(x, y, "◆", Style(color=ACCENT if sel else FG, bgcolor=BG, bold=True), b)
                continue
            p, t = n["pr"], n["title"]
            dead = prmap.is_closed(n)
            glyph = "◉" if sel else "✕" if dead else "◆" if n["kind"] == "local" else "●"
            glyph_color = ACCENT if sel else MUTED if dead or n["kind"] == "local" else self.color(b)
            put(x, y, glyph, Style(color=glyph_color, bgcolor=BG, bold=True), b)
            if b in self.hidden:
                put(x + 2, y, f"▸ +{self.hidden[b]}", Style(color=MUTED, bgcolor=BG), b)
            if p:
                number = f"#{p['number']}"
                put(x, y + 1, number, Style(color=BG if sel else FG, bgcolor=ACCENT if sel else BG,
                                            bold=True), b)
                stage = n["stage_label"]
                put(x + len(number) + 1, y + 1, stage,
                    Style(color=MUTED if dead else STAGE_COLORS.get(t["stage"], DIM), bgcolor=BG), b)
                if t["stage"] == "ready" and n.get("synced") is False:
                    put(x + len(number) + len(stage) + 2, y + 1, "!", Style(color=WARN, bgcolor=BG, bold=True), b)
            else:
                put(x, y + 1, "local", Style(color=BG if sel else MUTED, bgcolor=ACCENT if sel else BG,
                                             italic=True), b)
            put(x, y + 2, fit_middle(n["name"], LABEL_W),
                Style(color=MUTED if dead or not p else FG if sel else DIM, bgcolor=BG,
                      strike=dead), b)
            # The YOU flag stands for this pane's own session.
            chips(b, x, x + LABEL_W, y + 3)
            for yy in range(y + 1, y + 3):
                for xx in range(x, x + LABEL_W):
                    hit.setdefault((yy, xx), b)

        you = layout.at(mine, nodes) if mine else None
        if you in self.xy:
            x, y = self.xy[you]
            put(x - 1, y - 1, " ▼ YOU ARE HERE ", Style(color=BG, bgcolor=YOU, bold=True), you)

        self.hit = hit
        self.rows = []
        for row in grid:
            segs, run_text, run_style = [], "", None
            for ch, st in row:
                if st == run_style:
                    run_text += ch
                else:
                    if run_text:
                        segs.append(Segment(run_text, run_style))
                    run_text, run_style = ch, st
            if run_text:
                segs.append(Segment(run_text, run_style))
            self.rows.append(Strip(segs, width))

    # ---- rendering and input

    def render_line(self, y):
        sx, sy = self.scroll_offset
        row = sy + y
        width = self.size.width
        if row >= len(self.rows):
            return Strip.blank(width, Style(bgcolor=BG))
        return self.rows[row].crop(sx, sx + width).extend_cell_length(width, Style(bgcolor=BG))

    def relayout(self):
        if self.model:
            self.set_model(self.model, self.selected)
            self.select(self.selected)

    def select(self, branch, scroll=True, pane=None, jump=False):
        if self.model is None:
            return
        if branch not in self.pos and branch in self.model["nodes"]:
            self.opened.add(self.stack_of(branch))
            self.closed.discard(self.stack_of(branch))
            self.set_model(self.model, branch)
        if branch not in self.pos:
            return
        self.selected = branch
        self._draw()
        self.refresh()
        if scroll:
            # After the next refresh, so the first selection scrolls with the real widget size.
            x, y = self.xy[branch]
            region = Region(max(0, x - self.name_w), max(0, y - 2), CW + self.name_w, LANE_H + 3)
            self.call_after_refresh(self.scroll_to_region, region, animate=False)
        self.post_message(self.Selected(branch, pane, jump))

    def toggle_fold(self):
        if self.selected == ROOT:
            return
        stack = self.stack_of(self.selected)
        if stack in self.hidden:
            self.opened.add(stack)
            self.closed.discard(stack)
        elif self.kids.get(stack):
            self.closed.add(stack)
            self.opened.discard(stack)
            self.selected = stack
        self.relayout()

    def on_mouse_down(self, event):
        if event.button == 1:
            self.drag = (event.screen_x, event.screen_y, self.scroll_x, self.scroll_y)
            self.dragged = False
            self.capture_mouse()

    def on_mouse_move(self, event):
        if self.drag:
            sx, sy, ox, oy = self.drag
            dx, dy = event.screen_x - sx, event.screen_y - sy
            if self.dragged or abs(dx) + abs(dy) >= 2:
                self.dragged = True
                self.scroll_to(ox - dx, oy - dy, animate=False)

    def on_mouse_up(self, event):
        if self.drag:
            self.drag = None
            self.release_mouse()

    def on_click(self, event):
        offset = event.get_content_offset(self)
        if offset is None or self.dragged:
            return
        target = self.hit.get((offset.y + int(self.scroll_y), offset.x + int(self.scroll_x)))
        if isinstance(target, tuple):
            self.select(target[1], scroll=False, pane=target[2], jump=getattr(event, "chain", 1) > 1)
        elif target:
            self.select(target, scroll=False)

    def _step(self, dlane):
        ln, col = self.pos[self.selected]
        cands = [(abs(l2 - ln) + 2 * abs(c2 - col), abs(l2 - ln), b) for b, (l2, c2) in self.pos.items()
                 if (l2 - ln) * dlane > 0]
        if cands:
            self.select(min(cands)[2])

    def key_right(self):
        if self.selected in self.hidden:
            self.toggle_fold()
        kids = self.kids.get(self.selected) or []
        if kids:
            self.select(kids[0])

    def key_left(self):
        if self.selected in self.up:
            self.select(self.up[self.selected])

    def key_down(self):
        self._step(+1)

    def key_up(self):
        self._step(-1)

    key_l, key_h, key_j, key_k = key_right, key_left, key_down, key_up


# ---------------------------------------------------------------- app

class PRMapApp(App):
    CSS = f"""
    Screen {{ background: {BG}; }}
    #title {{ height: 1; background: {BAR}; color: {FG}; padding: 0 1; }}
    #search {{ height: 3; display: none; background: {BAR}; border: tall {GRID}; }}
    #body {{ height: 1fr; }}
    MetroMap {{ width: 1fr; background: {BG}; scrollbar-size: 0 0; }}
    #detail {{ width: 32%; min-width: 52; background: {PANEL}; padding: 0 1; border-left: tall {GRID};
               scrollbar-size: 1 1; scrollbar-background: {PANEL}; scrollbar-color: {GRID}; }}
    .section {{ color: {MUTED}; text-style: bold; margin-top: 1; }}
    #sessions {{ height: auto; max-height: 20; background: {PANEL}; border: none; padding: 0; }}
    #sessions:focus {{ border: none; }}
    #sessions > .option-list--option-highlighted {{ background: #2e3c64; }}
    #legend {{ height: auto; max-height: 13; background: {BAR}; color: {FG}; padding: 0 1; }}
    """
    BINDINGS = [Binding("q", "quit", "quit"), Binding("escape", "back", show=False),
                Binding("r", "refetch", "refetch PRs"), Binding("tab", "swap", "switch pane", priority=True),
                Binding("enter", "enter", show=False), Binding("o,space", "fold", show=False),
                Binding("a", "all", show=False), Binding("f", "follow", show=False),
                Binding("slash", "search", show=False)] + \
               [Binding(str(i), f"session({i})", show=False) for i in range(1, 10)]

    def __init__(self, popup=False):
        super().__init__()
        self.popup = popup
        self.model = None
        self.follow = False
        self.session_rows = []
        self.shown = None
        self.me = os.environ.get("HERDR_ACTIVE_PANE_ID") or os.environ.get("HERDR_PANE_ID") or ("w1:p1" if prmap.DEMO else None)
        panel = [os.environ.get(k) for k in ("HERDR_PANEL_COLUMN", "HERDR_PANEL_LINE",
                                             "HERDR_PANEL_WIDTH", "HERDR_PANEL_HEIGHT")]
        self.clicked = tuple(int(v) for v in panel) if all(panel) else None

    def compose(self) -> ComposeResult:
        yield Static("PR map · loading…", id="title")
        yield Input(placeholder="PR number, branch, title words or session name", id="search")
        with Horizontal(id="body"):
            yield MetroMap(id="map")
            with VerticalScroll(id="detail"):
                yield Static(id="head")
                yield Static("SESSIONS", classes="section")
                yield OptionList(id="sessions")
                yield Static(id="commits-title", classes="section")
                yield Static(id="commits")
        yield Static(id="legend")

    def on_mount(self):
        self.query_one(MetroMap).focus()
        self.reload()
        self.set_interval(5, self.reload)
        self.set_interval(30, self._title)

    @work(thread=True, exclusive=True, group="reload")
    def reload(self, force=False):
        model = prmap.load(force, frame=not force and self.model is None)
        if model is None and self.model is None and os.environ.get("HERDR_PANEL_FRAME_ID"):
            self.call_from_thread(self.notify, "Panel frame expired; showing the current map", severity="warning")
            model = prmap.load(force)
        if model:
            self.call_from_thread(self.apply, model)
        else:
            self.call_from_thread(self.notify, "PR map backend failed; showing the last model",
                                  severity="warning")

    def mine(self):
        sessions = self.model["sessions"] if self.model else []
        return next((s for s in sessions if s["pane"] == self.me), None) or \
            next((s for s in sessions if s["focused"]), None)

    def apply(self, model):
        first = self.model is None
        self.model = model
        m = self.query_one(MetroMap)
        mine = self.mine()
        m.me = mine["pane"] if mine else None
        target = m.selected
        if (first or self.follow) and mine:
            target = layout.at(mine, model["nodes"])
        if first and self.clicked:
            # The cells the panel drew, so the click names the station it showed.
            target = prmap.clicked_station(*self.clicked) or target
        m.set_model(model, target)
        if first or self.follow:
            m.select(m.selected)
        self._title()
        self._legend()
        self.show(m.selected)

    def _title(self):
        if not self.model:
            return
        nodes = self.model["nodes"]
        prs = sum(1 for n in nodes.values() if n["pr"] and not prmap.is_closed(n))
        m = self.query_one(MetroMap)
        t = Text()
        t.append("PR map  ", Style(bold=True, color=FG))
        mine = self.mine()
        if mine:
            t.append(" ▼ YOU ", Style(color=BG, bgcolor=YOU, bold=True))
            n = nodes.get(mine["current"])
            t.append(f" {mine['label']} → {mine['station']}    " if n
                     else f" {mine['label']} → {layout.repo_name(self.model)}, no PR    ",
                     Style(color=YOU, bold=True))
        t.append(f"{prs} open · fetched {ago(self.model['fetched_at'])} · "
                 f"{datetime.now(TZ).strftime('%H:%M %Z')}    ", Style(color=MUTED))
        for key, label, on in (("a", "all stacks", m.show_all), ("f", "follow", self.follow)):
            t.append(f"{key} ", Style(color=MUTED))
            t.append(f"{label} {'on' if on else 'off'}", Style(color=ACCENT if on else DIM))
            t.append(" · ", Style(color=GRID))
        t.append("/ search · o fold · tab sessions · enter jump · r refetch · q quit", Style(color=MUTED))
        self.query_one("#title", Static).update(t)

    def _legend(self):
        t = Text()
        nodes = self.model["nodes"]
        mine = self.mine()
        m = self.query_one(MetroMap)
        for i, s in enumerate(self.model["sessions"]):
            icon, c = STATUS.get(s["status"], STATUS["unknown"])
            n = nodes.get(s["current"]) if s["current"] else None
            t.append(f"{i + 1} ", Style(color=MUTED))
            t.append(f"{icon} {pad(s['label'], 24)} ", Style(color=m.scolor[s["pane"]], bold=True))
            t.append(" YOU " if s is mine else "     ", Style(color=BG, bgcolor=YOU, bold=True)
                     if s is mine else Style(color=MUTED))
            t.append(f" {s['status']:<8}", Style(color=c))
            if n:
                where = f"#{n['pr']['number']} {n['name']}" if n["pr"] else f"{n['name']} (local)"
                t.append(f"→ {pad(where, 34)} ", Style(color=FG))
                t.append(s["how"] + (f" {ago(s['since'])}" if s["since"] else ""), Style(color=MUTED))
            else:
                t.append(f"→ {layout.repo_name(self.model)}, no PR", Style(color=MUTED))
            t.append("\n")
        if self.model["elsewhere"]:
            t.append(outside(self.model) + "\n", Style(color=MUTED))
        if not self.model["herdr"]:
            t.append("herdr snapshot unavailable: sessions not shown\n", Style(color=WARN))
        for sample, color, meaning in (("──", FG, "ready for review"), ("╌╌", FG, "draft"),
                                       ("──", WARN, "needs rebase"), ("··", MUTED, "local branch"),
                                       ("─✕", MUTED, "closed without merging")):
            t.append(f"{sample} ", Style(color=color, bold=True))
            t.append(f"{meaning}  ", Style(color=DIM))
        self.query_one("#legend", Static).update(t)

    def on_metro_map_selected(self, message):
        self.show(message.branch, message.pane)
        if message.jump and message.pane:
            self._jump(next(s for s in self.model["sessions"] if s["pane"] == message.pane))

    def show(self, branch, pane=None):
        if not self.model or (branch not in self.model["nodes"] and branch != ROOT):
            return
        nodes, sessions = self.model["nodes"], self.model["sessions"]
        n = nodes.get(branch)
        p, tp = (n["pr"], n["title"]) if n else (None, None)
        head = Text()
        if branch == ROOT:
            kids = layout.children(nodes)
            stacks = sum(len(kids.get(r, [])) for r in kids[ROOT])
            open_prs = sum(1 for m in nodes.values() if m["pr"] and not prmap.is_closed(m))
            head.append(f"{self.model['repo']}\n", Style(bold=True, color=FG))
            head.append(f"repository · {open_prs} open PRs in {stacks} stacks from "
                        f"{len(kids[ROOT])} base branches\n", Style(color=MUTED))
            head.append("sessions in the workspace on no PR branch stand here\n", Style(color=MUTED))
            if self.model["elsewhere"]:
                head.append(outside(self.model), Style(color=MUTED))
        elif p:
            head.append(f"#{p['number']}  ", Style(bold=True, color=FG))
            cat = category(n)
            if cat:
                c = self.query_one(MetroMap).cat_colors.get(cat, UNTAGGED)
                head.append("━━ ", Style(color=c, bold=True))
                head.append(" · ".join(x for x in (tp["category"], tp["domain"]) if x), Style(color=FG))
            else:
                head.append(f"untagged · {branch.split('/')[0] if '/' in branch else 'no prefix'}",
                            Style(color=MUTED))
            head.append(f"\n{tp['summary']}\n", Style(color=FG, bold=True))
            head.append(branch, Style(color=DIM))
            head.append(f" ← {n['parent']}\n\n", Style(color=MUTED))
            review = {"APPROVED": ("approved", "#9ece6a"), "CHANGES_REQUESTED": ("changes requested", WARN),
                      "REVIEW_REQUIRED": ("review required", DIM)}.get(p.get("reviewDecision"), ("none", MUTED))
            closed_at = prmap.epoch(p["closedAt"]) if prmap.is_closed(n) else None
            state = (f"closed without merging {ago(closed_at)}", MUTED) if closed_at else \
                ("draft", DIM) if p["isDraft"] else ("open, ready for review", FG)
            sync = {True: ("contains its base", DIM), False: ("needs a rebase onto its base", WARN),
                    None: ("not checked", MUTED)}[n.get("synced")]
            updated = datetime.fromisoformat(p["updatedAt"].replace("Z", "+00:00")).timestamp()
            for label, value, color in (
                    ("State", state[0], state[1]),
                    ("Stage", (tp["stage"] or "—").capitalize() +
                     (f"  [{tp['position']}]" if tp["position"] else ""), STAGE_COLORS.get(tp["stage"], FG)),
                    ("Review", review[0], review[1]),
                    ("Base", sync[0], sync[1]),
                    ("Size", f"{n.get('commits') or 0} commits · updated {ago(updated)}", DIM)):
                head.append(f"{label:<8}", Style(color=MUTED))
                head.append(f"{value}\n", Style(color=color))
            for h in n["hints"]:
                head.append(f"! {h}\n", Style(color=WARN))
            head.append(p["url"], Style(color=MUTED, link=p["url"]))
        elif n["kind"] == "local":
            head.append(branch, Style(bold=True, color=FG))
            parent = nodes.get(n["parent"]) if n["parent"] else None
            on = f"#{parent['pr']['number']}" if parent and parent["pr"] else (n["parent"] or "?")
            head.append(f"\nlocal branch without a PR · contains the head of {on}", Style(color=MUTED))
        else:
            kids = sum(1 for m in nodes.values() if m["parent"] == branch)
            head.append(branch, Style(bold=True, color=FG))
            head.append(f"\nbase branch · {kids} stacks start here", Style(color=MUTED))
        self.query_one("#head", Static).update(head)

        width = max(30, self.query_one("#detail").content_size.width - 1)
        ol = self.query_one("#sessions", OptionList)
        if pane is None and ol.highlighted is not None and branch == self.shown:
            pane = ol.get_option_at_index(ol.highlighted).id
        self.shown = branch
        ol.clear_options()
        self.session_rows = []
        mine = self.mine()
        scolor = self.query_one(MetroMap).scolor
        groups = (("working here", lambda s: layout.at(s, nodes) == branch),
                  ("recently visited", lambda s: s["recent"] == branch),
                  ("earlier", lambda s: branch in s["earlier"]))
        for title, test in groups:
            group = [s for s in sessions if test(s)]
            if not group:
                continue
            ol.add_option(Option(Text(title, Style(color=MUTED, italic=True)), disabled=True))
            for s in sorted(group, key=lambda s: -(explain(s, branch)[0] or 0)):
                faint = title == "earlier"
                icon, c = STATUS.get(s["status"], STATUS["unknown"])
                t = Text()
                t.append(f"{icon} {s['label']} ", Style(color=MUTED if faint else scolor[s["pane"]], bold=True))
                if s is mine:
                    t.append(" YOU ", Style(color=BG, bgcolor=YOU, bold=True))
                t.append(f" {s['status']}", Style(color=c))
                _, why = explain(s, branch)
                if why:
                    t.append(f"\n     {fit(why, width - 6)}", Style(color=MUTED))
                ol.add_option(Option(t, id=s["pane"]))
                self.session_rows.append(s)
        if not self.session_rows:
            ol.add_option(Option(Text("no live session here", Style(color=MUTED)), disabled=True))
        elif pane:
            ol.highlighted = next((i for i in range(ol.option_count)
                                   if ol.get_option_at_index(i).id == pane), None)

        rows = prmap.commits(branch) if n else []
        by_badge = {s["badge"]: s for s in sessions}
        title = Text("COMMITS", Style(color=MUTED, bold=True))
        if any(r[4] for r in rows):
            title.append("   name: its commit output shows the sha   ~name: by time only",
                         Style(color=MUTED))
        self.query_one("#commits-title", Static).update(title)
        lines = Text()
        for graph, sha, ct, subject, badge, confirmed in rows:
            lines.append(graph, Style(color=MUTED))
            if sha:
                lines.append(f"{sha} ", Style(color=DIM))
                lines.append(fit(subject, width - len(graph) - 28), Style(color=FG))
                lines.append(f"  {age(ct)}", Style(color=MUTED))
                if badge:
                    s = by_badge[badge]
                    lines.append(" ")
                    lines.append(fit(s["label"], 12) if confirmed else f"~{fit(s['label'], 11)}",
                                 Style(color=scolor[s["pane"]], bold=confirmed, italic=not confirmed))
            lines.append("\n")
        lines.rstrip()
        self.query_one("#commits", Static).update(
            lines if lines.plain or not n else Text("no commits of its own", Style(color=MUTED)))

    def _jump(self, s):
        if prmap.focus_pane(s["pane"]):
            if self.popup:
                self.exit()
            else:
                self.notify(f"focused {s['label']}")
        else:
            self.notify(f"could not focus {s['pane']}", severity="error")

    def on_option_list_option_selected(self, event):
        if event.option_list.id == "sessions" and event.option.id:
            self._jump(next(s for s in self.model["sessions"] if s["pane"] == event.option.id))

    def _focus_sessions(self):
        ol = self.query_one("#sessions", OptionList)
        ol.focus()
        if ol.highlighted is None or ol.get_option_at_index(ol.highlighted).disabled:
            ol.highlighted = next((i for i in range(ol.option_count)
                                   if not ol.get_option_at_index(i).disabled), None)

    def action_swap(self):
        if self.query_one("#sessions", OptionList).has_focus:
            self.query_one(MetroMap).focus()
        else:
            self._focus_sessions()

    def action_enter(self):
        if self.query_one(MetroMap).has_focus and self.session_rows:
            self._focus_sessions()

    def action_back(self):
        search = self.query_one("#search", Input)
        if search.display:
            search.display = False
            self.query_one(MetroMap).focus()
        elif self.query_one("#sessions", OptionList).has_focus:
            self.query_one(MetroMap).focus()
        else:
            self.exit()

    def action_session(self, i):
        if self.model and i <= len(self.model["sessions"]):
            s = self.model["sessions"][i - 1]
            m = self.query_one(MetroMap)
            m.focus()
            m.select(layout.at(s, self.model["nodes"]), pane=s["pane"])

    def action_fold(self):
        m = self.query_one(MetroMap)
        if m.has_focus:
            m.toggle_fold()

    def action_all(self):
        m = self.query_one(MetroMap)
        m.show_all = not m.show_all
        m.opened.clear()
        m.closed.clear()
        m.relayout()
        self._title()

    def action_follow(self):
        self.follow = not self.follow
        self._title()
        mine = self.mine()
        if self.follow and mine:
            self.query_one(MetroMap).select(layout.at(mine, self.model["nodes"]))
        elif self.follow:
            self.notify("this pane has no session on the map", severity="warning")

    def action_search(self):
        search = self.query_one("#search", Input)
        search.display = True
        search.value = ""
        search.focus()

    def on_input_submitted(self, event):
        q = event.value.strip().lstrip("#").lower()
        event.input.display = False
        m = self.query_one(MetroMap)
        m.focus()
        if not q or not self.model:
            return
        nodes = self.model["nodes"].values()
        hits = [n["branch"] for n in nodes if n["pr"] and str(n["pr"]["number"]) == q] or \
               [n["branch"] for n in nodes if q in n["branch"].lower() or
                (n["pr"] and q in n["pr"]["title"].lower())] or \
               [layout.at(s, self.model["nodes"]) for s in self.model["sessions"] if q in s["label"].lower()]
        if hits:
            m.select(hits[0])
        else:
            self.notify(f"no station matches {event.value!r}", severity="warning")

    def action_refetch(self):
        self.notify("refetching PRs…")
        self.reload(force=True)
