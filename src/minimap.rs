//! Sidebar minimap: the full PR map around the focused pane's session, in ANSI colours.
//!
//! `prmap minimap` is the command of the herdr fork's [ui.sidebar.panel]. herdr runs it with
//! COLUMNS and LINES set to the panel body and HERDR_ACTIVE_PANE_ID set to the focused pane, and
//! shows the output between Spaces and Agents. The mouse wheel over the panel steps
//! HERDR_PANEL_ZOOM through ZOOMS, and dragging it pans the view by HERDR_PANEL_DRAG_X and
//! HERDR_PANEL_DRAG_Y cells. Each run leaves the cells it drew a station in at
//! an immutable frame record, where the full map opened by a click on the panel looks up
//! the clicked station.
//!
//! The layout is the full map's (layout.rs) at a smaller scale, picked from ZOOMS: the
//! repository ◈ on top, a bus down the left edge to the base branches and each stack running
//! right, in the full map's line styles. Zoomed in, each station carries '#N' and its name in
//! full (further in: '#N Stage', the name and the sessions there), a station's children starting
//! right past its labels; stacks without a session are folded (+N) as in the full map, and the
//! view shows the focused pane's session, marked by a pink cell (the ◈ itself when it is in the
//! workspace on no PR branch). Other sessions colour their stations or follow them as dots, and
//! an arrow in a session's colour on the edge points to one out of view. Zoomed out to one cell
//! per station, stacks fold only as far as needed to fit. The last row names the focused
//! session's position.

use std::collections::{BTreeSet, HashMap, HashSet};

use indexmap::IndexMap;

use crate::layout::{self, Kids, ROOT};
use crate::model::{Kind, Model, Node, Session};
use crate::style::{self, cell_width, cells, fit, Rgb, ACCENT, BAR, DIM, FG, MUTED, UNTAGGED, YOU};

/// (rows per lane, label rows under a station): one cell per station; '#N' and the name;
/// '#N Stage', the name and the sessions there.
const ZOOMS: [(i64, i64); 3] = [(1, 0), (3, 2), (4, 3)];
const DEFAULT_ZOOM: i64 = 1;
/// Cells between station columns when zoomed out.
const DOT_PITCH: i64 = 3;
const MAP_BG: Rgb = BAR;

#[derive(Clone, Debug)]
struct Cell {
    /// One character, '' for the second cell of a wide one.
    ch: String,
    fg: Option<Rgb>,
    bold: bool,
    bg: Option<Rgb>,
}

impl Cell {
    fn new(ch: &str, fg: Option<Rgb>, bold: bool) -> Cell {
        Cell {
            ch: ch.to_string(),
            fg,
            bold,
            bg: Some(MAP_BG),
        }
    }
}

/// A run of text in one style.
#[derive(Clone, Debug, PartialEq)]
pub struct Seg {
    pub text: String,
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub bold: bool,
}

fn seg(text: &str, fg: Option<Rgb>, bg: Option<Rgb>, bold: bool) -> Seg {
    Seg {
        text: text.to_string(),
        fg,
        bg,
        bold,
    }
}

/// {(column, line): station} of the cells a click opens (a branch or ROOT).
pub type Hit = HashMap<(i64, i64), String>;

/// The canvas the map is drawn on, with room for a view hanging past the map's far edges.
struct Canvas {
    grid: Vec<Vec<Cell>>,
    hit: Hit,
    rows: i64,
    cols: i64,
}

impl Canvas {
    /// Writes text from cell x; a wide character takes two cells, the second left empty.
    // A cell write carries position, text, style and an optional station hit target together.
    #[allow(clippy::too_many_arguments)]
    fn put(
        &mut self,
        mut x: i64,
        y: i64,
        text: &str,
        fg: Rgb,
        bold: bool,
        target: Option<&str>,
        bg: Rgb,
    ) {
        let row_ok = 0 <= y && y < self.rows;
        for ch in text.chars() {
            let w = cell_width(ch);
            if w == 0 && row_ok && 0 < x && x <= self.cols {
                self.grid[y as usize][x as usize - 1].ch.push(ch);
            }
            for c in [ch.to_string(), String::new()].into_iter().take(w) {
                if row_ok && 0 <= x && x < self.cols {
                    self.grid[y as usize][x as usize] = Cell {
                        ch: c,
                        fg: Some(fg),
                        bold,
                        bg: Some(bg),
                    };
                    if let Some(t) = target {
                        self.hit.insert((x, y), t.to_string());
                    }
                }
                x += 1;
            }
        }
    }

