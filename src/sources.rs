//! External sources: PRs from `gh` (cached), the shared git object store of the experiment
//! worktrees (memoized), and herdr's snapshot.

use std::sync::OnceLock;

use indexmap::IndexMap;
use serde_json::Value;

use crate::config;
use crate::sys;

/// Seconds before the PR cache is refetched.
const PR_TTL: f64 = 300.0;
const PR_FIELDS: &str = "number,title,headRefName,baseRefName,headRefOid,baseRefOid,isDraft,\
                         reviewDecision,url,updatedAt,state,closedAt";

/// Refetches all PRs into ~/.cache/pr-map/prs.json; false when gh fails.
fn fetch_prs(timeout: f64) -> bool {
    let cmd = [
        "gh",
        "pr",
        "list",
        "--repo",
        &config::get().repo,
        "--state",
        "all",
        "--limit",
        "400",
        "--json",
        PR_FIELDS,
    ];
    let Some(prs) =
        sys::stdout_of(&cmd, timeout).and_then(|s| serde_json::from_str::<Value>(&s).ok())
    else {
        return false;
    };
    let data = serde_json::json!({"fetched_at": sys::now(), "prs": prs});
    sys::write_cache("prs.json", data.to_string().as_bytes());
    true
}

/// (prs, fetched_at) from the cache.
fn load_prs() -> (Vec<Value>, f64) {
    let cached = std::fs::read(sys::cache_dir().join("prs.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    match cached {
        Some(d) => (
            d["prs"].as_array().cloned().unwrap_or_default(),
            d["fetched_at"].as_f64().unwrap_or(0.0),
        ),
        None => (Vec::new(), 0.0),
    }
}

/// (prs, fetched_at), refetched when the cache is older than PR_TTL or `force` is set.
///
/// `timeout` limits the refetch; callers run by herdr stay below herdr's own timeout.
pub fn fresh_prs(force: bool, timeout: f64) -> (Vec<Value>, f64) {
    let (prs, fetched_at) = load_prs();
    if (force || sys::now() - fetched_at > PR_TTL) && fetch_prs(timeout) {
        return load_prs();
    }
    (prs, fetched_at)
}

/// Memoized ancestry and count queries, keyed by object ids so they never go stale.
///
/// Only answers are kept: an unknown result (a missing object or a failed command) is asked
/// again next time, when a fetch may have brought the object.
pub struct Git {
    memo: IndexMap<String, Value>,
    dirty: bool,
}

impl Git {
    pub fn load() -> Git {
        let memo: IndexMap<String, Value> = std::fs::read(sys::cache_dir().join("git.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Git {
            memo: memo.into_iter().filter(|(_, v)| !v.is_null()).collect(),
            dirty: false,
        }
    }

    fn cached(&mut self, key: String, compute: impl FnOnce() -> Option<Value>) -> Option<&Value> {
        if !self.memo.contains_key(&key) {
            let value = compute()?;
            self.memo.insert(key.clone(), value);
            self.dirty = true;
        }
        self.memo.get(&key)
    }

    /// Whether commit `a` is an ancestor of `b`; None when git cannot tell.
    pub fn is_ancestor(&mut self, a: &str, b: &str) -> Option<bool> {
        self.cached(format!("anc:{a}:{b}"), || {
            match sys::run(
                &[
                    "git",
                    "-C",
                    &config::get().checkout,
                    "merge-base",
                    "--is-ancestor",
                    a,
                    b,
                ],
                10.0,
            )?
            .code
            {
                Some(0) => Some(Value::Bool(true)),
                Some(1) => Some(Value::Bool(false)),
                _ => None,
            }
        })
        .and_then(Value::as_bool)
    }

    /// Number of commits in a..b.
    pub fn count(&mut self, a: &str, b: &str) -> Option<u64> {
        self.cached(format!("cnt:{a}:{b}"), || {
            let out = sys::stdout_of(
                &[
                    "git",
                    "-C",
                    &config::get().checkout,
                    "rev-list",
                    "--count",
                    &format!("{a}..{b}"),
                ],
                10.0,
            )?;
            out.trim().parse::<u64>().ok().map(Value::from)
        })
        .and_then(Value::as_u64)
    }

    pub fn save(&mut self) {
        if self.dirty {
            sys::write_cache(
                "git.json",
                &serde_json::to_vec(&self.memo).unwrap_or_default(),
            );
            self.dirty = false;
        }
    }
}

/// [(path, branch)] for every worktree of `checkout` on a branch, longest path first.
pub fn worktrees(checkout: &str) -> Vec<(String, String)> {
    let out = sys::run(
        &["git", "-C", checkout, "worktree", "list", "--porcelain"],
        10.0,
    )
    .map(|o| o.stdout)
    .unwrap_or_default();
    let (mut trees, mut path) = (Vec::new(), None);
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(p.to_string());
        } else if let (Some(b), Some(p)) = (line.strip_prefix("branch refs/heads/"), &path) {
            trees.push((p.clone(), b.to_string()));
        }
    }
    trees.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
    trees
}

/// {branch: head commit} of the local branches.
pub fn local_heads() -> IndexMap<String, String> {
    let cmd = [
        "git",
        "-C",
        &config::get().checkout,
        "for-each-ref",
        "refs/heads",
        "--format=%(refname:short) %(objectname)",
    ];
    let out = sys::run(&cmd, 10.0).map(|o| o.stdout).unwrap_or_default();
    out.lines()
        .filter_map(|l| l.rsplit_once(' '))
        .map(|(b, oid)| (b.to_string(), oid.to_string()))
        .collect()
}

/// herdr's snapshot of the session it runs in: workspaces, tabs, panes and agents.
///
/// Taken once per process, so choosing the project and building the model see the same agents.
pub fn herdr_snapshot() -> Option<Value> {
    static SNAPSHOT: OnceLock<Option<Value>> = OnceLock::new();
    SNAPSHOT
        .get_or_init(|| {
            let out = sys::stdout_of(&[&sys::herdr_bin(), "api", "snapshot"], 5.0)?;
            let mut v: Value = serde_json::from_str(&out).ok()?;
            let snap = v.get_mut("result")?.get_mut("snapshot")?.take();
            snap.is_object().then_some(snap)
        })
        .clone()
}
