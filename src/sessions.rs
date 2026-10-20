//! Where each herdr agent works, from the tool calls at the end of its Claude or Codex
//! transcript.
//!
//! A tool call hits a worktree (by path, or outputs/tasks/<name> for worktree <name>) or a PR
//! (gh pr <verb> N, repos/.../pulls/N). Hits are graded:
//!   changed  Edit/Write/apply_patch, git commit/push/rebase, gh pr comment/edit, gh api writes,
//!            when the call did not fail
//!   read     any other hit
//!   records  other outputs/ paths, and the shared git directory used for object queries
//! A session works on the branch of its latest change; failing that, its latest read; failing
//! that, its working directory. A later read elsewhere is "recently visited"; other branches it
//! hit are "earlier".
//!
//! The agent also names the session: Claude by its /rename title, else the title it gave the
//! conversation, both re-appended to the transcript as it runs; Codex by its thread name in
//! ~/.codex/session_index.jsonl.
//!
//! With several projects configured, `owner` decides which one an agent works for.

use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use fancy_regex::Regex as Fancy;
use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::{self, Project};
use crate::sys;

/// Bytes read from the end of a transcript.
const TAIL_BYTES: u64 = 4 << 20;
/// Most recent tool calls considered per session.
const WINDOW: usize = 300;
const RECORDS: u8 = 0;
const READ: u8 = 1;
const CHANGED: u8 = 2;
/// Grade names, indexed by grade.
const GRADES: [&str; 3] = ["records", "read", "changed"];

const GIT_CMD: &str = r#"\bgit(?:\s+-[Cc]\s+(?:"[^"]*"|'[^']*'|\S+))*\s+"#;
/// Calls that change a branch or a PR.
static ACT: LazyLock<Fancy> = LazyLock::new(|| {
    let git = format!(r"{GIT_CMD}(?:commit|push|rebase|cherry-pick|merge|am|reset)(?![\w-])");
    Fancy::new(&[
        git.as_str(),
        r"\bgh\s+pr\s+(?:create|comment|edit|ready|review|merge|close|reopen)\b",
        r"\bgh\s+api\b[^|;&\n]*?(?:\s-X\s*(?:POST|PATCH|PUT)|--method\s+(?:POST|PATCH|PUT)|\s-[fF]\s|--field|--raw-field|--input)",
    ].join("|"))
    .unwrap()
});
/// Calls whose output names the commits they made.
static WRITES_COMMITS: LazyLock<Fancy> = LazyLock::new(|| {
    Fancy::new(&format!(
        r"{GIT_CMD}(?:commit|rebase|cherry-pick|am|merge)(?![\w-])"
    ))
    .unwrap()
});
static PATCH_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\*\*\* (?:Add|Update|Delete) File: (\S+)").unwrap());
static EXIT_CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"exited with code (\d+)").unwrap());
static LEADING_CD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^cd\s+\S+\s*&&\s*").unwrap());

/// Where a session works; see the module docs.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Place {
    /// The branch it works on, or None.
    pub current: Option<String>,
    /// What placed it there: 'changed', 'read', 'records' or 'cwd'.
    pub how: String,
    /// When that happened, in seconds since the epoch; None for 'cwd'.
    pub since: Option<f64>,
    /// A branch read after `current`'s event.
    pub recent: Option<String>,
    /// Other branches it hit, sorted.
    pub earlier: Vec<String>,
    /// {branch: [(time, grade or 'failed', 'Tool: text')]}, the last four per branch.
    pub evidence: IndexMap<String, Vec<(f64, String, String)>>,
    /// [(time, branches, output)] of its git commit, rebase or cherry-pick calls.
    pub commit_calls: Vec<(f64, Vec<Option<String>>, String)>,
    /// Modification time of its transcript.
    pub last: Option<f64>,
}

struct Call {
    ts: f64,
    tool: String,
    text: String,
    act: bool,
    ok: Option<bool>,
    result: String,
    commits: bool,
}

impl Call {
    fn new(ts: f64, tool: &str, text: String, act: bool) -> Call {
        let commits = tool == "Bash" && WRITES_COMMITS.is_match(&text).unwrap_or(false);
        Call {
            ts,
            tool: tool.to_string(),
            text,
            act,
            ok: None,
            result: String::new(),
            commits,
        }
    }