    fn put_plain(&mut self, x: i64, y: i64, text: &str, fg: Rgb) {
        self.put(x, y, text, fg, false, None, MAP_BG);
    }

    fn ch(&self, x: i64, y: i64) -> &str {
        &self.grid[y as usize][x as usize].ch
    }
}

fn faint(color: Rgb, t: f64) -> Rgb {
    style::blend(color, MAP_BG, t)
}

fn number_of(node: &Node) -> String {
    node.pr
        .as_ref()
        .map_or_else(|| "local".to_string(), |p| format!("#{}", p.number))
}

/// Blanks the half of a wide character that the view's edge or an indicator cut off.
fn mend(row: &mut [Cell]) {
    let wide = |ch: &str| ch.chars().next().is_some_and(|c| cell_width(c) == 2);
    for i in 0..row.len() {
        if (row[i].ch.is_empty() && (i == 0 || !wide(&row[i - 1].ch)))
            || (wide(&row[i].ch) && (i + 1 == row.len() || !row[i + 1].ch.is_empty()))
        {
            row[i].ch = " ".into();
        }
    }
}

/// Draws the minimap at a zoom level of ZOOMS, its view dragged by `drag` cells.
///
/// Returns:
///   (rows, hit): at most `height` rows of segments, and the cells a click opens.
pub fn render(
    model: &Model,
    pane: Option<&str>,
    width: i64,
    height: i64,
    zoom: usize,
    drag: (i64, i64),
) -> (Vec<Vec<Seg>>, Hit) {
    let (nodes, sessions, cats) = (&model.nodes, &model.sessions, &model.colors.category);
    let scol = |s: &Session| {
        model
            .colors
            .session
            .get(&s.pane)
            .map_or(FG, |c| style::hex(c))
    };
    let mine = sessions.iter().find(|s| Some(s.pane.as_str()) == pane);
    let is_mine = |s: &Session| mine.is_some_and(|m| std::ptr::eq(m, s));
    let you: Option<&str> = mine.map(|s| layout::at(s, nodes));
    let mut by_time: Vec<&Session> = sessions.iter().collect();
    by_time.sort_by(|a, b| {
        b.place
            .since
            .unwrap_or(0.0)
            .total_cmp(&a.place.since.unwrap_or(0.0))
    });
    let mut here: IndexMap<&str, Vec<&Session>> = IndexMap::new();
    for s in by_time {
        here.entry(layout::at(s, nodes)).or_default().push(s);
    }
    let (lane_h, labels) = ZOOMS[zoom];
    let avail = (height - 1).max(1);

    let full = layout::children(nodes);
    let active = layout::active_stacks(model);
    let (kids, hidden): (Kids, IndexMap<String, usize>) = if labels > 0 {
        layout::fold(&full, |b| active.contains(b))
    } else {
        // Fold stacks without a session, largest first, until the lanes fit.
        fn leaves(full: &Kids, b: &str) -> usize {
            full.get(b)
                .map_or(0, |ks| ks.iter().map(|k| leaves(full, k)).sum::<usize>())
                .max(1)
        }
        let mut stacks: Vec<&String> = full[ROOT]
            .iter()
            .flat_map(|r| full.get(r).into_iter().flatten())
            .filter(|k| !active.contains(*k) && full.get(*k).is_some_and(|v| !v.is_empty()))
            .collect();
        stacks.sort_by_key(|b| std::cmp::Reverse(leaves(&full, b)));
        let mut folded: HashSet<&str> = HashSet::new();
        loop {
            let (k, h) = layout::fold(&full, |b| !folded.contains(b));
            let lanes: HashSet<i64> = layout::place(&k).values().map(|(lane, _)| *lane).collect();
            if (lanes.len() as i64) < avail || folded.len() == stacks.len() {
                break (k, h);
            }
            folded.insert(stacks[folded.len()]);
        }
    };
    let pos = layout::place(&kids);
    let depth = pos.values().map(|(_, col)| *col).max().unwrap_or(0);
    let others_at = |b: &str| -> Vec<&Session> {
        here.get(b).map_or_else(Vec::new, |on| {
            on.iter().copied().filter(|s| !is_mine(s)).collect()
        })
    };
    let more = |others: &[&Session]| {
        if others.len() > 1 {
            format!(" +{}", others.len() - 1)
        } else {
            String::new()
        }
    };
    // Cells the labels under station b take uncut.
    let label_cells = |b: &str| -> i64 {
        let n = &nodes[b];
        if n.kind == Kind::Base {
            return cells(b);
        }
        let number = number_of(n).len() as i64;
        let mut wide = number.max(cells(&n.name));
        if labels == 3 {
            let others = others_at(b);
            let sessions_row = others
                .first()
                .map_or(0, |s| cells(&s.label) + more(&others).len() as i64);
            wide = wide
                .max(number + 1 + n.stage_label.len() as i64)
                .max(sessions_row);
        }
        wide
    };

    // Zoomed in, names show in full up to the panel's width: a station's children start right
    // past its labels and the trunk to its later children, so every lane's spacing follows its
    // own names. Lanes stay apart, and a trunk only passes left of its parent's descendants.
    let mut xy: IndexMap<String, (i64, i64)> = IndexMap::new();
    let mut room: HashMap<String, i64> = HashMap::new();
    if labels > 0 {
        room = pos
            .keys()
            .map(|b| (b.clone(), (width - 4).max(7).min(label_cells(b))))
            .collect();
        fn visit(
            b: &str,
            x: i64,
            kids: &Kids,
            pos: &IndexMap<String, (i64, i64)>,
            room: &HashMap<String, i64>,
            lane_h: i64,
            xy: &mut IndexMap<String, (i64, i64)>,
        ) {
            xy.insert(b.to_string(), (x, 1 + lane_h * pos[b].0));
            for k in kids.get(b).into_iter().flatten() {
                visit(k, x + room[b] + 2, kids, pos, room, lane_h, xy);
            }
        }
        for r in &kids[ROOT] {
            visit(r, 2, &kids, &pos, &room, lane_h, &mut xy);
        }
    } else {
        let pitch = if 3 + 3 * depth <= width { DOT_PITCH } else { 2 };
        for (b, (lane, col)) in &pos {
            xy.insert(b.clone(), (2 + pitch * col, 1 + lane_h * lane));
            room.insert(b.clone(), 0);
        }
    }
    xy.insert(ROOT.into(), (0, 0));
    room.insert(ROOT.into(), 0);
    let content_h = lane_h + xy.values().map(|(_, y)| *y).max().unwrap_or(0);
    let content_w = 1 + xy
        .iter()
        .map(|(b, (x, _))| x + room[b].max(3))
        .max()
        .unwrap_or(0);
    let (rows_n, cols_n) = (content_h + avail, content_w + width);
    let mut c = Canvas {
        grid: vec![
            vec![
                Cell {
                    ch: " ".into(),
                    fg: None,
                    bold: false,
                    bg: Some(MAP_BG)
                };
                cols_n as usize
            ];
            rows_n as usize
        ],
        hit: Hit::new(),
        rows: rows_n,
        cols: cols_n,
    };

    // The bus from the repository to the base branches.
    let roots = &kids[ROOT];
    let bus = faint(DIM, 0.4);
    if let Some(last) = roots.last() {
        for y in 1..xy[last].1 {
            c.put_plain(0, y, "│", bus);
        }
        for r in roots {
            c.put_plain(0, xy[r].1, if r == last { "╰─" } else { "├─" }, bus);
        }
    }

    // Lines, bottom-up so a trunk segment takes the style of the nearest child below it.
    for (p, ks) in &kids {
        if p == ROOT || ks.is_empty() {
            continue;
        }
        let (xp, yp) = xy[p];
        let xj = xp + if labels > 0 { room[p] } else { 1 };
        for (i, k) in ks.iter().enumerate().rev() {
            let (xk, yk) = xy[k];
            let (color, h, v) = style::edge_style(&nodes[k], cats);
            let line = |n: i64| h.repeat(n.max(0) as usize);
            if yk == yp {
                c.put_plain(xp + 1, yp, &line(xk - xp - 1), faint(color, 0.35));
            } else {
                for y in yp + 1..yk {
                    c.put_plain(xj, y, v, faint(color, 0.35));
                }
                let elbow = if i < ks.len() - 1 { "├" } else { "╰" };
                c.put_plain(
                    xj,
                    yk,
                    &format!("{elbow}{}", line(xk - xj - 1)),
                    faint(color, 0.35),
                );
            }
        }
        if ks.len() > 1 {
            c.put_plain(
                xj,
                yp,
                "┬",
                faint(style::edge_style(&nodes[&ks[0]], cats).0, 0.35),
            );
        }
    }

    // Stations: plain ones recede, sessions stand out. Zoomed in, '#N Stage' and the name go
    // under each station.
    for (b, &(x, y)) in &xy {
        if b == ROOT {
            continue;
        }
        let (n, on) = (
            &nodes[b],
            here.get(b.as_str()).map_or(&[][..], Vec::as_slice),
        );
        let (p, dead, mine_here) = (&n.pr, n.is_closed(), you == Some(b.as_str()));
        let t = Some(b.as_str());
        if mine_here {
            c.put(
                x,
                y,
                if n.kind == Kind::Base { "◆" } else { "◉" },
                MAP_BG,
                true,
                t,
                YOU,
            );
        } else if n.kind == Kind::Base {
            let color = on
                .first()
                .map_or(if b == "main" { FG } else { DIM }, |s| scol(s));
            c.put(x, y, "◆", color, true, t, MAP_BG);
        } else if let Some(s) = on.first() {
            c.put(x, y, "●", scol(s), true, t, MAP_BG);
        } else if dead {
            c.put(x, y, "×", MUTED, false, t, MAP_BG);
        } else if n.kind == Kind::Local {
            c.put(x, y, "◦", MUTED, false, t, MAP_BG);
        } else {
            let color = n
                .category()
                .and_then(|k| cats.get(k))
                .map_or(UNTAGGED, |k| style::hex(k));
            c.put(
                x,
                y,
                if labels > 0 { "●" } else { "•" },
                color,
                false,
                t,
                MAP_BG,
            );
        }
        if let Some(count) = hidden.get(b) {
            c.put(
                x + 1,
                y,
                &if labels > 0 {
                    format!("+{count}")
                } else {
                    "+".into()
                },
                MUTED,
                false,
                t,
                MAP_BG,
            );
        }
        if labels == 0 {
            continue;
        }
        // The other sessions here as dots in their colours, as beside the repository's name.
        let extra: Vec<&Session> = on
            .iter()
            .copied()
            .filter(|s| !is_mine(s))
            .skip(if mine_here { 0 } else { 1 })
            .collect();
        for (i, s) in extra.iter().take(3).enumerate() {
            c.put(x + 1 + i as i64, y, "●", scol(s), true, t, MAP_BG);
        }
        if extra.len() > 3 {
            c.put(
                x + 4,
                y,
                &format!("+{}", extra.len() - 3),
                MUTED,
                false,
                t,
                MAP_BG,
            );
        }
        let w = room[b];
        if n.kind == Kind::Base {
            c.put(
                x,
                y + 1,
                &fit(b, w),
                if b == "main" { FG } else { DIM },
                b == "main",
                t,
                MAP_BG,
            );
            continue;
        }
        let number = number_of(n);
        let stage_color = if dead {
            MUTED
        } else {
            style::stage_color(n.title.stage.as_deref()).unwrap_or(FG)
        };
        if mine_here {
            c.put(x, y + 1, &number, MAP_BG, true, t, YOU);
        } else {
            let color = if p.is_none() {
                MUTED
            } else if labels == 2 {
                stage_color
            } else {
                FG
            };
            c.put(x, y + 1, &number, color, true, t, MAP_BG);
        }
        let stage = fit(&n.stage_label, w - number.len() as i64 - 1);
        if labels == 3 && !stage.is_empty() {
            c.put(
                x + number.len() as i64,
                y + 1,
                &format!(" {stage}"),
                stage_color,
                false,
                t,
                MAP_BG,
            );
        }
        let name_color = if dead || p.is_none() {
            MUTED
        } else if mine_here {
            FG
        } else {
            DIM
        };
        c.put(x, y + 2, &fit(&n.name, w), name_color, mine_here, t, MAP_BG);
        let others = others_at(b);
        if let (3, Some(first)) = (labels, others.first()) {
            let name = fit(&first.label, w - more(&others).len() as i64);
            c.put(x, y + 3, &name, scol(first), true, t, MAP_BG);
            c.put(
                x + cells(&name),
                y + 3,
                &more(&others),
                MUTED,
                false,
                t,
                MAP_BG,
            );
        }
    }

    // The repository, with the sessions on no PR branch beside its name.
    if you == Some(ROOT) {
        c.put(0, 0, "◈", MAP_BG, true, Some(ROOT), YOU);
    } else {
        c.put(0, 0, "◈", ACCENT, true, Some(ROOT), MAP_BG);
    }
    let name = fit(layout::repo_name(&model.repo), width - 3);
    c.put(2, 0, &name, FG, true, Some(ROOT), MAP_BG);
    for (i, s) in others_at(ROOT).iter().enumerate() {
        c.put(
            3 + cells(&name) + i as i64,
            0,
            "●",
            scol(s),
            true,
            Some(ROOT),
            MAP_BG,
        );
    }

    // The view: of the windows showing the focused session and its labels, the one with the
    // most map in it, closer to centred on a tie. Its edges fall on a lane's station row and
    // right before a station, so the focused lane's labels are not cut.
    let (fx, fy) = you.and_then(|y| xy.get(y)).copied().unwrap_or((0, 0));
    let you_room = you.and_then(|y| room.get(y)).copied().unwrap_or(0);
    let tops: Vec<i64> = std::iter::once(0)
        .chain((0..=content_h / lane_h).map(|ln| 1 + lane_h * ln))
        .collect();
    let lefts: BTreeSet<i64> = std::iter::once(0)
        .chain(xy.values().filter(|(x, _)| *x != 0).map(|(x, _)| x - 1))
        .collect();
    let filled: Vec<Vec<bool>> = c
        .grid
        .iter()
        .map(|row| row.iter().map(|cell| cell.ch != " ").collect())
        .collect();
    let score = |top: i64, left: i64| -> f64 {
        let drawn: usize = filled[top as usize..(top + avail).min(rows_n) as usize]
            .iter()
            .map(|row| {
                row[left as usize..(left + width).min(cols_n) as usize]
                    .iter()
                    .filter(|f| **f)
                    .count()
            })
            .sum();
        drawn as f64
            - (fx - left - width / 2).abs() as f64 / width as f64
            - (fy - top - avail / 2).abs() as f64 / avail as f64
    };
    let mut best: Option<(f64, i64, i64)> = None;
    for &t in tops.iter().filter(|&&t| t <= fy && fy + labels < t + avail) {
        for &l in lefts
            .iter()
            .filter(|&&l| l <= fx && fx + you_room <= l + width)
        {
            let s = score(t, l);
            if best.is_none_or(|(b, _, _)| s > b) {
                best = Some((s, t, l));
            }
        }
    }
    let (top, left) = best.map_or((0, 0), |(_, t, l)| (t, l));
    // The map follows a drag, until its far edge reaches the view's.
    let top = (top - drag.1).max(0).min(top.max(content_h - avail));
    let left = (left - drag.0).max(0).min(left.max(content_w - width));
    let mut view: Vec<Vec<Cell>> = c.grid[top as usize..(top + avail).min(rows_n) as usize]
        .iter()
        .map(|row| row[left as usize..(left + width).min(cols_n) as usize].to_vec())
        .collect();
    // A label running past a side of the view ends in '…' there; cut on the left, the rest of
    // that label is dropped as well.
    for (vy, row) in view.iter_mut().enumerate() {
        let y = top + vy as i64;
        let label = |x: i64| {
            if c.ch(x, y) != " " {
                c.hit.get(&(x, y))
            } else {
                None
            }
        };
        let same = label(left);
        if left > 0 && same.is_some() && c.hit.get(&(left - 1, y)) == same {
            let end = (0..width)
                .find(|i| c.hit.get(&(left + i, y)) != same)
                .unwrap_or(width) as usize;
            let first = Cell {
                ch: "…".into(),
                fg: row[0].fg,
                bold: false,
                bg: row[0].bg,
            };
            for (i, cell) in row.iter_mut().take(end).enumerate() {
                *cell = if i == 0 {
                    first.clone()
                } else {
                    Cell::new(" ", None, false)
                };
            }
        }
        if left + width < cols_n
            && label(left + width - 1).is_some()
            && label(left + width - 1) == label(left + width)
        {
            if let Some(last) = row.last_mut() {
                last.ch = "…".into();
            }
        }
    }
    let n_rows = view.len() as i64;
    if top > 0 {
        *view[0].last_mut().unwrap() = Cell::new("▴", Some(MUTED), false);
    }
    if top + avail < content_h {
        *view.last_mut().unwrap().last_mut().unwrap() = Cell::new("▾", Some(MUTED), false);
    }
    if left > 0 {
        view[(fy - top).max(0).min(n_rows - 1) as usize][0] = Cell::new("◂", Some(MUTED), false);
    }
    // Sessions out of view, the focused one too once dragged away: an arrow in their colour on
    // the nearest edge, in the free cell closest to their direction.
    let mut away: Vec<(&&str, &Vec<&Session>)> = here.iter().collect();
    away.sort_by_key(|(b, _)| Some(**b) == you);
    for (b, on) in away {
        let (x, y) = xy.get(*b).copied().unwrap_or((0, 0));
        let (vx, vy) = (x - left, y - top);
        if 0 <= vx && vx < width && 0 <= vy && vy < n_rows {
            continue;
        }
        let arrow = if vx < 0 {
            "◂"
        } else if vx >= width {
            "▸"
        } else if vy < 0 {
            "▴"
        } else {
            "▾"
        };
        let (cx, cy) = (vx.max(0).min(width - 1), vy.max(0).min(n_rows - 1));
        let along = [0, 1, -1, 2, -2].map(|d| {
            if matches!(arrow, "◂" | "▸") {
                (cx, cy + d)
            } else {
                (cx + d, cy)
            }
        });
        let free = along.into_iter().find(|&(ax, ay)| {
            0 <= ax
                && ax < width
                && 0 <= ay
                && ay < n_rows
                && matches!(
                    view[ay as usize][ax as usize].ch.as_str(),
                    "" | " " | "◂" | "▸" | "▴" | "▾"
                )
        });
        if let Some((ax, ay)) = free {
            let color = if Some(*b) == you {
                YOU
            } else if *b == ROOT {
                ACCENT
            } else {
                scol(on[0])
            };
            view[ay as usize][ax as usize] = Cell::new(arrow, Some(color), true);
        }
    }
    for row in &mut view {
        mend(row);
    }
    let mut hit: Hit = c
        .hit
        .iter()
        .map(|(&(x, y), b)| ((x - left, y - top), b.clone()))
        .filter(|&((x, y), _)| 0 <= x && x < width && 0 <= y && y < n_rows)
        .collect();

    let mut rows: Vec<Vec<Seg>> = view
        .iter()
        .map(|row| {
            let mut segs: Vec<Seg> = Vec::new();
            for cell in row {
                match segs.last_mut() {
                    Some(s) if (s.fg, s.bg, s.bold) == (cell.fg, cell.bg, cell.bold) => {
                        s.text.push_str(&cell.ch)
                    }
                    _ => segs.push(seg(&cell.ch, cell.fg, cell.bg, cell.bold)),
                }
            }
            segs
        })
        .collect();

    // The last row names the focused session's position.
    let foot = match you {
        None => vec![seg("no agent in this pane", Some(MUTED), None, false)],
        Some(ROOT) => vec![
            seg("◈", Some(MAP_BG), Some(YOU), true),
            seg(" ", None, None, false),
            seg(layout::repo_name(&model.repo), Some(FG), None, true),
            seg(" · no PR", Some(MUTED), None, false),
        ],
        Some(b) => {
            let n = &nodes[b];
            let stage = &n.stage_label;
            let number =
                n.pr.as_ref()
                    .map_or_else(String::new, |p| format!("#{} ", p.number));
            let room = width
                - 2
                - number.len() as i64
                - if stage.is_empty() {
                    0
                } else {
                    stage.len() as i64 + 1
                };
            let name = if n.pr.is_some() {
                n.name.clone()
            } else {
                format!("{} (local)", n.name)
            };
            let stage_color = style::stage_color(n.title.stage.as_deref()).unwrap_or(MUTED);
            vec![
                seg("◉", Some(MAP_BG), Some(YOU), true),
                seg(" ", None, None, false),
                seg(&number, Some(FG), None, true),
                seg(&fit(&name, room), Some(DIM), None, false),
                seg(&format!(" {stage}"), Some(stage_color), None, false),
            ]
        }
    };
    if let Some(b) = you {
        hit.extend((0..width).map(|x| ((x, rows.len() as i64), b.to_string())));
    }
    rows.push(foot);
    // A narrow sidebar may not even fit the footer's fixed PR/stage text.
    for row in &mut rows {
        let mut used = 0;
        let mut clipped = false;
        for segment in row {
            segment.text = segment
                .text
                .chars()
                .take_while(|&ch| {
                    let next = used + cell_width(ch) as i64;
                    if clipped || next > width {
                        clipped = true;
                        return false;
                    }
                    used = next;
                    true
                })
                .collect();
        }
    }
    (rows, hit)
}

