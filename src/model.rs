//! Data model of the PR map: open PR stacks and the live herdr sessions on them.
//!
//! Sources:
//!   PRs       `gh pr list` (sources.rs). The map shows open PRs and PRs closed without merging
//!             in the last CLOSED_DAYS.
//!   Stack     each PR's base branch; edge state and commit counts come from the shared git
//!             object store of the experiment worktrees.
//!   Titles    `[<Domain>: <Category>] [<Stage>] [k/n] summary` (tagged PR titles) gives each
//!             PR its category, which names its station and colours the line to it, its stage
//!             and summary. Untagged PRs and local branches are named after their branch.
//!   Sessions  herdr's snapshot, each agent placed by its transcript (sessions.rs).
//!   Commits   a commit is confirmed for a session when the output of that session's git commit,
//!             rebase or cherry-pick call shows the commit's sha; otherwise it is inferred when
//!             such a call on the same branch precedes the committer time by at most COMMIT_LAG.
//!
//! The model is cached in the configured cache namespace, so the minimap, the sidebar tokens and the
//! full map share one build every MODEL_TTL seconds.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::sessions::{self, Locator, Place};
use crate::sources::{self, Git};
use crate::style;
use crate::{config, sys};

/// Seconds a cached model is reused.
pub const MODEL_TTL: f64 = 4.0;
const CLOSED_DAYS: f64 = 7.0;
/// Seconds a commit may trail the tool call that made it.
const COMMIT_LAG: f64 = 900.0;
const STAGES: [&str; 6] = [
    "planning",
    "developing",
    "evaluating",
    "fix",
    "ready",
    "abandoned",
];

pub fn stage_abbr(stage: &str) -> Option<&'static str> {
    Some(match stage {
        "planning" => "Plan",
        "developing" => "Dev",
        "evaluating" => "Eval",
        "fix" => "Fix",
        "ready" => "Ready",
        "abandoned" => "Abnd",
        _ => return None,
    })
}

/// A PR as `gh pr list --json` gives it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pr {
    pub number: u64,
    pub title: String,
    pub head_ref_name: String,
    pub base_ref_name: String,
    pub head_ref_oid: String,
    pub base_ref_oid: String,
    pub is_draft: bool,
    #[serde(default)]
    pub review_decision: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub updated_at: String,
    pub state: String,
    #[serde(default)]
    pub closed_at: Option<String>,
}

/// A PR title's optional tags; absent parts are None.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Title {
    pub tags: Vec<String>,
    pub domain: Option<String>,
    pub category: Option<String>,
    /// Lower case, one of STAGES.
    pub stage: Option<String>,
    /// 'k/n'.
    pub position: Option<String>,
    pub summary: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Pr,
    Base,
    Local,
}

/// A station: a PR head, a base branch, or a local branch a session works on.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub branch: String,
    pub pr: Option<Pr>,
    pub parent: Option<String>,
    pub kind: Kind,
    pub title: Title,
    /// The category, else the branch tail; a base branch's full name.
    pub name: String,
    /// Whether an open PR contains its base's head; None when unknown or not an open PR.
    pub synced: Option<bool>,
    /// An open PR's own commits.
    pub commits: Option<u64>,
    /// A local branch's head, and the head of the PR it hangs under.
    pub head: Option<String>,
    pub base_oid: Option<String>,
    /// 'closed', the stage's abbreviation, or '' (no PR or no stage).
    pub stage_label: String,
    pub hints: Vec<String>,
}

impl Node {
    fn new(branch: &str, kind: Kind, pr: Option<&Pr>, parent: Option<&str>) -> Node {
        Node {
            branch: branch.to_string(),
            pr: pr.cloned(),
            parent: parent.map(str::to_string),
            kind,
            title: title_parts(pr.map_or("", |p| &p.title)),
            name: String::new(),
            synced: None,
            commits: None,
            head: None,
            base_oid: None,
            stage_label: String::new(),
            hints: Vec::new(),
        }
    }

