//! Process configuration and cache identity, loaded once at the command boundary.
//!
//! The config file names one project with top-level keys, or several as `[[project]]` tables
//! with the same keys. With several, each Herdr session maps the one most of its agents work in.
use std::cmp::Reverse;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

static CONFIG: OnceLock<Config> = OnceLock::new();

#[derive(Debug)]
pub struct Config {
    pub repo: String,
    pub workspace: String,
    pub checkout: String,
    pub aliases: Vec<String>,
    pub cache: PathBuf,
    pub demo: bool,
}

/// One project as the config file gives it; absent parts are inferred from the checkout.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileProject {
    repo: Option<String>,
    workspace: Option<PathBuf>,
    checkout: Option<PathBuf>,
    #[serde(default)]
    aliases: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileProjects {
    project: Vec<FileProject>,
}

/// A resolved project: (repo, workspace, checkout, aliases).
type Project = (String, String, String, Vec<String>);

pub fn get() -> &'static Config {
    CONFIG.get().expect("configuration initialized by main")
}

fn git(args: &[&str]) -> Result<String, String> {
    let mut cmd = vec!["git"];
    cmd.extend(args);
    crate::sys::stdout_of(&cmd, 5.0)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "cannot infer Git checkout; create a config file (see prmap --help)".into())
}

fn repo_from_remote(remote: &str) -> Option<String> {
    let rest = remote
        .strip_prefix("https://github.com/")
        .or_else(|| remote.strip_prefix("git@github.com:"))
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))?;
    let repo = rest.trim_end_matches('/').trim_end_matches(".git");
    valid_repo(repo).then(|| repo.to_string())
}

fn valid_repo(repo: &str) -> bool {
    repo.split('/').count() == 2
        && repo.split('/').all(|s| {
            !s.is_empty()
                && s != "."
                && s != ".."
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
        })
}

/// Fills a project's absent parts and makes its paths absolute against `base`.
fn resolve(file: FileProject, base: &Path) -> Result<Project, String> {
    let checkout = match file.checkout {
        Some(p) => absolute(&p, base)?,
        None => git(&["rev-parse", "--show-toplevel"])?,
    };
    let workspace = match file.workspace {
        Some(p) => absolute(&p, base)?,
        None => checkout.clone(),
    };
    let repo = match file.repo {
        Some(repo) => repo,
        None => repo_from_remote(&git(&["-C", &checkout, "remote", "get-url", "origin"])?)
            .ok_or("origin is not a GitHub remote; set repo = 'owner/repository' in config")?,
    };
    if !valid_repo(&repo) {
        return Err("repo must have the form owner/repository".into());
    }
    Ok((repo, workspace, checkout, file.aliases))
}

/// The project most of the snapshot's agents work in, below its workspace, an alias or one of
/// its worktrees; the first one listed on a tie, without matching agents or without a snapshot.
fn select(mut projects: Vec<Project>, snap: Option<&serde_json::Value>) -> Project {
    let cwds: Vec<String> = snap
        .and_then(|s| s["agents"].as_array())
        .into_iter()
        .flatten()
        .map(|a| crate::sessions::first_str(a, &["foreground_cwd", "cwd"]))
        .map(|c| format!("{}/", c.trim_end_matches('/')))
        .collect();
    let agents_in = |(_, workspace, checkout, aliases): &Project| {
        let roots: Vec<String> = std::iter::once(workspace)
            .chain(aliases)
            .cloned()
            .chain(
                crate::sources::worktrees(checkout)
                    .into_iter()
                    .map(|(p, _)| p),
            )
            .map(|r| format!("{}/", r.trim_end_matches('/')))
            .collect();
        cwds.iter()
            .filter(|c| roots.iter().any(|r| c.starts_with(r.as_str())))
            .count()
    };
    let best = projects
        .iter()
        .enumerate()
        .max_by_key(|(i, p)| (agents_in(p), Reverse(*i)))
        .map_or(0, |(i, _)| i);
    projects.swap_remove(best)
}

fn absolute(path: &Path, base: &Path) -> Result<String, String> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    path.canonicalize()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| format!("{}: {e}", path.display()))
}

