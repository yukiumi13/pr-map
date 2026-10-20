//! `prmap`: backend of the PR map.
//!
//! Subcommands:
//!   minimap [--model FILE]  sidebar panel of the herdr fork, run by herdr every few seconds
//!   report                  sets each herdr agent's `pr` sidebar token to its station and its
//!                           pane title to the session name; prints the active pane's station;
//!                           run by a herdr tab bar command
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

/// Sets each agent's `pr` sidebar token to its station and its pane title to the session name,
/// and clears stale ones.
///
/// herdr shows the token through `$pr` and the title through `pane` in [ui.sidebar.agents]
/// rows. Only changed values are reported; the snapshot holds the values herdr currently
/// displays. A pane label set by hand in herdr keeps precedence over the session name.
///
/// Returns:
///   The station of the active pane's session, or ''.
fn report(active_pane: Option<&str>) -> String {
    let snap = sources::herdr_snapshot();
    let model = model::cached(model::MODEL_TTL, HERDR_FETCH_TIMEOUT);
    let by_pane: HashMap<&str, &model::Session> = model
        .sessions
        .iter()
        .map(|s| (s.pane.as_str(), s))
        .collect();
    let agents = snap
        .as_ref()
        .and_then(|s| s["agents"].as_array())
        .cloned()
        .unwrap_or_default();
    for a in &agents {
        let pane = sessions::first_str(a, &["pane_id"]);
        let session = by_pane.get(pane);
        let station = session.map_or("", |s| s.station.as_str());
        let shown_station = a
            .get("tokens")
            .and_then(|t| t.get("pr"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let title = session
            .and_then(|s| s.name.as_deref())
            .filter(|_| sessions::first_str(a, &["label"]).is_empty())
            .unwrap_or("");
        let mut changes = Vec::new();
        if station != shown_station {
            changes.extend(match station {
                "" => ["--clear-token".to_string(), "pr".into()],
                _ => ["--token".into(), format!("pr={station}")],
            });
        }
        if title != sessions::first_str(a, &["title"]) {
            changes.extend(match title {
                "" => vec!["--clear-title".to_string()],
                _ => vec!["--title".into(), title.into()],
            });
        }
        if !changes.is_empty() {
            let herdr = sys::herdr_bin();
            let mut cmd = vec![
                herdr.as_str(),
                "pane",
                "report-metadata",
                pane,
                "--source",
                "pr-map",
            ];
            cmd.extend(changes.iter().map(String::as_str));
            sys::run(&cmd, 5.0);
        }
    }
    active_pane
        .and_then(|p| by_pane.get(p))
        .map_or("", |s| s.station.as_str())
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
  report                       update Herdr agent PR tokens and session-name titles
  clicked ID X Y WIDTH HEIGHT  look up a cell in the accepted panel frame

Configuration: $PRMAP_CONFIG or $XDG_CONFIG_HOME/pr-map/config.toml
TOML keys: repo = 'owner/repository', workspace = '/path', checkout = '/path', or several
[[project]] tables with these keys; each Herdr session maps the one most of its agents work in.
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