/// Segments as ANSI SGR text.
pub fn ansi(segments: &[Seg]) -> String {
    let sgr = |color: Option<Rgb>, layer: u8| {
        color.map_or_else(String::new, |[r, g, b]| {
            format!("\x1b[{layer};2;{r};{g};{b}m")
        })
    };
    segments
        .iter()
        .filter(|s| !s.text.is_empty())
        .map(|s| {
            format!(
                "{}{}{}{}\x1b[0m",
                sgr(s.fg, 38),
                sgr(s.bg, 48),
                if s.bold { "\x1b[1m" } else { "" },
                s.text
            )
        })
        .collect()
}

/// `prmap minimap [--model FILE]`: draws the panel for herdr. For tests, FILE replaces the
/// model; an optional HERDR_PANEL_FRAME_ID stores immutable model and hit cells.
pub fn main(model_file: Option<&str>) -> Result<(), String> {
    let env_num = |key: &str, default: i64| {
        std::env::var(key)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    let width = env_num("COLUMNS", 25).clamp(1, 512);
    let height = env_num("LINES", 12).clamp(1, 256);
    let zoom = env_num("HERDR_PANEL_ZOOM", DEFAULT_ZOOM).clamp(0, ZOOMS.len() as i64 - 1) as usize;
    let drag = (
        env_num("HERDR_PANEL_DRAG_X", 0).clamp(-32768, 32767),
        env_num("HERDR_PANEL_DRAG_Y", 0).clamp(-32768, 32767),
    );
    let pane = std::env::var("HERDR_ACTIVE_PANE_ID")
        .ok()
        .or_else(|| crate::config::get().demo.then(|| "w1:p1".into()));
    let model = match model_file {
        Some(path) => serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?,
        None => crate::load_model(false)?,
    };
    // A demo opened inside an existing Herdr session must use fictional pane identities.
    let pane = if crate::config::get().demo {
        pane.filter(|id| model.sessions.iter().any(|s| &s.pane == id))
            .or_else(|| {
                model
                    .sessions
                    .iter()
                    .find(|s| s.focused)
                    .or_else(|| model.sessions.first())
                    .map(|s| s.pane.clone())
            })
    } else {
        pane
    };
    let (rows, hit) = render(&model, pane.as_deref(), width, height, zoom, drag);
    let mut cells: Vec<(i64, i64, String)> = hit.into_iter().map(|((x, y), b)| (x, y, b)).collect();
    cells.sort();
    crate::frames::save(crate::frames::Frame {
        pane,
        width,
        height,
        zoom,
        drag,
        hit: cells,
        model,
    })?;
    println!(
        "{}",
        rows.iter().map(|r| ansi(r)).collect::<Vec<_>>().join("\n")
    );
    Ok(())
}
