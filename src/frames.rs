//! Immutable panel results: a click reads the exact model and hit cells accepted by Herdr.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::{model::Model, sys};

#[derive(Serialize, Deserialize)]
pub struct Frame {
    pub pane: Option<String>,
    pub width: i64,
    pub height: i64,
    pub zoom: usize,
    pub drag: (i64, i64),
    pub hit: Vec<(i64, i64, String)>,
    pub model: Model,
}

fn path(id: &str) -> Result<PathBuf, String> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        return Err("invalid panel frame id".into());
    }
    Ok(sys::cache_dir().join("frames").join(format!("{id}.json")))
}

pub fn save(frame: Frame) -> Result<(), String> {
    let Ok(id) = std::env::var("HERDR_PANEL_FRAME_ID") else {
        return Ok(());
    };
    if id.is_empty() {
        return Ok(());
    }
    let dest = path(&id)?;
    let dir = dest.parent().ok_or("invalid frame directory")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let data = serde_json::to_vec(&frame).map_err(|e| e.to_string())?;
    let tmp = dest.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, data)
        .and_then(|_| std::fs::rename(&tmp, &dest))
        .map_err(|e| e.to_string())?;
    // Keep 64 frames per producer. Separate Herdr instances never evict each other's display.
    let prefix = id
        .rsplit_once('-')
        .map(|(p, _)| format!("{p}-"))
        .unwrap_or(id);
    let mut old: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name().to_string_lossy().starts_with(&prefix)
                && e.path().extension().is_some_and(|s| s == "json")
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    old.sort_by_key(|e| e.0);
    let remove = old.len().saturating_sub(64);
    for (_, file) in old.into_iter().take(remove) {
        let _ = std::fs::remove_file(file);
    }
    Ok(())
}

pub fn load(id: &str) -> Result<Frame, String> {
    let data = std::fs::read(path(id)?)
        .map_err(|_| "displayed panel frame has expired; reopen the map".to_string())?;
    serde_json::from_slice(&data).map_err(|e| e.to_string())
}

pub fn clicked(frame: &Frame, column: i64, line: i64, width: i64, height: i64) -> Option<&str> {
    if (frame.width, frame.height) != (width, height)
        || column < 0
        || line < 0
        || column >= width
        || line >= height
    {
        return None;
    }
    frame
        .hit
        .iter()
        .filter(|(_, y, _)| *y == line)
        .min_by_key(|(x, _, _)| (x - column).abs())
        .filter(|(x, _, _)| (x - column).abs() <= 2)
        .map(|(_, _, branch)| branch.as_str())
}

#[cfg(test)]
mod tests {
    #[test]
    fn rejects_path_traversal() {
        assert!(super::path("../other").is_err());
    }
}
