//! Background collectors. Each worker owns one data source and one refresh cadence,
//! so a slow `gh` call never delays agent-state updates.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Default)]
pub struct Agent {
    pub status: String,
    /// Which coding agent: herdr's `display_agent` when a pane reports one, else its canonical id
    /// (`claude`, `codex`, `pi`, `opencode`, ...).
    pub name: String,
    pub title: String,
}

#[derive(Clone, Debug)]
pub struct WtRaw {
    pub path: String,
    pub branch: String,
    pub prunable: bool,
    pub linked: bool,
    pub open_ws: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct HerdrSnap {
    pub worktrees: Vec<WtRaw>,
    /// (cwd, agent) for every pane running a detected agent.
    pub agents: Vec<(String, Agent)>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct GitInfo {
    pub dirty: u32,
    pub ahead: u32,
    pub behind: u32,
    pub last_ct: i64,
    pub subject: String,
    pub files: u32,
    pub ins: u32,
    pub del: u32,
}

/// Cache rows are a superset of the bash board's `{b,n,ci,t}` so both boards can share one file.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PrInfo {
    pub b: String,
    pub n: u64,
    #[serde(default)]
    pub ci: String,
    #[serde(default)]
    pub t: u32,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub review: String,
    #[serde(default)]
    pub fails: Vec<String>,
    #[serde(default)]
    pub pending: u32,
}

pub enum Msg {
    Herdr(HerdrSnap),
    Git(HashMap<String, GitInfo>),
    Pr(Vec<PrInfo>, i64),
    Status(String, bool),
}

/// Seconds from a `WORKTREES_*` setting (config.env), falling back to `default`.
pub fn env_secs(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).filter(|&n| n > 0).unwrap_or(default)
}

pub fn herdr_bin() -> String {
    std::env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".into())
}

pub fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn herdr_json(args: &[&str]) -> Option<Value> {
    serde_json::from_str(&run(&herdr_bin(), args)?).ok()
}

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or_default().to_string()
}

pub fn herdr_snapshot(ws: &str) -> HerdrSnap {
    let Some(wt) = herdr_json(&["worktree", "list", "--workspace", ws]) else {
        return HerdrSnap { error: Some("not a git workspace".into()), ..Default::default() };
    };
    let worktrees = wt["result"]["worktrees"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|w| WtRaw {
            path: s(w, "path"),
            branch: if w["is_detached"].as_bool().unwrap_or(false) { "(detached)".into() } else { s(w, "branch") },
            prunable: w["is_prunable"].as_bool().unwrap_or(false),
            linked: w["is_linked_worktree"].as_bool().unwrap_or(false),
            open_ws: w["open_workspace_id"].as_str().map(String::from),
        })
        .collect();
    let agents = herdr_json(&["pane", "list"])
        .and_then(|p| p["result"]["panes"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter(|p| p["agent"].is_string())
        .map(|p| {
            let cwd = p["foreground_cwd"].as_str().or(p["cwd"].as_str()).unwrap_or_default().to_string();
            let agent = Agent {
                status: p["agent_status"].as_str().unwrap_or("unknown").into(),
                name: p["display_agent"]
                    .as_str()
                    .filter(|d| !d.is_empty())
                    .map(String::from)
                    .unwrap_or_else(|| s(p, "agent")),
                title: s(p, "terminal_title_stripped"),
            };
            (cwd, agent)
        })
        .collect();
    HerdrSnap { worktrees, agents, error: None }
}

pub fn spawn_herdr(ws: String, tx: Sender<Msg>, kick: Receiver<()>) {
    std::thread::spawn(move || loop {
        if tx.send(Msg::Herdr(herdr_snapshot(&ws))).is_err() {
            return;
        }
        let _ = kick.recv_timeout(Duration::from_millis(1500));
    });
}

fn git(path: &str, args: &[&str]) -> Option<String> {
    let mut full = vec!["-C", path];
    full.extend_from_slice(args);
    run("git", &full)
}

pub fn git_info(path: &str, base: &str) -> GitInfo {
    let mut g = GitInfo {
        dirty: git(path, &["status", "--porcelain"]).map(|o| o.lines().count() as u32).unwrap_or(0),
        ..Default::default()
    };
    if let Some(o) = git(path, &["rev-list", "--left-right", "--count", &format!("{base}...HEAD")]) {
        let mut it = o.split_whitespace().map(|n| n.parse().unwrap_or(0));
        g.behind = it.next().unwrap_or(0);
        g.ahead = it.next().unwrap_or(0);
    }
    if let Some(o) = git(path, &["log", "-1", "--format=%ct%x1f%s"]) {
        let (ct, subj) = o.trim_end().split_once('\x1f').unwrap_or(("0", ""));
        g.last_ct = ct.parse().unwrap_or(0);
        g.subject = subj.to_string();
    }
    if g.ahead > 0 {
        if let Some(o) = git(path, &["diff", "--shortstat", &format!("{base}...HEAD")]) {
            for part in o.split(',') {
                let n: u32 = part.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0);
                if part.contains("file") {
                    g.files = n;
                } else if part.contains("insertion") {
                    g.ins = n;
                } else if part.contains("deletion") {
                    g.del = n;
                }
            }
        }
    }
    g
}

pub fn spawn_git(paths: Arc<Mutex<Vec<String>>>, base: String, tx: Sender<Msg>, kick: Receiver<()>) {
    let every = Duration::from_secs(env_secs("WORKTREES_GIT_EVERY", 10));
    std::thread::spawn(move || loop {
        let list = paths.lock().map(|p| p.clone()).unwrap_or_default();
        let infos =
            list.iter().filter(|p| std::path::Path::new(p).is_dir()).map(|p| (p.clone(), git_info(p, &base))).collect();
        if tx.send(Msg::Git(infos)).is_err() {
            return;
        }
        let _ = kick.recv_timeout(every);
    });
}

const PR_QUERY: &str = r#"query($owner:String!,$name:String!){repository(owner:$owner,name:$name){
  pullRequests(states:OPEN,first:100,orderBy:{field:UPDATED_AT,direction:DESC}){nodes{
    number headRefName url isDraft reviewDecision
    reviewThreads(first:100){nodes{isResolved}}
    commits(last:1){nodes{commit{statusCheckRollup{state contexts(first:100){nodes{
      __typename ... on CheckRun{name status conclusion} ... on StatusContext{context state}}}}}}}}}}}"#;

