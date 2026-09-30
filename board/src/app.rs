//! Board state: joins the three data sources into ranked rows, tracks selection by path,
//! animations, and runs user actions off the UI thread.

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui::layout::Rect;

use crate::data::{self, Agent, GitInfo, HerdrSnap, Msg, PrInfo};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Place {
    Here,
    Open,
    Closed,
    Prunable,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub path: String,
    pub branch: String,
    pub place: Place,
    pub linked: bool,
    pub open_ws: Option<String>,
    /// Most urgent first.
    pub agents: Vec<Agent>,
    pub git: Option<GitInfo>,
    pub pr: Option<PrInfo>,
}

impl Row {
    pub fn top_status(&self) -> &str {
        self.agents.first().map(|a| a.status.as_str()).unwrap_or("")
    }
    pub fn ci_failed(&self) -> bool {
        self.pr.as_ref().is_some_and(|p| p.ci == "FAILURE" || p.ci == "ERROR")
    }
    /// Attention order: needs you > CI broken > finished > busy > quiet.
    fn rank(&self) -> u8 {
        match self.top_status() {
            "blocked" => 0,
            _ if self.ci_failed() => 1,
            "done" => 2,
            "working" => 3,
            _ => 4,
        }
    }
}

fn urgency(status: &str) -> u8 {
    match status {
        "blocked" => 0,
        "working" => 1,
        "done" => 2,
        "idle" => 3,
        _ => 4,
    }
}

pub enum Mode {
    Normal,
    Filter,
    NewBranch(String),
    ConfirmDelete(String),
}

/// Vertical position of a card, eased from its old slot to its new one when rows re-sort.
struct Slide {
    from: f32,
    to: f32,
    start: Instant,
}

/// Index of the git worker's kick channel in `App::kicks` (herdr, git, pr order in main).
pub const GIT_KICK: usize = 1;

pub const SLIDE: Duration = Duration::from_millis(260);
pub const PULSE: Duration = Duration::from_millis(3000);