    /// 'cd <ws>/experiments/x && grep ...' -> 'Bash: grep ...', workspace paths shortened.
    fn short_text(&self) -> String {
        let text = self.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let text = LEADING_CD
            .replace(&text, "")
            .replace(
                &format!("{}/experiments/", config::get().project().workspace),
                "",
            )
            .replace(&format!("{}/", config::get().project().workspace), "");
        format!("{}: {text}", self.tool)
    }
}

/// Text of a JSON value as Python's str() gives it, for tool inputs.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        other => other.to_string(),
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn id_key(v: Option<&Value>) -> Option<String> {
    v.filter(|v| !v.is_null())
        .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
}

fn truncate(text: &str) -> String {
    text.chars().take(4000).collect()
}

/// A name as one line without control characters, or None when nothing is left.
pub fn clean_name(name: &str) -> Option<String> {
    let name = name
        .split(char::is_control)
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ");
    (!name.is_empty()).then_some(name)
}

/// The latest /rename title in a Claude transcript, else the latest title Claude gave it.
fn claude_name(lines: &[&str]) -> Option<String> {
    let mut ai_title = None;
    for line in lines.iter().rev() {
        if !line.contains("-title\"") {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let name = |key: &str| record.get(key).and_then(Value::as_str).and_then(clean_name);
        match record.get("type").and_then(Value::as_str) {
            Some("custom-title") => {
                if let Some(name) = name("customTitle") {
                    return Some(name);
                }
            }
            Some("ai-title") if ai_title.is_none() => ai_title = name("aiTitle"),
            _ => {}
        }
    }
    ai_title
}

/// Codex thread names by session id: the last entry for each id in session_index.jsonl.
static CODEX_NAMES: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    let index = sys::home().join(".codex/session_index.jsonl");
    std::fs::read_to_string(index)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|entry| {
            let id = entry.get("id")?.as_str()?.to_string();
            Some((id, clean_name(entry.get("thread_name")?.as_str()?)?))
        })
        .collect()
});