const PR_JQ: &str = r#"[.data.repository.pullRequests.nodes[]
  | (.commits.nodes[0].commit.statusCheckRollup // {}) as $r
  | ($r.contexts.nodes // []) as $c
  | {b: .headRefName, n: .number, url: .url, draft: .isDraft, review: (.reviewDecision // ""),
     ci: ($r.state // ""),
     t: ([.reviewThreads.nodes[] | select(.isResolved | not)] | length),
     fails: [$c[] | select((.conclusion // .state // "") as $s
               | ["FAILURE","TIMED_OUT","CANCELLED","ACTION_REQUIRED","STARTUP_FAILURE","ERROR"] | index($s))
             | (.name // .context)],
     pending: ([$c[] | select(.__typename == "CheckRun" and .status != "COMPLETED")] | length)}]"#;

/// Shared cache: every board on the repo reads it; a stale mtime lets one of them refresh.
/// Touching the file before the slow call claims the refresh slot.
pub fn spawn_pr(slug: String, cache: PathBuf, gh: String, tx: Sender<Msg>, kick: Receiver<()>) {
    let ttl = env_secs("WORKTREES_PR_TTL", 60) as i64;
    std::thread::spawn(move || loop {
        let mtime = |p: &PathBuf| {
            std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        };
        let stale = mtime(&cache).map(|t| now() - t.as_secs() as i64 >= ttl).unwrap_or(true);
        if stale {
            let _ = touch(&cache);
            let (owner, name) = slug.split_once('/').unwrap_or_default();
            let args = [
                "api",
                "graphql",
                "-F",
                &format!("owner={owner}"),
                "-F",
                &format!("name={name}"),
                "-f",
                &format!("query={PR_QUERY}"),
                "--jq",
                PR_JQ,
            ];
            if let Some(out) = run(&gh, &args) {
                let tmp = cache.with_extension(format!("tmp.{}", std::process::id()));
                if std::fs::write(&tmp, out).is_ok() {
                    let _ = std::fs::rename(&tmp, &cache);
                }
            }
        }
        let prs: Vec<PrInfo> =
            std::fs::read_to_string(&cache).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let at = mtime(&cache).map(|t| t.as_secs() as i64).unwrap_or(0);
        if tx.send(Msg::Pr(prs, at)).is_err() {
            return;
        }
        let _ = kick.recv_timeout(Duration::from_secs(5));
    });
}

fn touch(p: &PathBuf) -> std::io::Result<()> {
    std::fs::File::options().create(true).append(true).open(p)?.set_modified(SystemTime::now())
}
