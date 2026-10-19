//! `prmap`: backend of the PR map.
//!
//! Subcommands:
//!   minimap [--model FILE]  sidebar panel of the herdr fork, run by herdr every few seconds
//!   report                  sets each herdr agent's `pr` sidebar token to its station and prints
//!                           the active pane's, run by a herdr tab bar command
//!   model [--force]         the map model as JSON, for the full map (--force refetches PRs)
//!   commits BRANCH          a station's own commits as JSON, for the full map's detail view
//!
//! The full map itself is the Python TUI in python/pr_map/app.py, started by
//! `pr-map --popup`; it reads the model and commits through this binary.

mod config;
mod frames;
mod layout;
mod minimap;
mod model;
mod sessions;
mod sources;
mod style;
mod sys;

use std::collections::HashMap;

use serde_json::Value;

/// Seconds a PR refetch may take when herdr runs us; herdr stops its commands after five.
const HERDR_FETCH_TIMEOUT: f64 = 4.0;
/// Seconds a PR refetch may take for the full map, which loads in the background.
const MAP_FETCH_TIMEOUT: f64 = 40.0;

/// Sets each agent's `pr` sidebar token to its station and clears stale ones.
///
/// herdr shows the token through `$pr` in [ui.sidebar.agents] rows. Only changed tokens are
/// reported; the snapshot holds the values herdr currently displays.
///
/// Returns:
///   The station of the active pane's session, or ''.
fn report(active_pane: Option<&str>) -> String {
    let snap = sources::herdr_snapshot();
    let model = model::cached(model::MODEL_TTL, HERDR_FETCH_TIMEOUT);
    let want: HashMap<&str, &str> = model
        .sessions
        .iter()
        .map(|s| (s.pane.as_str(), s.station.as_str()))
        .collect();
    let agents = snap
        .as_ref()
        .and_then(|s| s["agents"].as_array())
        .cloned()
        .unwrap_or_default();
    for a in &agents {
        let pane = sessions::first_str(a, &["pane_id"]);
        let have = a
            .get("tokens")
            .and_then(|t| t.get("pr"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let value = want.get(pane).copied().unwrap_or("");
        if !value.is_empty() && value != have {
            let token = format!("pr={value}");
            sys::run(
                &[
                    &sys::herdr_bin(),
                    "pane",
                    "report-metadata",
                    pane,
                    "--source",
                    "pr-map",
                    "--token",
                    &token,
                ],
                5.0,
            );
        } else if value.is_empty() && !have.is_empty() {
            sys::run(
                &[
                    &sys::herdr_bin(),
                    "pane",
                    "report-metadata",
                    pane,
                    "--source",
                    "pr-map",
                    "--clear-token",
                    "pr",
                ],
                5.0,
            );
        }
    }
    active_pane
        .and_then(|p| want.get(p))
        .unwrap_or(&"")
        .to_string()
}

pub(crate) fn load_model(force: bool) -> Result<model::Model, String> {
    if config::get().demo {
        let mut demo: model::Model = serde_json::from_str(include_str!("../examples/demo.json"))
            .map_err(|e| e.to_string())?;
        demo.fetched_at = sys::now();
        for (i, session) in demo.sessions.iter_mut().enumerate() {
            session.place.since = Some(demo.fetched_at - 120.0 * (i + 1) as f64);
            session.place.last = Some(demo.fetched_at - 45.0);
        }
        Ok(demo)
    } else if force {
        Ok(model::fresh(true, MAP_FETCH_TIMEOUT))
    } else {
        Ok(model::cached(model::MODEL_TTL, HERDR_FETCH_TIMEOUT))
    }
}

const HELP: &str = "prmap — PR stack and coding-agent map backend

Usage: prmap [--config FILE] [--demo] COMMAND

Commands:
  model [--force | --frame ID]  print the model as JSON
  minimap [--model FILE]        draw an ANSI sidebar panel
  commits BRANCH               print a station's commit list
  report                       update Herdr agent PR tokens
  clicked ID X Y WIDTH HEIGHT  look up a cell in the accepted panel frame

Configuration: $PRMAP_CONFIG or $XDG_CONFIG_HOME/pr-map/config.toml
TOML keys: repo = 'owner/repository', workspace = '/path', checkout = '/path'
Without a config file, checkout and GitHub origin are inferred from the current directory.
--demo uses bundled fictional data and never reads Git, GitHub or agent transcripts.
The full-screen interface is pr-map (Python/Textual).";

fn run() -> Result<(), String> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") || args.is_empty() {
        println!("{HELP}");
        return Ok(());
    }
    if args == ["--version"] {
        println!("prmap {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let mut config_path = None;
    if let Some(i) = args.iter().position(|a| a == "--config") {
        if i + 1 >= args.len() {
            return Err("--config requires a file".into());
        }
        config_path = Some(args.remove(i + 1));
        args.remove(i);
    }
    let demo = args.iter().any(|a| a == "--demo");
    args.retain(|a| a != "--demo");
    let fixture =
        args.first().is_some_and(|a| a == "minimap") && args.iter().any(|a| a == "--model");
    config::init(config::Config::load(config_path.as_deref(), demo, fixture)?)?;
    match args.first().map(String::as_str) {
        Some("minimap") if args.len() == 1 => minimap::main(None)?,
        Some("minimap") if args.len() == 3 && args[1] == "--model" => {
            minimap::main(Some(&args[2]))?
        }
        Some("model") => {
            let m = match args.as_slice() {
                [_] => load_model(false)?,
                [_, force] if force == "--force" => load_model(true)?,
                [_, flag, id] if flag == "--frame" => frames::load(id)?.model,
                _ => return Err("usage: prmap model [--force | --frame ID]".into()),
            };
            println!("{}", serde_json::to_string(&m).map_err(|e| e.to_string())?);
        }
        Some("commits") if args.len() == 2 => {
            let rows = if demo {
                let all: HashMap<String, Vec<model::CommitRow>> =
                    serde_json::from_str(include_str!("../examples/commits.json"))
                        .map_err(|e| e.to_string())?;
                all.get(&args[1]).cloned().unwrap_or_default()
            } else {
                let m = model::load_cached()
                    .map(Ok)
                    .unwrap_or_else(|| load_model(false))?;
                model::commits(&m, &args[1], 60)
            };
            println!(
                "{}",
                serde_json::to_string(&rows).map_err(|e| e.to_string())?
            );
        }
        Some("report") if args.len() == 1 => {
            if demo {
                let m = load_model(false)?;
                print!("{}", m.sessions.first().map_or("", |s| &s.station));
            } else {
                print!(
                    "{}",
                    report(std::env::var("HERDR_ACTIVE_PANE_ID").ok().as_deref())
                );
            }
        }
        Some("clicked") if args.len() == 6 => {
            let values: Vec<i64> = args[2..]
                .iter()
                .map(|v| {
                    v.parse::<i64>()
                        .map_err(|_| "click coordinates must be integers")
                })
                .collect::<Result<_, _>>()?;
            let frame = frames::load(&args[1])?;
            println!(
                "{}",
                serde_json::to_string(&frames::clicked(
                    &frame, values[0], values[1], values[2], values[3]
                ))
                .map_err(|e| e.to_string())?
            );
        }
        _ => return Err(format!("unknown command or arguments; {HELP}")),
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("prmap: {error}");
        std::process::exit(1);
    }
}