    pub fn is_closed(&self) -> bool {
        self.pr.as_ref().is_some_and(|p| p.state == "CLOSED")
    }

    /// The category the line to this station is coloured by.
    pub fn category(&self) -> Option<&str> {
        [&self.title.category, &self.title.domain]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .find(|c| !c.is_empty())
    }
}

/// A herdr agent working in the workspace.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub pane: String,
    /// Its tab's label, else '<space>:<tab>'; ' (codex)' marks a non-Claude agent.
    pub label: String,
    pub agent: Option<String>,
    pub status: String,
    pub title: String,
    pub focused: bool,
    #[serde(flatten)]
    pub place: Place,
    /// Two letters, unique among the sessions, marking its commits.
    pub badge: String,
    /// station_label of its current branch, as the sidebar token shows it.
    pub station: String,
}

/// Colours that stay with a session or category across refreshes (style::stable_colors).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Colors {
    pub session: IndexMap<String, String>,
    pub category: IndexMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Model {
    pub repo: String,
    /// Checkout where the full map reads commit graphs.
    pub git: String,
    pub fetched_at: f64,
    pub nodes: IndexMap<String, Node>,
    pub sessions: Vec<Session>,
    /// {directory: [status]} of agents outside the workspace.
    pub elsewhere: IndexMap<String, Vec<String>>,
    /// Whether herdr's snapshot was available.
    pub herdr: bool,
    pub colors: Colors,
}

static TAGS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^((?:\s*\[[^\]]+\])+)\s*(.*)$").unwrap());
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([^\]]+)\]").unwrap());
static POSITION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+/\d+$").unwrap());

/// Splits a PR title into its optional category and stage tags.
pub fn title_parts(title: &str) -> Title {
    let mut out = Title {
        summary: title.to_string(),
        ..Title::default()
    };
    let Some(m) = TAGS.captures(title) else {
        return out;
    };
    out.tags = TAG.captures_iter(&m[1]).map(|c| c[1].to_string()).collect();
    out.summary = m[2].to_string();
    for tag in &out.tags {
        let lower = tag.to_lowercase();
        if STAGES.contains(&lower.as_str()) {
            out.stage = Some(lower);
        } else if POSITION.is_match(tag) {
            out.position = Some(tag.clone());
        } else if out.domain.is_none() {
            let (domain, category) = tag.split_once(':').unwrap_or((tag, ""));
            out.domain = Some(domain.trim().to_string());
            out.category = Some(category.trim().to_string()).filter(|c| !c.is_empty());
        }
    }
    out
}

/// 'feat/v2-trace-prefix-qa' -> 'trace-prefix-qa'.
fn station_name(branch: &str) -> &str {
    let tail = branch.rsplit('/').next().unwrap_or(branch);
    tail.strip_prefix("v2-").unwrap_or(tail)
}

/// '#103 Index · Eval', or 'cache-warmup · local' for a branch without a PR.
pub fn station_label(nodes: &IndexMap<String, Node>, branch: Option<&str>) -> String {
    let Some(n) = branch.and_then(|b| nodes.get(b)) else {
        return String::new();
    };
    let Some(p) = &n.pr else {
        return format!(
            "{} · {}",
            n.name,
            serde_json::to_value(n.kind).unwrap().as_str().unwrap()
        );
    };
    let label = format!("#{} {}", p.number, n.name);
    if n.stage_label.is_empty() {
        label
    } else {
        format!("{label} · {}", n.stage_label)
    }
}