pub struct App {
    pub ws: String,
    pub here: String,
    pub repo: String,
    pub root: String,
    pub base: String,
    pub plugin_root: String,
    pub snap: HerdrSnap,
    pub git: HashMap<String, GitInfo>,
    pub prs: Vec<PrInfo>,
    pub pr_at: i64,
    pub rows: Vec<Row>,
    pub selected: Option<String>,
    pub hover: Option<String>,
    pub expanded: bool,
    pub filter: String,
    pub mode: Mode,
    pub status: Option<(String, bool, Instant)>,
    pub started: Instant,
    pub scroll: u16,
    /// Card rects from the last frame, for mouse hit-testing.
    pub hits: Vec<(Rect, String)>,
    pub last_click: Option<(String, Instant)>,
    pub git_paths: Arc<Mutex<Vec<String>>>,
    pub tx: Sender<Msg>,
    pub kicks: Vec<Sender<()>>,
    pub quit: bool,
    slides: HashMap<String, Slide>,
    pulses: HashMap<String, Instant>,
    prev_status: HashMap<String, String>,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ws: String,
        here: String,
        repo: String,
        root: String,
        base: String,
        plugin_root: String,
        git_paths: Arc<Mutex<Vec<String>>>,
        tx: Sender<Msg>,
        kicks: Vec<Sender<()>>,
    ) -> Self {
        Self {
            ws,
            here,
            repo,
            root,
            base,
            plugin_root,
            git_paths,
            tx,
            kicks,
            snap: HerdrSnap::default(),
            git: HashMap::new(),
            prs: Vec::new(),
            pr_at: 0,
            rows: Vec::new(),
            selected: None,
            hover: None,
            expanded: false,
            filter: String::new(),
            mode: Mode::Normal,
            status: None,
            started: Instant::now(),
            scroll: 0,
            hits: Vec::new(),
            last_click: None,
            quit: false,
            slides: HashMap::new(),
            pulses: HashMap::new(),
            prev_status: HashMap::new(),
        }
    }

    pub fn apply(&mut self, msg: Msg) {
        match msg {
            Msg::Herdr(s) => {
                let paths: Vec<String> = s.worktrees.iter().map(|w| w.path.clone()).collect();
                let changed = self.git_paths.lock().map(|mut p| std::mem::replace(&mut *p, paths.clone()) != paths);
                if changed.unwrap_or(false) {
                    // New or removed checkouts: rescan git now instead of at the next 10s tick.
                    let _ = self.kicks[GIT_KICK].send(());
                }
                self.snap = s;
            }
            Msg::Git(g) => self.git = g,
            Msg::Pr(p, at) => {
                self.prs = p;
                self.pr_at = at;
            }
            Msg::Status(text, ok) => {
                self.status = Some((text, ok, Instant::now()));
                self.kick();
            }
        }
        self.rebuild();
    }

    pub fn kick(&self) {
        for k in &self.kicks {
            let _ = k.send(());
        }
    }

    pub fn rebuild(&mut self) {
        let paths: Vec<&str> = self.snap.worktrees.iter().map(|w| w.path.as_str()).collect();
        // An agent belongs to the deepest worktree containing its cwd: the main checkout
        // is a parent directory of every `.worktrees/*` checkout.
        let owner = |cwd: &str| {
            paths
                .iter()
                .filter(|p| cwd == **p || cwd.strip_prefix(**p).is_some_and(|r| r.starts_with('/')))
                .max_by_key(|p| p.len())
                .map(|p| p.to_string())
        };
        let mut by_owner: HashMap<String, Vec<Agent>> = HashMap::new();
        for (cwd, a) in &self.snap.agents {
            if let Some(o) = owner(cwd) {
                by_owner.entry(o).or_default().push(a.clone());
            }
        }
        let mut rows: Vec<Row> = self
            .snap
            .worktrees
            .iter()
            .map(|w| {
                let mut agents = by_owner.remove(&w.path).unwrap_or_default();
                agents.sort_by_key(|a| urgency(&a.status));
                let place = if w.prunable {
                    Place::Prunable
                } else if w.open_ws.as_deref() == Some(self.ws.as_str()) || w.path == self.here {
                    Place::Here
                } else if w.open_ws.is_some() {
                    Place::Open
                } else {
                    Place::Closed
                };
                Row {
                    path: w.path.clone(),
                    branch: w.branch.clone(),
                    place,
                    linked: w.linked,
                    open_ws: w.open_ws.clone(),
                    agents,
                    git: self.git.get(&w.path).cloned(),
                    pr: self.prs.iter().find(|p| p.b == w.branch).cloned(),
                }
            })
            .filter(|r| fuzzy(&self.filter, &r.branch))
            .collect();
        rows.sort_by_key(Row::rank);

        for r in &rows {
            let st = r.top_status().to_string();
            if st == "blocked" && self.prev_status.get(&r.path).map(String::as_str) != Some("blocked") {
                self.pulses.insert(r.path.clone(), Instant::now());
            }
            self.prev_status.insert(r.path.clone(), st);
        }
        self.rows = rows;
        if !self.selected.as_ref().is_some_and(|s| self.rows.iter().any(|r| &r.path == s)) {
            self.selected = self.rows.first().map(|r| r.path.clone());
        }
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.rows.iter().find(|r| Some(&r.path) == self.selected.as_ref())
    }

    pub fn move_sel(&mut self, d: i32) {
        let n = self.rows.len() as i32;
        if n == 0 {
            return;
        }
        let cur = self.rows.iter().position(|r| Some(&r.path) == self.selected.as_ref()).unwrap_or(0) as i32;
        self.selected = Some(self.rows[(cur + d).clamp(0, n - 1) as usize].path.clone());
    }

    /// Record a card's target y; returns the eased y to draw it at this frame.
    pub fn slide_y(&mut self, path: &str, target: f32) -> f32 {
        let now = Instant::now();
        let s = self.slides.entry(path.to_string()).or_insert(Slide { from: target, to: target, start: now });
        if (s.to - target).abs() > f32::EPSILON {
            let cur = ease(s, now);
            *s = Slide { from: cur, to: target, start: now };
        }
        ease(s, now)
    }

    pub fn animating(&self) -> bool {
        let now = Instant::now();
        self.slides.values().any(|s| now - s.start < SLIDE) || self.pulses.values().any(|p| now - *p < PULSE)
    }

    pub fn pulse_on(&self, path: &str) -> bool {
        self.pulses.get(path).is_some_and(|p| p.elapsed() < PULSE && (p.elapsed().as_millis() / 150) % 2 == 0)
    }

    pub fn frame(&self) -> usize {
        (self.started.elapsed().as_millis() / 80) as usize
    }

    pub fn flash(&mut self, text: impl Into<String>, ok: bool) {
        self.status = Some((text.into(), ok, Instant::now()));
    }

    // ---- actions -------------------------------------------------------------------------

    fn background(&self, job: impl FnOnce() -> Result<String, String> + Send + 'static) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let (text, ok) = match job() {
                Ok(t) => (t, true),
                Err(e) => (e, false),
            };
            let _ = tx.send(Msg::Status(text, ok));
        });
    }

    pub fn open_selected(&mut self) {
        let Some(r) = self.selected_row() else { return };
        let (ws, path, branch) = (self.ws.clone(), r.path.clone(), r.branch.clone());
        self.background(move || {
            herdr(&["worktree", "open", "--workspace", &ws, "--path", &path, "--focus"])
                .map(|_| format!("opened {branch}"))
        });
    }

    pub fn create(&mut self, branch: String) {
        let (ws, base) = (self.ws.clone(), self.base.clone());
        self.flash(format!("creating {branch}…"), true);
        self.background(move || {
            herdr(&["worktree", "create", "--workspace", &ws, "--branch", &branch, "--base", &base, "--focus"])
                .map(|_| format!("created {branch}"))
        });
    }

    pub fn delete(&mut self, path: String) {
        let Some(r) = self.rows.iter().find(|r| r.path == path).cloned() else { return };
        if !r.linked {
            return self.flash("main checkout can't be removed", false);
        }
        let root = self.root.clone();
        self.background(move || match &r.open_ws {
            // Let herdr close the workspace and run `git worktree remove` itself.
            Some(ws) => herdr(&["worktree", "remove", "--workspace", ws]).map(|_| format!("removed {}", r.branch)),
            None => cmd("git", &["-C", &root, "worktree", "remove", &r.path]).map(|_| format!("removed {}", r.branch)),
        });
    }

    pub fn claude(&mut self) {
        let Some(r) = self.selected_row() else { return };
        let (ws, path, branch) = (self.ws.clone(), r.path.clone(), r.branch.clone());
        self.background(move || {
            let out = herdr(&["worktree", "open", "--workspace", &ws, "--path", &path, "--focus"])?;
            let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
            let target_ws = v["result"]["workspace"]["workspace_id"].as_str().unwrap_or_default().to_string();
            let pane = v["result"]["root_pane"]["pane_id"].as_str().map(String::from).or_else(|| {
                let panes = data::herdr_json(&["pane", "list"])?;
                panes["result"]["panes"].as_array()?.iter().find_map(|p| {
                    (p["workspace_id"] == target_ws.as_str() && p["label"] != crate::LABEL)
                        .then(|| p["pane_id"].as_str().map(String::from))
                        .flatten()
                })
            });
            let pane = pane.ok_or("no pane to split")?;
            let split = herdr(&["pane", "split", &pane, "--direction", "down", "--focus"])?;
            let v: serde_json::Value = serde_json::from_str(&split).map_err(|e| e.to_string())?;
            let new = v["result"]["pane"]["pane_id"].as_str().ok_or("split returned no pane")?;
            herdr(&["pane", "run", new, "claude"]).map(|_| format!("claude started in {branch}"))
        });
    }

    pub fn open_pr(&mut self) {
        match self.selected_row().and_then(|r| r.pr.clone()) {
            Some(pr) if !pr.url.is_empty() => {
                let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
                let _ = Command::new(opener)
                    .arg(&pr.url)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
                self.flash(format!("opening #{}", pr.n), true);
            }
            _ => self.flash("no PR for this branch", false),
        }
    }

    /// OSC 52: herdr forwards clipboard writes to the outer terminal.
    pub fn yank(&mut self) {
        let Some(path) = self.selected.clone() else { return };
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]52;c;{}\x07", base64(path.as_bytes()));
        let _ = out.flush();
        self.flash("path copied", true);
    }

    pub fn hide(&mut self) {
        let _ =
            Command::new("bash").arg(format!("{}/panel.sh", self.plugin_root)).arg("off").stdin(Stdio::null()).status();
        self.quit = true;
    }
}

