//! Process configuration and cache identity, loaded once at the command boundary.
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

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    repo: Option<String>,
    workspace: Option<PathBuf>,
    checkout: Option<PathBuf>,
    #[serde(default)]
    aliases: Vec<String>,
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
            let cfg: FileConfig =
                toml::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?;
            let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
            let base = if text.is_empty() {
                cwd.as_path()
            } else {
                file.parent().unwrap_or(&cwd)
            };
            let checkout = match cfg.checkout {
                Some(p) => absolute(&p, base)?,
                None => git(&["rev-parse", "--show-toplevel"])?,
            };
            let workspace = match cfg.workspace {
                Some(p) => absolute(&p, base)?,
                None => checkout.clone(),
            };
            let repo = match cfg.repo {
                Some(repo) => repo,
                None => repo_from_remote(&git(&["-C", &checkout, "remote", "get-url", "origin"])?)
                    .ok_or(
                        "origin is not a GitHub remote; set repo = 'owner/repository' in config",
                    )?,
            };
            if !valid_repo(&repo) {
                return Err("repo must have the form owner/repository".into());
            }
            (repo, workspace, checkout, cfg.aliases)
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
}