/// The warnings shown on a node, from its tags and stack position.
fn hints(node: &Node, nodes: &IndexMap<String, Node>) -> Vec<String> {
    let mut out = Vec::new();
    if node.kind != Kind::Pr || node.is_closed() {
        return out;
    }
    let (t, parent) = (&node.title, node.parent.as_ref().and_then(|p| nodes.get(p)));
    if t.tags.is_empty() {
        out.push("title has no [Domain] [Stage] tags".into());
    }
    let ready = t.stage.as_deref() == Some("ready");
    if node.synced == Some(false) {
        out.push(format!(
            "needs a rebase onto {}{}",
            node.parent.as_deref().unwrap_or(""),
            if ready {
                " although it is marked Ready"
            } else {
                ""
            }
        ));
    }
    if let Some(parent) =
        parent.filter(|p| ready && p.kind == Kind::Pr && p.title.stage.as_deref() != Some("ready"))
    {
        let stage = parent.title.stage.as_deref().unwrap_or("untagged");
        let mut chars = stage.chars();
        let stage: String = chars
            .next()
            .into_iter()
            .flat_map(char::to_uppercase)
            .chain(chars.flat_map(char::to_lowercase))
            .collect();
        out.push(format!(
            "marked Ready while its parent #{} is {stage}",
            parent.pr.as_ref().map_or(0, |p| p.number)
        ));
    }
    out
}