impl Config {
    pub fn load(path: Option<&str>, demo: bool, fixture: bool) -> Result<Self, String> {
        let home = crate::sys::home();
        let xdg = |key: &str, suffix: &str| {
            std::env::var_os(key)
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(suffix))
        };
        let default = xdg("XDG_CONFIG_HOME", ".config").join("pr-map/config.toml");
        let selected = path
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("PRMAP_CONFIG").map(PathBuf::from));
        let file = selected.as_deref().unwrap_or(&default);
        let (repo, workspace, checkout, aliases) = if demo || fixture {
            (
                "example/harbor".into(),
                "/demo/harbor".into(),
                "/demo/harbor".into(),
                Vec::new(),
            )
        } else {
            let text = match std::fs::read_to_string(file) {
                Ok(text) => text,
                Err(e) if selected.is_none() && e.kind() == std::io::ErrorKind::NotFound => {
                    String::new()
                }
                Err(e) => return Err(format!("{}: {e}", file.display())),
            };
            let invalid = |e: toml::de::Error| format!("{}: {e}", file.display());
            let table: toml::Table = toml::from_str(&text).map_err(invalid)?;
            // [[project]] tables exclude top-level project keys: FileProjects denies them.
            let files = if table.contains_key("project") {
                toml::Value::Table(table)
                    .try_into::<FileProjects>()
                    .map_err(invalid)?
                    .project
            } else {
                vec![toml::Value::Table(table)
                    .try_into::<FileProject>()
                    .map_err(invalid)?]
            };
            if files.is_empty() {
                return Err(format!("{}: no [[project]] listed", file.display()));
            }
            let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
            let base = if text.is_empty() {
                cwd.as_path()
            } else {
                file.parent().unwrap_or(&cwd)
            };
            let mut projects = files
                .into_iter()
                .map(|f| resolve(f, base))
                .collect::<Result<Vec<_>, _>>()?;
            if projects.len() == 1 {
                projects.remove(0)
            } else {
                select(projects, crate::sources::herdr_snapshot().as_ref())
            }
        };
        // Stable across processes, separated by repository, checkout and Herdr session.
        let session = ["HERDR_SOCKET_PATH", "HERDR_SESSION_NAME"]
            .map(|k| std::env::var(k).unwrap_or_default())
            .join("\0");
        let identity = format!("{repo}\0{workspace}\0{checkout}\0{session}");
        let hash = identity.bytes().fold(0xcbf29ce484222325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
        });
        let cache = xdg("XDG_CACHE_HOME", ".cache")
            .join("pr-map")
            .join(format!("{hash:016x}"));
        Ok(Self {
            repo,
            workspace,
            checkout,
            aliases,
            cache,
            demo,
        })
    }
}

pub fn init(config: Config) -> Result<(), String> {
    CONFIG
        .set(config)
        .map_err(|_| "configuration already initialized".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn github_remote_formats_and_invalid_names() {
        for r in [
            "https://github.com/example/harbor.git",
            "git@github.com:example/harbor.git",
            "ssh://git@github.com/example/harbor",
        ] {
            assert_eq!(repo_from_remote(r).as_deref(), Some("example/harbor"));
        }
        assert!(!valid_repo("owner/repo/extra"));
        assert!(!valid_repo("../repo"));
        assert!(repo_from_remote("https://other.invalid/owner/repo").is_none());
    }

    fn project(repo: &str, workspace: &str, aliases: &[&str]) -> Project {
        let aliases = aliases.iter().map(|a| a.to_string()).collect();
        (
            repo.into(),
            workspace.into(),
            "/nonexistent".into(),
            aliases,
        )
    }

    fn pick(cwds: Option<&[&str]>) -> String {
        let projects = vec![
            project("o/a", "/w/a", &[]),
            project("o/b", "/w/b", &["/old/b"]),
        ];
        let agents: Option<Vec<serde_json::Value>> =
            cwds.map(|c| c.iter().map(|d| serde_json::json!({"cwd": d})).collect());
        let snap = agents.map(|a| serde_json::json!({ "agents": a }));
        select(projects, snap.as_ref()).0
    }

    #[test]
    fn a_session_maps_the_project_most_of_its_agents_work_in() {
        // '/w/bb' is not below '/w/b'; an alias counts for its project.
        assert_eq!(pick(Some(&["/w/a", "/w/a/x", "/w/bb", "/old/b/y"])), "o/a");
        assert_eq!(pick(Some(&["/w/a", "/w/b/x", "/old/b/y"])), "o/b");
        // A tie, no matching agent or no snapshot keeps the first project listed.
        assert_eq!(pick(Some(&["/w/a", "/w/b"])), "o/a");
        assert_eq!(pick(Some(&["/elsewhere"])), "o/a");
        assert_eq!(pick(None), "o/a");
    }

    #[test]
    fn project_tables_and_top_level_keys_do_not_mix() {
        let dir = std::env::temp_dir().join(format!("prmap-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("config.toml");
        let load = |text: &str| {
            std::fs::write(&file, text).unwrap();
            Config::load(file.to_str(), false, false)
        };
        let one = "[[project]]\nrepo = 'o/a'\ncheckout = '.'\n";
        let config = load(one).unwrap();
        assert_eq!(
            (config.repo.as_str(), config.workspace.as_str()),
            ("o/a", config.checkout.as_str())
        );
        assert!(load(&format!("repo = 'o/b'\n{one}")).is_err());
        assert!(load("project = []\n").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