fn ease(s: &Slide, now: Instant) -> f32 {
    let t = ((now - s.start).as_secs_f32() / SLIDE.as_secs_f32()).min(1.0);
    let e = 1.0 - (1.0 - t).powi(3); // ease-out cubic
    s.from + (s.to - s.from) * e
}

/// Case-insensitive subsequence match, like most fuzzy finders' first pass.
pub fn fuzzy(needle: &str, hay: &str) -> bool {
    let mut it = hay.chars().flat_map(char::to_lowercase);
    needle.chars().flat_map(char::to_lowercase).all(|c| it.any(|h| h == c))
}

fn cmd(bin: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(bin).args(args).stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let out_text = String::from_utf8_lossy(&out.stdout);
        let msg = if err.trim().is_empty() { out_text } else { err };
        Err(msg.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("failed").trim().to_string())
    }
}

fn herdr(args: &[&str]) -> Result<String, String> {
    cmd(&data::herdr_bin(), args)
}

fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            out.push(if i <= chunk.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::WtRaw;

    const MAIN: &str = "/repo";
    const FEAT: &str = "/repo/.worktrees/feat";

    fn app() -> App {
        let (tx, _rx) = std::sync::mpsc::channel();
        let kicks = (0..3).map(|_| std::sync::mpsc::channel().0).collect();
        App::new(
            "w1".into(),
            String::new(),
            "repo".into(),
            MAIN.into(),
            "origin/main".into(),
            String::new(),
            Arc::new(Mutex::new(Vec::new())),
            tx,
            kicks,
        )
    }

    fn wt(path: &str, branch: &str) -> WtRaw {
        WtRaw { path: path.into(), branch: branch.into(), prunable: false, linked: path != MAIN, open_ws: None }
    }

    fn agent(status: &str) -> Agent {
        Agent { status: status.into(), name: "claude".into(), title: String::new() }
    }

    fn snap(agents: Vec<(&str, &str)>) -> Msg {
        Msg::Herdr(HerdrSnap {
            worktrees: vec![wt(MAIN, "main"), wt(FEAT, "feat")],
            agents: agents.into_iter().map(|(cwd, st)| (cwd.to_string(), agent(st))).collect(),
            error: None,
        })
    }

    fn row<'a>(a: &'a App, path: &str) -> &'a Row {
        a.rows.iter().find(|r| r.path == path).expect("row")
    }

    #[test]
    fn agent_in_nested_checkout_belongs_to_that_worktree_not_the_parent() {
        let mut a = app();
        a.apply(snap(vec![("/repo/.worktrees/feat/apps/api", "working"), ("/repo/apps/web", "idle")]));
        assert_eq!(row(&a, FEAT).top_status(), "working");
        assert_eq!(row(&a, MAIN).top_status(), "idle");
    }

    #[test]
    fn sibling_path_with_same_prefix_is_not_owned() {
        let mut a = app();
        a.apply(snap(vec![("/repo2", "working")]));
        assert!(a.rows.iter().all(|r| r.agents.is_empty()));
    }

    #[test]
    fn worktree_needing_you_sorts_above_failed_ci_and_busy_ones() {
        let mut a = app();
        a.apply(Msg::Pr(vec![PrInfo { b: "main".into(), n: 1, ci: "FAILURE".into(), ..Default::default() }], 1));
        a.apply(snap(vec![(FEAT, "blocked")]));
        assert_eq!(a.rows[0].path, FEAT, "blocked agent first");
        assert_eq!(a.rows[1].path, MAIN, "failed CI next");
    }

    #[test]
    fn pulse_starts_only_on_transition_into_blocked() {
        let mut a = app();
        a.apply(snap(vec![(FEAT, "working")]));
        assert!(!a.pulses.contains_key(FEAT));
        a.apply(snap(vec![(FEAT, "blocked")]));
        let first = *a.pulses.get(FEAT).expect("pulse on transition");
        a.apply(snap(vec![(FEAT, "blocked")]));
        assert_eq!(a.pulses[FEAT], first, "staying blocked must not restart the pulse");
    }

    #[test]
    fn selection_follows_path_across_resort() {
        let mut a = app();
        a.apply(snap(vec![]));
        a.selected = Some(MAIN.into());
        a.apply(snap(vec![(FEAT, "blocked")]));
        assert_eq!(a.rows[0].path, FEAT);
        assert_eq!(a.selected.as_deref(), Some(MAIN));
    }

    #[test]
    fn reordered_card_eases_from_old_slot_instead_of_jumping() {
        let mut a = app();
        assert_eq!(a.slide_y(FEAT, 8.0), 8.0, "first placement is immediate");
        let y = a.slide_y(FEAT, 0.0);
        assert!(y > 0.0 && y <= 8.0, "starts near the old slot, got {y}");
        std::thread::sleep(SLIDE);
        assert_eq!(a.slide_y(FEAT, 0.0), 0.0, "settles on the new slot");
    }

    #[test]
    fn fuzzy_is_case_insensitive_subsequence() {
        assert!(fuzzy("TrAd", "fix/tradingview-username-validation"));
        assert!(fuzzy("fxtv", "fix/tradingview"));
        assert!(!fuzzy("vt", "tv"));
    }

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b"/a/b"), "L2EvYg==");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(b"fo"), "Zm8=");
    }
}
