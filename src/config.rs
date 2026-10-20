//! Process configuration and cache identity, loaded once at the command boundary.
//!
//! The config file names one project with top-level keys, or several as `[[project]]` tables
//! with the same keys. With several, each Herdr session maps the one most of its agents work
//! in, or the one $PRMAP_PROJECT names; the map lists the others its agents work for
//! (sessions::owner).
use std::cmp::Reverse;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

static CONFIG: OnceLock<Config> = OnceLock::new();

#[derive(Debug)]
pub struct Config {
    /// Every configured project, in config order.
    pub projects: Vec<Project>,
    /// Index of the project this process maps.
    pub current: usize,
    pub demo: bool,
}

impl Config {
    /// The project this process maps.
    pub fn project(&self) -> &Project {
        &self.projects[self.current]
    }
}

/// A configured project.
#[derive(Clone, Debug)]
pub struct Project {
    /// 'owner/repository' on GitHub.
    pub repo: String,
    /// The directory its agents work in.
    pub workspace: String,
    /// The checkout whose objects and registered worktrees hold its branches.
    pub checkout: String,
    /// Compatibility links to the workspace in older transcript paths.
    pub aliases: Vec<String>,
    /// Its cache directory, separated by repository, checkout and Herdr session.
    pub cache: PathBuf,
}

impl Project {
    /// The directories its agents work in, each with whether it is a worktree: its workspace,
    /// aliases and `trees` (sources::worktrees of its checkout).
    pub fn roots(&self, trees: &[(String, String)]) -> Vec<(String, bool)> {
        std::iter::once(&self.workspace)
            .chain(&self.aliases)
            .map(|r| (r.clone(), false))
            .chain(trees.iter().map(|(p, _)| (p.clone(), true)))
            .collect()
    }
}

/// How deep `roots` reach around `dir`: (length of the deepest root containing it, whether a
/// worktree is among those that deep), or None when no root contains it.
pub fn claim(roots: &[(String, bool)], dir: &str) -> Option<(usize, bool)> {
    let dir = format!("{}/", dir.trim_end_matches('/'));
    roots
        .iter()
        .map(|(root, tree)| (format!("{}/", root.trim_end_matches('/')), *tree))
        .filter(|(root, _)| dir.starts_with(root.as_str()))
        .map(|(root, tree)| (root.len(), tree))
        .max()
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
    Ok(Project {
        cache: cache_of(&repo, &workspace, &checkout),
        repo,
        workspace,
        checkout,
        aliases: file.aliases,
    })
}

/// A project's cache directory, stable across processes.
fn cache_of(repo: &str, workspace: &str, checkout: &str) -> PathBuf {
    let session = ["HERDR_SOCKET_PATH", "HERDR_SESSION_NAME"]
        .map(|k| std::env::var(k).unwrap_or_default())
        .join("\0");
    let identity = format!("{repo}\0{workspace}\0{checkout}\0{session}");
    let hash = identity.bytes().fold(0xcbf29ce484222325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    xdg("XDG_CACHE_HOME", ".cache")
        .join("pr-map")
        .join(format!("{hash:016x}"))
}

fn xdg(key: &str, suffix: &str) -> PathBuf {
    std::env::var_os(key)
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::sys::home().join(suffix))
}

