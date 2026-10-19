"""Stack layout of the full map; the sidebar minimap (pr-map-rs/src/layout.rs) keeps the same
rules.

The repository is the root of the map: its children are the base branches the stacks start from
(main first), and a session working in the workspace on no branch of the map stands on it. Each
station below it gets a lane (row) and a column (depth below the base branches); a station's
first child continues its lane and later children open lanes below.
"""

from . import prmap

ROOT = ":repo"                       # ':' never occurs in a branch name


def repo_name(model):
    """Repository name displayed at the root station."""
    return model["repo"].split("/")[-1]


def at(session, nodes):
    """The station a session stands on: its branch, or ROOT when it is on no branch of the map."""
    return session["current"] if session["current"] in nodes else ROOT


def children(nodes):
    """{parent: children} in map order, ROOT's being the base branches with main first.

    Below a base branch: open PRs by number, then closed ones, then local branches.
    """
    kids = {ROOT: []}
    for n in nodes.values():
        kids.setdefault(n["parent"] if n["parent"] in nodes else ROOT, []).append(n["branch"])
    order = lambda b: (nodes[b]["kind"] == "local", prmap.is_closed(nodes[b]),
                       nodes[b]["pr"]["number"] if nodes[b]["pr"] else 0, b)
    for parent, v in kids.items():
        v.sort(key=(lambda b: (b != "main", b)) if parent == ROOT else order)
    return kids


def stack_of(nodes, branch):
    """The station a branch's stack folds into: its ancestor right below a base branch."""
    while branch in nodes and nodes[branch]["parent"] in nodes and \
            nodes[nodes[branch]["parent"]]["parent"] in nodes:
        branch = nodes[branch]["parent"]
    return branch


def active_stacks(model):
    """Stacks with a session on them."""
    nodes = model["nodes"]
    return {stack_of(nodes, s["current"]) for s in model["sessions"] if s["current"] in nodes}


def fold(kids, unfolded):
    """Cuts each stack for which unfolded(stack) is false down to its first station.

    Returns:
      (kids of the shown stations, {folded stack: number of stations hidden behind it}).
    """
    shown, hidden = {}, {}

    def size(b):
        return sum(1 + size(k) for k in kids.get(b, []))

    def visit(b, depth):
        ks = kids.get(b, [])
        if depth == 2 and ks and not unfolded(b):
            hidden[b] = size(b)
            ks = []
        shown[b] = ks
        for k in ks:
            visit(k, depth + 1)

    visit(ROOT, 0)
    return shown, hidden


def place(kids):
    """{station: (lane, column)} for every station below ROOT; base branches are column 0."""
    pos = {}

    def visit(b, lane, col):
        pos[b] = (lane, col)
        last = lane
        for i, k in enumerate(kids.get(b, [])):
            last = visit(k, lane if i == 0 else last + 1, col + 1)
        return last

    lane = 0
    for r in kids[ROOT]:
        lane = visit(r, lane, 0) + 1
    return pos
