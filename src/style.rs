//! Colours, line styles and text fitting of the minimap, in the full map's palette
//! (python/pr_map/style.py).

use std::collections::BTreeSet;

use indexmap::IndexMap;
use unicode_width::UnicodeWidthChar;

use crate::model::{Kind, Node};
use crate::sys;

pub type Rgb = [u8; 3];

pub const fn hex(s: &str) -> Rgb {
    const fn nibble(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => 0,
        }
    }
    let b = s.as_bytes();
    [
        nibble(b[1]) * 16 + nibble(b[2]),
        nibble(b[3]) * 16 + nibble(b[4]),
        nibble(b[5]) * 16 + nibble(b[6]),
    ]
}

pub const BAR: Rgb = hex("#16161e");
pub const FG: Rgb = hex("#c0caf5");
pub const DIM: Rgb = hex("#a9b1d6");
pub const MUTED: Rgb = hex("#565f89");
pub const ACCENT: Rgb = hex("#7dcfff");
pub const WARN: Rgb = hex("#ff9e64");
pub const YOU: Rgb = hex("#ff007c");
pub const UNTAGGED: Rgb = hex("#4c5578");
pub const SESSION_COLORS: &[&str] = &[
    "#9ece6a", "#7dcfff", "#bb9af7", "#e0af68", "#73daca", "#f7768e", "#7aa2f7", "#2ac3de",
    "#c0caf5",
];
/// Category lines, kept below the accent and warning in saturation.
pub const ROUTES: &[&str] = &[
    "#7aa2f7", "#9d7cd8", "#73daca", "#9ece6a", "#c49bd4", "#6fa8a0", "#d4a5a5", "#8fa2c8",
    "#b4c47a", "#a08cc8",
];

pub fn stage_color(stage: Option<&str>) -> Option<Rgb> {
    Some(hex(match stage? {
        "planning" => "#bb9af7",
        "developing" => "#e0af68",
        "evaluating" => "#7dcfff",
        "fix" => "#f7768e",
        "ready" => "#9ece6a",
        "abandoned" => return Some(MUTED),
        _ => return None,
    }))
}

/// `color` moved toward `base` by t: 0 keeps it, 1 gives `base`.
pub fn blend(color: Rgb, base: Rgb, t: f64) -> Rgb {
    let mix = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * t).round_ties_even() as u8;
    [
        mix(color[0], base[0]),
        mix(color[1], base[1]),
        mix(color[2], base[2]),
    ]
}

/// {key: colour} that stays with each key while others come and go.
///
/// A new key takes the colour least used by the current keys. The choice is kept in a registry
/// in the cache, which both maps read through the model, so an object keeps its colour across
/// refreshes.
pub fn stable_colors(kind: &str, keys: &[String], palette: &[&str]) -> IndexMap<String, String> {
    let path = sys::cache_dir().join("colors.json");
    let mut registry: IndexMap<String, IndexMap<String, usize>> = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let known = registry.entry(kind.to_string()).or_default();
    let new: BTreeSet<&String> = keys.iter().filter(|k| !known.contains_key(*k)).collect();
    for &key in &new {
        let used: Vec<usize> = keys.iter().filter_map(|k| known.get(k).copied()).collect();
        let pick = (0..palette.len())
            .min_by_key(|i| (used.iter().filter(|u| *u == i).count(), *i))
            .unwrap_or(0);
        known.insert(key.clone(), pick);
    }
    let colors = keys
        .iter()
        .map(|k| (k.clone(), palette[known[k] % palette.len()].to_string()))
        .collect();
    if !new.is_empty() {
        sys::write_cache(
            "colors.json",
            &serde_json::to_vec(&registry).unwrap_or_default(),
        );
    }
    colors
}

/// Terminal cells a character takes: two for wide (CJK) ones, none for combining marks.
pub fn cell_width(ch: char) -> usize {
    ch.width().unwrap_or(1)
}

pub fn cells(text: &str) -> i64 {
    text.chars().map(cell_width).sum::<usize>() as i64
}

/// `text` cut to `width` terminal cells, ending in '…' when cut.
pub fn fit(text: &str, width: i64) -> String {
    if cells(text) <= width {
        return text.to_string();
    }
    if width <= 0 {
        return String::new();
    }
    let (mut out, mut used) = (String::new(), 0);
    for ch in text.chars() {
        let w = cell_width(ch) as i64;
        if used + w > width - 1 {
            break;
        }
        out.push(ch);
        used += w;
    }
    out + "…"
}

/// (colour, horizontal glyph, vertical glyph) of the line leading to a station.
///
/// Dotted for a local branch, grey for a closed PR, dashed for a draft; the category colour
/// turns WARN when the PR does not contain its base's head.
pub fn edge_style(
    node: &Node,
    categories: &IndexMap<String, String>,
) -> (Rgb, &'static str, &'static str) {
    if node.kind == Kind::Local {
        return (MUTED, "·", "┊");
    }
    if node.is_closed() {
        return (MUTED, "─", "│");
    }
    let color = if node.synced == Some(false) {
        WARN
    } else {
        node.category()
            .and_then(|c| categories.get(c))
            .map_or(UNTAGGED, |c| hex(c))
    };
    if node.pr.as_ref().is_some_and(|p| p.is_draft) {
        (color, "╌", "╎")
    } else {
        (color, "─", "│")
    }
}