/// Builds the map model from PRs, herdr's snapshot and the git store.
pub fn build(prs: Vec<Value>, fetched_at: f64, snap: Option<Value>, git: &mut Git) -> Model {
    let trees = sources::worktrees();
    let heads = sources::local_heads();
    let cutoff = sys::now() - CLOSED_DAYS * 86400.0;
    let prs: Vec<Pr> = prs
        .into_iter()
        .filter_map(|v| serde_json::from_value::<Pr>(v).ok())
        .filter(|p| {
            p.state == "OPEN"
                || p.state == "CLOSED"
                    && p.closed_at
                        .as_deref()
                        .and_then(sys::epoch)
                        .is_some_and(|t| t > cutoff)
        })
        .collect();
    let pr_by_number: HashMap<u64, Pr> = prs.iter().map(|p| (p.number, p.clone())).collect();
    let locator = Locator::new(&trees, pr_by_number.keys().copied().collect());

    // Sessions: agents in the workspace or on a branch of the map; the rest are 'elsewhere'.
    let list = |key: &str| {
        snap.as_ref()
            .and_then(|s| s.get(key))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let labels = |key: &str, id: &str| -> HashMap<String, String> {
        list(key)
            .iter()
            .map(|v| {
                (
                    sessions::first_str(v, &[id]).to_string(),
                    sessions::first_str(v, &["label"]).to_string(),
                )
            })
            .collect()
    };
    let (tabs, spaces) = (
        labels("tabs", "tab_id"),
        labels("workspaces", "workspace_id"),
    );
    let focused = snap
        .as_ref()
        .map(|s| sessions::first_str(s, &["focused_pane_id"]))
        .unwrap_or("");
    let (mut sessions, mut elsewhere) = (
        Vec::<Session>::new(),
        IndexMap::<String, Vec<String>>::new(),
    );
    for a in list("agents") {
        let cwd = locator.normalize(sessions::first_str(&a, &["foreground_cwd", "cwd"]));
        let place = sessions::locate(&a, &locator, &pr_by_number);
        let status = a
            .get("agent_status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        if !Path::new(&cwd).starts_with(&config::get().workspace) && place.current.is_none() {
            let trimmed = cwd.trim_end_matches('/');
            let name = trimmed
                .rsplit('/')
                .next()
                .filter(|n| !n.is_empty())
                .unwrap_or(&cwd);
            elsewhere.entry(name.to_string()).or_default().push(status);
            continue;
        }
        let pane = sessions::first_str(&a, &["pane_id"]).to_string();
        let mut label = tabs
            .get(sessions::first_str(&a, &["tab_id"]))
            .cloned()
            .unwrap_or_default();
        if label.is_empty() || label.chars().all(|c| c.is_ascii_digit()) {
            let space = spaces
                .get(sessions::first_str(&a, &["workspace_id"]))
                .map_or("", String::as_str);
            label = format!("{space}:{label}").trim_matches(':').to_string();
        }
        let agent = a.get("agent").and_then(Value::as_str).map(str::to_string);
        if agent.as_deref() != Some("claude") {
            label += &format!(" ({})", agent.as_deref().unwrap_or("None"));
        }
        sessions.push(Session {
            label: if label.is_empty() {
                pane.clone()
            } else {
                label
            },
            agent,
            status,
            title: sessions::first_str(&a, &["terminal_title_stripped"]).to_string(),
            focused: pane == focused,
            pane,
            place,
            badge: String::new(),
            station: String::new(),
        });
    }
    let mut used = BTreeSet::new();
    for s in &mut sessions {
        let base = sessions::initials(&s.label);
        let (mut badge, mut k) = (base.clone(), 2);
        while used.contains(&badge) {
            badge = format!("{}{k}", base.chars().next().unwrap_or('?'));
            k += 1;
        }
        used.insert(badge.clone());
        s.badge = badge;
    }

    // Stations: PR heads, their bases, and local branches sessions work on.
    let mut nodes: IndexMap<String, Node> = IndexMap::new();
    for p in &prs {
        nodes.insert(
            p.head_ref_name.clone(),
            Node::new(&p.head_ref_name, Kind::Pr, Some(p), Some(&p.base_ref_name)),
        );
    }
    for b in prs.iter().map(|p| p.base_ref_name.as_str()).chain(["main"]) {
        nodes
            .entry(b.to_string())
            .or_insert_with(|| Node::new(b, Kind::Base, None, None));
    }
    // A branch a session works on without a PR hangs under the deepest PR head it contains.
    for s in &sessions {
        let p = &s.place;
        for b in p.current.iter().chain(&p.recent).chain(&p.earlier) {
            let Some(head) = heads.get(b).filter(|_| !nodes.contains_key(b)) else {
                continue;
            };
            let below: Vec<&Pr> = prs
                .iter()
                .filter(|p| git.is_ancestor(&p.head_ref_oid, head) == Some(true))
                .collect();
            let mut deepest: Option<(&Pr, usize)> = None;
            for &p in &below {
                let depth = below
                    .iter()
                    .filter(|q| git.is_ancestor(&q.head_ref_oid, &p.head_ref_oid) == Some(true))
                    .count();
                if deepest.is_none_or(|(_, d)| depth > d) {
                    deepest = Some((p, depth));
                }
            }
            let parent = match deepest {
                Some((p, _)) => Some(p.head_ref_name.as_str()),
                None => heads
                    .get("main")
                    .filter(|m| git.is_ancestor(m, head) == Some(true))
                    .map(|_| "main"),
            };
            let mut node = Node::new(b, Kind::Local, None, parent);
            node.head = Some(head.clone());
            node.base_oid = deepest.map(|(p, _)| p.head_ref_oid.clone());
            nodes.insert(b.clone(), node);
        }
    }
    for n in nodes.values_mut() {
        n.name = if n.kind == Kind::Base {
            n.branch.clone()
        } else {
            n.title
                .category
                .clone()
                .unwrap_or_else(|| station_name(&n.branch).to_string())
        };
        if let Some(p) = n.pr.as_ref().filter(|_| !n.is_closed()) {
            n.synced = git.is_ancestor(&p.base_ref_oid, &p.head_ref_oid);
            n.commits = git.count(&p.base_ref_oid, &p.head_ref_oid);
        }
        n.stage_label = match &n.pr {
            None => String::new(),
            Some(_) if n.is_closed() => "closed".into(),
            Some(_) => n
                .title
                .stage
                .as_deref()
                .and_then(stage_abbr)
                .unwrap_or("")
                .into(),
        };
    }
    let all_hints: Vec<Vec<String>> = nodes.values().map(|n| hints(n, &nodes)).collect();
    for (n, h) in nodes.values_mut().zip(all_hints) {
        n.hints = h;
    }
    git.save();

    for s in &mut sessions {
        s.station = station_label(&nodes, s.place.current.as_deref());
    }
    let panes: Vec<String> = sessions.iter().map(|s| s.pane.clone()).collect();
    let categories: Vec<String> = nodes
        .values()
        .filter_map(Node::category)
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let colors = Colors {
        session: style::stable_colors("session", &panes, style::SESSION_COLORS),
        category: style::stable_colors("category", &categories, style::ROUTES),
    };
    Model {
        repo: config::get().repo.clone(),
        git: config::get().checkout.clone(),
        fetched_at,
        nodes,
        sessions,
        elsewhere,
        herdr: snap.is_some(),
        colors,
    }
}

/// Loads PRs and herdr's snapshot, builds the model and caches it.
///
/// Args:
///   force: refetch PRs even when their cache is fresh.
///   fetch_timeout: limit for a PR refetch; callers run by herdr stay below herdr's timeout.
pub fn fresh(force: bool, fetch_timeout: f64) -> Model {
    let (prs, fetched_at) = sources::fresh_prs(force, fetch_timeout);
    let model = build(prs, fetched_at, sources::herdr_snapshot(), &mut Git::load());
    sys::write_cache(
        "model.json",
        &serde_json::to_vec(&model).unwrap_or_default(),
    );
    model
}

/// The cached model, or None when there is none.
pub fn load_cached() -> Option<Model> {
    serde_json::from_slice(&std::fs::read(sys::cache_dir().join("model.json")).ok()?).ok()
}

/// The cached model while it is younger than `ttl` seconds, else a fresh one.
pub fn cached(ttl: f64, fetch_timeout: f64) -> Model {
    let young =
        sys::mtime(&sys::cache_dir().join("model.json")).is_some_and(|t| sys::now() - t < ttl);
    young
        .then(load_cached)
        .flatten()
        .unwrap_or_else(|| fresh(false, fetch_timeout))
}

/// A row of a station's commit list: graph, sha, committer time, subject, the badge of the
/// session that made it, and whether that is confirmed. Pure graph lines have no sha.
pub type CommitRow = (
    String,
    Option<String>,
    Option<i64>,
    String,
    Option<String>,
    bool,
);

/// A station's own commits, newest first, at most `limit`.
pub fn commits(model: &Model, branch: &str, limit: usize) -> Vec<CommitRow> {
    let Some(node) = model.nodes.get(branch) else {
        return Vec::new();
    };
    let (base, head) = match (&node.pr, node.kind, &node.base_oid, &node.head) {
        (Some(p), _, _, _) => (p.base_ref_oid.clone(), p.head_ref_oid.clone()),
        (None, Kind::Local, Some(base), Some(head)) => (base.clone(), head.clone()),
        _ => return Vec::new(),
    };
    let cmd = [
        "git",
        "-C",
        &model.git,
        "log",
        "--graph",
        &format!("--max-count={limit}"),
        "--format=%x00%h%x1f%ct%x1f%s",
        &format!("{base}..{head}"),
    ];
    let log = sys::stdout_of(&cmd, 10.0).unwrap_or_default();
    log.lines()
        .map(|line| {
            let (graph, data) = line.split_once('\0').unwrap_or((line, ""));
            let mut fields = data.splitn(3, '\x1f');
            let (Some(sha), Some(ct), Some(subject)) =
                (fields.next(), fields.next(), fields.next())
            else {
                return (graph.to_string(), None, None, String::new(), None, false);
            };
            let ct: i64 = ct.parse().unwrap_or(0);
            let (mut confirmed, mut inferred): (Option<&str>, Option<(f64, &str)>) = (None, None);
            for s in &model.sessions {
                for (ts, branches, output) in &s.place.commit_calls {
                    if output.contains(&sha[..sha.len().min(7)]) {
                        confirmed = Some(&s.badge);
                    } else if branches.iter().any(|b| b.as_deref() == Some(branch))
                        && ts - 5.0 <= ct as f64
                        && ct as f64 <= ts + COMMIT_LAG
                        && inferred.is_none_or(|(t, _)| *ts > t)
                    {
                        inferred = Some((*ts, &s.badge));
                    }
                }
            }
            let badge = confirmed.or(inferred.map(|(_, b)| b)).map(str::to_string);
            (
                graph.to_string(),
                Some(sha.to_string()),
                Some(ct),
                subject.to_string(),
                badge,
                confirmed.is_some(),
            )
        })
        .collect()
}