/// The project most of the snapshot's agents work in; the first listed on a tie.
///
/// An agent counts for the projects with the deepest root around its directory (`claim`):
/// one in a worktree inside another project's workspace counts for the worktree's project
/// only, one in a directory that is both one project's workspace and another's checkout for
/// both.
fn select(projects: &[Project], snap: Option<&serde_json::Value>) -> usize {
    let roots: Vec<Vec<(String, bool)>> = projects
        .iter()
        .map(|p| p.roots(&crate::sources::worktrees(&p.checkout)))
        .collect();
    let mut counts = vec![0; projects.len()];
    for a in snap
        .and_then(|s| s["agents"].as_array())
        .into_iter()
        .flatten()
    {
        let dir = crate::sessions::first_str(a, &["foreground_cwd", "cwd"]);
        let depths: Vec<Option<usize>> = roots
            .iter()
            .map(|r| claim(r, dir).map(|(depth, _)| depth))
            .collect();
        let deepest = depths.iter().flatten().max().copied();
        for (count, depth) in counts.iter_mut().zip(&depths) {
            if depth.is_some() && *depth == deepest {
                *count += 1;
            }
        }
    }
    counts
        .iter()
        .enumerate()
        .max_by_key(|(i, c)| (**c, Reverse(*i)))
        .map_or(0, |(i, _)| i)
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
        let default = xdg("XDG_CONFIG_HOME", ".config").join("pr-map/config.toml");
        let selected = path
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("PRMAP_CONFIG").map(PathBuf::from));
        let file = selected.as_deref().unwrap_or(&default);
        if demo || fixture {
            let (repo, root) = ("example/harbor", "/demo/harbor");
            return Ok(Self {
                projects: vec![Project {
                    repo: repo.into(),
                    workspace: root.into(),
                    checkout: root.into(),
                    aliases: Vec::new(),
                    cache: cache_of(repo, root, root),
                }],
                current: 0,
                demo,
            });
        }
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
        let projects = files
            .into_iter()
            .map(|f| resolve(f, base))
            .collect::<Result<Vec<_>, _>>()?;
        let current = match std::env::var("PRMAP_PROJECT")
            .ok()
            .filter(|r| !r.is_empty())
        {
            Some(repo) => projects
                .iter()
                .position(|p| p.repo == repo)
                .ok_or_else(|| format!("PRMAP_PROJECT: {repo} is not a configured project"))?,
            None if projects.len() == 1 => 0,
            None => select(&projects, crate::sources::herdr_snapshot().as_ref()),
        };
        Ok(Self {
            projects,
            current,
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
        Project {
            repo: repo.into(),
            workspace: workspace.into(),
            checkout: "/nonexistent".into(),
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
            cache: PathBuf::new(),
        }
    }

    fn pick(projects: &[Project], cwds: Option<&[&str]>) -> String {
        let agents: Option<Vec<serde_json::Value>> =
            cwds.map(|c| c.iter().map(|d| serde_json::json!({"cwd": d})).collect());
        let snap = agents.map(|a| serde_json::json!({ "agents": a }));
        projects[select(projects, snap.as_ref())].repo.clone()
    }

    #[test]
    fn a_session_maps_the_project_most_of_its_agents_work_in() {
        let two = [
            project("o/a", "/w/a", &[]),
            project("o/b", "/w/b", &["/old/b"]),
        ];
        // '/w/bb' is not below '/w/b'; an alias counts for its project.
        assert_eq!(
            pick(&two, Some(&["/w/a", "/w/a/x", "/w/bb", "/old/b/y"])),
            "o/a"
        );
        assert_eq!(pick(&two, Some(&["/w/a", "/w/b/x", "/old/b/y"])), "o/b");
        // A tie, no matching agent or no snapshot keeps the first project listed.
        assert_eq!(pick(&two, Some(&["/w/a", "/w/b"])), "o/a");
        assert_eq!(pick(&two, Some(&["/elsewhere"])), "o/a");
        assert_eq!(pick(&two, None), "o/a");
        // An agent counts for the projects with the deepest root around it: below '/w/a/c' for
        // c alone, in the rest of '/w/a' for both a and d.
        let nested = [
            project("o/a", "/w/a", &[]),
            project("o/c", "/w/a/c", &[]),
            project("o/d", "/w/a", &[]),
        ];
        assert_eq!(
            pick(&nested, Some(&["/w/a/c/x", "/w/a/c", "/w/a/y"])),
            "o/c"
        );
        assert_eq!(pick(&nested, Some(&["/w/a/c/x", "/w/a/y", "/w/a"])), "o/a");
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
        let project = config.project();
        assert_eq!(
            (project.repo.as_str(), project.workspace.as_str()),
            ("o/a", project.checkout.as_str())
        );
        assert!(load(&format!("repo = 'o/b'\n{one}")).is_err());
        assert!(load("project = []\n").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