fn claude_calls(lines: &[&str]) -> Vec<Call> {
    let (mut calls, mut by_id) = (Vec::<Call>::new(), HashMap::new());
    let empty = Map::new();
    for line in lines {
        if !line.contains("\"tool_use\"") && !line.contains("\"tool_result\"") {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(ts) = record
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(sys::epoch)
        else {
            continue;
        };
        let Some(blocks) = record
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for block in blocks.iter().filter_map(Value::as_object) {
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    let input = block
                        .get("input")
                        .and_then(Value::as_object)
                        .unwrap_or(&empty);
                    let text = ["command", "file_path", "path", "notebook_path"]
                        .iter()
                        .filter_map(|k| input.get(*k).map(py_str))
                        .collect::<Vec<_>>()
                        .join(" ");
                    let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                    let act = matches!(name, "Edit" | "Write" | "NotebookEdit")
                        || input.contains_key("command") && ACT.is_match(&text).unwrap_or(false);
                    by_id.insert(id_key(block.get("id")), calls.len());
                    calls.push(Call::new(ts, name, text, act));
                }
                Some("tool_result") => {
                    let Some(&i) = by_id.get(&id_key(block.get("tool_use_id"))) else {
                        continue;
                    };
                    let call = &mut calls[i];
                    call.ok = Some(!block.get("is_error").is_some_and(truthy));
                    if call.commits {
                        call.result = truncate(&match block.get("content") {
                            Some(Value::String(s)) => s.clone(),
                            Some(Value::Array(parts)) => parts
                                .iter()
                                .filter_map(Value::as_object)
                                .map(|p| p.get("text").and_then(Value::as_str).unwrap_or(""))
                                .collect::<Vec<_>>()
                                .join(" "),
                            _ => String::new(),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    calls
}

fn codex_calls(lines: &[&str]) -> Vec<Call> {
    let (mut calls, mut by_id) = (Vec::<Call>::new(), HashMap::new());
    let empty = Map::new();
    for line in lines {
        if !line.contains("\"function_call") && !line.contains("\"custom_tool_call") {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(ts) = record
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(sys::epoch)
        else {
            continue;
        };
        let p = record
            .get("payload")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        let call_id = id_key(p.get("call_id"));
        match p.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                let args: Value = p
                    .get("arguments")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .and_then(|s| serde_json::from_str(s).ok())
                    .filter(Value::is_object)
                    .unwrap_or_else(|| Value::Object(Map::new()));
                let cmd = match args
                    .get("cmd")
                    .filter(|v| truthy(v))
                    .or(args.get("command").filter(|v| truthy(v)))
                {
                    Some(Value::Array(parts)) => {
                        parts.iter().map(py_str).collect::<Vec<_>>().join(" ")
                    }
                    Some(v) => py_str(v),
                    None => String::new(),
                };
                let workdir = args.get("workdir").map(py_str).unwrap_or_default();
                let text = format!("{cmd} {workdir}").trim().to_string();
                by_id.insert(call_id, calls.len());
                calls.push(Call::new(
                    ts,
                    "Bash",
                    text,
                    ACT.is_match(&cmd).unwrap_or(false),
                ));
            }
            Some("custom_tool_call")
                if p.get("name").and_then(Value::as_str) == Some("apply_patch") =>
            {
                let input = p.get("input").map(py_str).unwrap_or_default();
                let paths: Vec<&str> = PATCH_FILE
                    .captures_iter(&input)
                    .map(|c| c.get(1).unwrap().as_str())
                    .collect();
                by_id.insert(call_id, calls.len());
                calls.push(Call::new(ts, "Edit", paths.join(" "), true));
            }
            Some("function_call_output" | "custom_tool_call_output") => {
                let Some(&i) = by_id.get(&call_id) else {
                    continue;
                };
                let out = match p.get("output") {
                    Some(Value::Object(o)) => o.get("output").map(py_str).unwrap_or_default(),
                    Some(v) if truthy(v) => py_str(v),
                    _ => String::new(),
                };
                let call = &mut calls[i];
                call.ok = Some(EXIT_CODE.captures(&out).is_none_or(|c| &c[1] == "0"));
                if call.commits {
                    call.result = truncate(&out);
                }
            }
            _ => {}
        }
    }
    calls
}

/// A session's transcript: Claude projects/<dir>/<id>.jsonl or Codex rollout-*-<id>.jsonl,
/// the most recently modified when several match.
fn transcript_path(kind: &str, sid: &str) -> Option<PathBuf> {
    fn entries(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default()
    }
    let home = sys::home();
    let found: Vec<PathBuf> = match kind {
        "claude" => entries(&home.join(".claude/projects"))
            .into_iter()
            .map(|d| d.join(format!("{sid}.jsonl")))
            .filter(|p| p.exists())
            .collect(),
        "codex" => {
            let suffix = format!("-{sid}.jsonl");
            entries(&home.join(".codex/sessions"))
                .iter()
                .flat_map(|y| entries(y))
                .flat_map(|m| entries(&m))
                .flat_map(|d| entries(&d))
                .filter(|f| {
                    f.file_name().map(|n| n.to_string_lossy()).is_some_and(|n| {
                        n.starts_with("rollout-")
                            && n.ends_with(&suffix)
                            && n.len() >= "rollout-".len() + suffix.len()
                    })
                })
                .collect()
        }
        _ => Vec::new(),
    };
    let mut best: Option<(f64, PathBuf)> = None;
    for path in found {
        let t = sys::mtime(&path).unwrap_or(f64::MIN);
        if best.as_ref().is_none_or(|(bt, _)| t > *bt) {
            best = Some((t, path));
        }
    }
    best.map(|(_, p)| p)
}

/// A transcript's last tool calls and the name its tail gives the session (read_tail).
type Tail = (Vec<Call>, Option<String>);

/// read_tail once per process: deciding which project an agent works for may locate it more
/// than once.
fn tail(kind: &str, path: &Path) -> Arc<Tail> {
    static TAILS: LazyLock<Mutex<HashMap<PathBuf, Arc<Tail>>>> = LazyLock::new(Mutex::default);
    let mut tails = TAILS.lock().unwrap();
    tails
        .entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(read_tail(kind, path)))
        .clone()
}

/// The last WINDOW tool calls of a transcript, oldest first, and the name its tail gives the
/// session (Claude only).
fn read_tail(kind: &str, path: &Path) -> Tail {
    let mut buf = Vec::new();
    let Ok(mut f) = std::fs::File::open(path) else {
        return (Vec::new(), None);
    };
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    if f.seek(SeekFrom::Start(size.saturating_sub(TAIL_BYTES)))
        .is_err()
        || f.read_to_end(&mut buf).is_err()
    {
        return (Vec::new(), None);
    }
    let text = String::from_utf8_lossy(&buf);
    // The first line of a tail starts mid-record.
    let lines: Vec<&str> = text.lines().skip(usize::from(size > TAIL_BYTES)).collect();
    let (mut calls, name) = if kind == "claude" {
        (claude_calls(&lines), claude_name(&lines))
    } else {
        (codex_calls(&lines), None)
    };
    calls.drain(..calls.len().saturating_sub(WINDOW));
    (calls, name)
}

/// Escapes text for a regex, leaving characters outside the syntax alone.
fn escape(text: &str) -> String {
    text.chars().fold(String::new(), |mut out, c| {
        if "\\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
        out
    })
}

/// Characters that continue a path or name; one before a match means it is not standalone.
fn path_char(c: char) -> bool {
    c.is_alphanumeric() || "_/.-".contains(c)
}

/// Where the paths occur in `text`, as the regex `(?:p1|p2|...)(?=[/\s'"`;:)|&]|$)` finds
/// them, with `(?<![\w/.-])` in front when `standalone`: scanning left to right, the first path
/// in order that fits at a position is taken and the scan resumes after it. Literal search
/// with these checks is several times faster than a backtracking regex with lookaround.
///
/// Returns:
///   [(end of the match, path, branch)].
fn find_paths<'a>(
    text: &str,
    paths: &'a [(String, String)],
    standalone: bool,
) -> Vec<(usize, &'a str, &'a str)> {
    let ends = |i: usize| {
        text[i..]
            .chars()
            .next()
            .is_none_or(|c| c.is_whitespace() || "/'\"`;:)|&".contains(c))
    };
    let free = |i: usize| text[..i].chars().next_back().is_none_or(|c| !path_char(c));
    let mut found = Vec::new();
    for (order, (path, branch)) in paths.iter().enumerate() {
        let mut from = 0;
        while let Some(i) = text[from..].find(path.as_str()).map(|i| i + from) {
            if ends(i + path.len()) && (!standalone || free(i)) {
                found.push((i, order, path.as_str(), branch.as_str()));
            }
            from = i + text[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    found.sort_unstable_by_key(|&(i, order, _, _)| (i, order));
    let mut resume = 0;
    found
        .into_iter()
        .filter(|&(i, _, path, _)| {
            i >= resume && {
                resume = i + path.len();
                true
            }
        })
        .map(|(i, _, path, branch)| (i + path.len(), path, branch))
        .collect()
}

/// Names after 'outputs/tasks/' at the workspace root, as the regex
/// `(?:<workspace>/|(?<![\w/.-]))outputs/tasks/([\w.-]+)` finds them.
fn root_tasks<'a>(text: &'a str, workspace: &str) -> impl Iterator<Item = &'a str> {
    const ANCHOR: &str = "outputs/tasks/";
    let root = format!("{workspace}/");
    text.match_indices(ANCHOR).filter_map(move |(j, _)| {
        let before = &text[..j];
        if !before.ends_with(&root) && before.chars().next_back().is_some_and(path_char) {
            return None;
        }
        let rest = &text[j + ANCHOR.len()..];
        let n = rest
            .find(|c: char| !(c.is_alphanumeric() || "_.-".contains(c)))
            .unwrap_or(rest.len());
        (n > 0).then(|| &rest[..n])
    })
}

/// Maps one project's tool-call text to (branch, grade) hits; see the module docs.
pub struct Locator {
    workspace: String,
    checkout: String,
    /// Where its agents work (config::Project::roots).
    roots: Vec<(String, bool)>,
    /// (path, branch), longest path first; `rel` holds those in the workspace, relative to it.
    trees: Vec<(String, String)>,
    rel: Vec<(String, String)>,
    aliases: Vec<(String, String)>,
    task_re: Regex,
    pr_re: Regex,
    by_name: HashMap<String, String>,
    /// {number: head branch} of the PRs on its map.
    pr_heads: HashMap<u64, String>,
}

impl Locator {
    /// Args:
    ///   trees: sources::worktrees of the project's checkout.
    ///   pr_heads: {number: head branch} of the PRs on the project's map.
    pub fn new(
        project: &Project,
        trees: &[(String, String)],
        pr_heads: HashMap<u64, String>,
    ) -> Locator {
        let aliases = project
            .aliases
            .iter()
            .filter(|a| std::fs::symlink_metadata(a).is_ok_and(|m| m.file_type().is_symlink()))
            .filter_map(|a| {
                Some((
                    format!("{a}/"),
                    format!("{}/", std::fs::canonicalize(a).ok()?.display()),
                ))
            })
            .collect();
        let under = format!("{}/", project.workspace);
        let name = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
        Locator {
            workspace: project.workspace.clone(),
            checkout: project.checkout.clone(),
            roots: project.roots(trees),
            trees: trees.to_vec(),
            rel: trees.iter().filter_map(|(p, b)| Some((p.strip_prefix(&under)?.to_string(), b.clone()))).collect(),
            aliases,
            task_re: Regex::new(r"^/outputs/tasks/([\w.-]+)").unwrap(),
            pr_re: Regex::new(&format!(
                r"gh\s+pr\s+(?:view|comment|edit|diff|checks|ready|review|close|reopen|merge)\s+(\d+)|repos/{}/(?:pulls|issues)/(\d+)",
                escape(&project.repo))).unwrap(),
            by_name: trees.iter().map(|(p, b)| (name(p), b.clone())).collect(),
            pr_heads,
        }
    }

    /// How deep the project reaches around `dir` (config::claim).
    pub fn claim(&self, dir: &str) -> Option<(usize, bool)> {
        config::claim(&self.roots, dir)
    }

    /// Rewrites compatibility links to the paths they point at.
    pub fn normalize(&self, text: &str) -> String {
        self.aliases
            .iter()
            .fold(text.to_string(), |t, (alias, target)| {
                t.replace(alias, target)
            })
    }

    fn hits(&self, text: &str, act: bool) -> Vec<(String, u8)> {
        let text = self.normalize(text);
        let top = if act { CHANGED } else { READ };
        let git = &self.checkout;
        let git_rel = git
            .strip_prefix(&format!("{}/", self.workspace))
            .unwrap_or(git);
        let mut out = Vec::new();
        for (paths, standalone) in [(&self.trees, false), (&self.rel, true)] {
            for (end, path, branch) in find_paths(&text, paths, standalone) {
                let rest = &text[end..];
                let task = self
                    .task_re
                    .captures(rest)
                    .and_then(|c| self.by_name.get(&c[1]));
                if let Some(task) = task {
                    out.push((task.clone(), READ));
                } else if rest.starts_with("/outputs/") || !act && (path == git || path == git_rel)
                {
                    // outputs/ holds records of whatever task the host worktree carries, and the
                    // shared git directory serves object queries for every branch.
                    out.push((branch.to_string(), RECORDS));
                } else {
                    out.push((branch.to_string(), top));
                }
            }
        }
        for b in root_tasks(&text, &self.workspace).filter_map(|n| self.by_name.get(n)) {
            out.push((b.clone(), READ));
        }
        for c in self.pr_re.captures_iter(&text) {
            let n = c
                .get(1)
                .or(c.get(2))
                .and_then(|m| m.as_str().parse::<u64>().ok());
            if let Some(head) = n.and_then(|n| self.pr_heads.get(&n)) {
                out.push((head.clone(), top));
            }
        }
        out
    }

    fn branch_of_dir(&self, path: &str) -> Option<String> {
        let path = self.normalize(&format!("{}/", path.trim_end_matches('/')));
        self.trees
            .iter()
            .find(|(p, _)| path.starts_with(&format!("{p}/")))
            .map(|(_, b)| b.clone())
    }
}

/// First non-empty string among an object's keys.
pub fn first_str<'a>(v: &'a Value, keys: &[&str]) -> &'a str {
    keys.iter()
        .filter_map(|k| v.get(*k).and_then(Value::as_str))
        .find(|s| !s.is_empty())
        .unwrap_or("")
}

/// The project an agent works for, as an index into `locators` (one per configured project),
/// or None.
///
/// The projects with the deepest root around its directory `dir` claim it, a worktree before
/// a workspace or alias as deep: an agent in one repository's checkout works for that
/// repository. A project claiming it through its workspace still takes it when its transcript
/// places it on one of that project's branches (`placed`), as when it drives the project's
/// worktrees from that checkout. An agent outside every project works for the first one that
/// places it.
pub fn owner(dir: &str, locators: &[Locator], placed: impl Fn(usize) -> bool) -> Option<usize> {
    let claims: Vec<(usize, (usize, bool))> = locators
        .iter()
        .enumerate()
        .filter_map(|(i, l)| Some((i, l.claim(dir)?)))
        .collect();
    let Some(deepest) = claims.iter().map(|(_, (depth, _))| *depth).max() else {
        return (0..locators.len()).find(|&i| placed(i));
    };
    let tied: Vec<(usize, bool)> = claims
        .into_iter()
        .filter(|(_, (depth, _))| *depth == deepest)
        .map(|(i, (_, tree))| (i, tree))
        .collect();
    if let [(only, _)] = tied.as_slice() {
        return Some(*only);
    }
    tied.iter()
        .find(|&&(i, tree)| !tree && placed(i))
        .or_else(|| tied.iter().find(|(_, tree)| *tree))
        .or(tied.first())
        .map(|&(i, _)| i)
}

/// Places one herdr agent on a project's branches and finds the name its agent gives the
/// session; see the module docs.
pub fn locate(agent: &Value, locator: &Locator) -> (Place, Option<String>) {
    let sid = agent
        .get("agent_session")
        .and_then(|s| s.get("value"))
        .and_then(Value::as_str);
    let kind = agent.get("agent").and_then(Value::as_str).unwrap_or("");
    let transcript = sid
        .filter(|s| !s.is_empty())
        .and_then(|sid| transcript_path(kind, sid));
    let cwd_branch = locator.branch_of_dir(first_str(agent, &["foreground_cwd", "cwd"]));
    let mut events: Vec<(f64, String, u8)> = Vec::new();
    let mut evidence: IndexMap<String, Vec<(f64, String, String)>> = IndexMap::new();
    let mut commit_calls = Vec::new();
    let tail = transcript.as_deref().map(|p| tail(kind, p));
    let (calls, transcript_name) = tail.as_deref().map_or((&[][..], None), |(calls, name)| {
        (calls.as_slice(), name.clone())
    });
    let name = match kind {
        "codex" => sid.and_then(|sid| CODEX_NAMES.get(sid)).cloned(),
        _ => transcript_name,
    };
    for call in calls {
        let mut grades: IndexMap<String, u8> = IndexMap::new();
        for (key, grade) in locator.hits(&call.text, call.act) {
            let best = grades.entry(key).or_insert(grade);
            *best = (*best).max(grade);
        }
        let failed = call.ok == Some(false);
        for (branch, &grade) in &grades {
            let grade = if grade == CHANGED && failed {
                READ
            } else {
                grade
            };
            events.push((call.ts, branch.clone(), grade));
            let word = if failed && call.act {
                "failed"
            } else {
                GRADES[grade as usize]
            };
            evidence.entry(branch.clone()).or_default().push((
                call.ts,
                word.into(),
                call.short_text(),
            ));
        }
        if call.commits && !failed {
            let branches = if grades.is_empty() {
                vec![cwd_branch.clone()]
            } else {
                grades.keys().cloned().map(Some).collect()
            };
            commit_calls.push((call.ts, branches, call.result.clone()));
        }
    }
    let latest = |test: &dyn Fn(u8) -> bool| events.iter().rev().find(|e| test(e.2));
    let home = latest(&|g| g == CHANGED)
        .or_else(|| latest(&|g| g == READ))
        .or_else(|| latest(&|_| true));
    let (current, how, since) = match home {
        Some((ts, b, g)) => (Some(b.clone()), GRADES[*g as usize].to_string(), Some(*ts)),
        None => (cwd_branch, "cwd".to_string(), None),
    };
    let seen = latest(&|g| g >= READ);
    let recent = match (seen, home) {
        (Some(s), Some(h)) if Some(&s.1) != current.as_ref() && s.0 > h.0 => Some(s.1.clone()),
        _ => None,
    };
    let earlier: BTreeSet<String> = events
        .iter()
        .map(|e| e.1.clone())
        .filter(|b| Some(b) != current.as_ref() && Some(b) != recent.as_ref())
        .collect();
    for ev in evidence.values_mut() {
        ev.drain(..ev.len().saturating_sub(4));
    }
    let place = Place {
        current,
        how,
        since,
        recent,
        earlier: earlier.into_iter().collect(),
        evidence,
        commit_calls,
        last: transcript.as_deref().and_then(sys::mtime),
    };
    (place, name)
}

/// Two-letter badge for a session label: 'search-data' -> 'SE', 'index-for-receipts' -> 'IR'.
pub fn initials(label: &str) -> String {
    let lower = label.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty() && !["data", "for", "the", "of", "and"].contains(w))
        .collect();
    match words.as_slice() {
        [] => "??".into(),
        [one] => one.chars().take(2).collect::<String>().to_uppercase(),
        [a, b, ..] => format!("{}{}", &a[..1], &b[..1]).to_uppercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rename_names_a_claude_session_before_its_own_title() {
        let record = |kind: &str, key: &str, name: &str| {
            serde_json::json!({"type": kind, key: name}).to_string()
        };
        let titled = |name: &str| record("ai-title", "aiTitle", name);
        let renamed = |name: &str| record("custom-title", "customTitle", name);
        let name =
            |lines: &[String]| claude_name(&lines.iter().map(String::as_str).collect::<Vec<_>>());
        let topic = name(&[titled("Old topic"), titled("Search tuning")]);
        assert_eq!(topic.as_deref(), Some("Search tuning"));
        let both = name(&[renamed("search-dev"), titled("Search tuning")]);
        assert_eq!(both.as_deref(), Some("search-dev"));
        let twice = name(&[renamed("old"), renamed("search-dev")]);
        assert_eq!(twice.as_deref(), Some("search-dev"));
        // Control characters never reach the terminal; a blank rename names nothing.
        let unsafe_name = name(&[titled("a\x1b[2Jb\nc"), renamed(" \t")]);
        assert_eq!(unsafe_name.as_deref(), Some("a [2Jb c"));
        assert_eq!(name(&[]), None);
    }

    #[test]
    fn an_agent_works_for_the_deepest_project_unless_a_workspace_places_it() {
        // 'o/tool' works from /w, the checkout of 'o/ws'; both keep worktrees in /w/experiments.
        let project = |repo: &str| Project {
            repo: repo.into(),
            workspace: "/w".into(),
            checkout: "/w".into(),
            aliases: Vec::new(),
            cache: PathBuf::new(),
        };
        let tree = |path: &str, branch: &str| (path.to_string(), branch.to_string());
        let locators = [
            Locator::new(
                &project("o/tool"),
                &[tree("/w/experiments/a", "feat/a")],
                HashMap::new(),
            ),
            Locator::new(
                &project("o/ws"),
                &[tree("/w", "main"), tree("/w/experiments/b", "tools/b")],
                HashMap::new(),
            ),
        ];
        let owner = |dir: &str, placed: &[usize]| owner(dir, &locators, |i| placed.contains(&i));
        // A worktree's agent works for its project, whatever its transcript names.
        assert_eq!(owner("/w/experiments/a/src", &[1]), Some(0));
        assert_eq!(owner("/w/experiments/b", &[0]), Some(1));
        // In the checkout, for its repository unless the workspace's project places the agent.
        assert_eq!(owner("/w", &[]), Some(1));
        assert_eq!(owner("/w/reports", &[0]), Some(0));
        // Outside every project, for the first that places it.
        assert_eq!(owner("/elsewhere", &[1]), Some(1));
        assert_eq!(owner("/elsewhere", &[]), None);
    }
}
