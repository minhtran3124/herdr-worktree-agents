//! wt-board: the herdr worktree panel. Run by the plugin's `board` pane entrypoint.
//! `wt-board --snapshot 38x40` renders one frame as text (for checking layout without a pane).

mod app;
mod data;
mod ui;

use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::crossterm::{event::DisableMouseCapture, event::EnableMouseCapture, execute};
use ratatui::layout::Position;
use ratatui::Terminal;

use app::{App, Mode};
use data::Msg;

/// panel.sh finds boards by this pane label (herdr has no "list plugin panes" API).
pub const LABEL: &str = "⎇ worktrees";

/// Must match `id` in herdr-plugin.toml: `--status` runs from herdr's config, outside plugin env.
const PLUGIN_ID: &str = "minhtran.worktrees";

fn env(k: &str) -> String {
    std::env::var(k).unwrap_or_default()
}

fn setup() -> (App, Receiver<Msg>) {
    let ws = env("HERDR_WORKSPACE_ID");
    // herdr links a workspace to a worktree only when herdr opened it, so also match by path.
    let here = serde_json::from_str::<serde_json::Value>(&env("HERDR_PLUGIN_CONTEXT_JSON"))
        .ok()
        .and_then(|c| c["workspace_cwd"].as_str().map(String::from))
        .and_then(|cwd| data::run("git", &["-C", &cwd, "rev-parse", "--show-toplevel"]))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let src = data::herdr_json(&["worktree", "list", "--workspace", &ws]);
    let field = |k: &str| src.as_ref().and_then(|v| v["result"]["source"][k].as_str()).unwrap_or_default().to_string();
    let (repo, root) = (field("repo_name"), field("repo_root"));
    let remotes = data::run("git", &["-C", &root, "remote"]).unwrap_or_default();
    let base = remotes
        .lines()
        .find_map(|r| {
            data::run("git", &["-C", &root, "symbolic-ref", "-q", "--short", &format!("refs/remotes/{r}/HEAD")])
        })
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "main".into());
    let url = base
        .split_once('/')
        .and_then(|(remote, _)| data::run("git", &["-C", &root, "remote", "get-url", remote]))
        .unwrap_or_default();
    let slug = github_slug(url.trim());

    let (tx, rx) = mpsc::channel();
    let paths = Arc::new(Mutex::new(Vec::new()));
    let mut kicks = Vec::new();
    let mut kick = || {
        let (k, r) = mpsc::channel();
        kicks.push(k);
        r
    };
    data::spawn_herdr(ws.clone(), tx.clone(), kick());
    data::spawn_git(paths.clone(), base.clone(), tx.clone(), kick());
    if let Some(slug) = slug {
        let state = PathBuf::from(env("HERDR_PLUGIN_STATE_DIR"));
        let cache = state.join(format!("prs-{}.json", slug.replace('/', "_")));
        let gh = std::env::var("WORKTREES_GH").ok().unwrap_or_else(|| {
            // Bare `gh` may resolve to a pyenv shim that rejects --jq.
            if std::path::Path::new("/usr/bin/gh").exists() {
                "/usr/bin/gh".into()
            } else {
                "gh".into()
            }
        });
        data::spawn_pr(slug, cache, gh, tx.clone(), kick());
    }
    let app = App::new(ws, here, repo, root, base, env("HERDR_PLUGIN_ROOT"), paths, tx, kicks);
    (app, rx)
}

fn github_slug(url: &str) -> Option<String> {
    let rest = url.split_once("github.com")?.1.trim_start_matches([':', '/']);
    let slug = rest.trim_end_matches('/').trim_end_matches(".git");
    (slug.split('/').count() == 2).then(|| slug.to_string())
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if let Some(size) = args.iter().position(|a| a == "--snapshot").and_then(|i| args.get(i + 1)) {
        return snapshot(size);
    }
    if args.iter().any(|a| a == "--status") {
        let state = args.iter().position(|a| a == "--state").and_then(|i| args.get(i + 1)).cloned();
        return status(state);
    }
    let (mut app, rx) = setup();
    if !env("HERDR_PANE_ID").is_empty() {
        let _ = data::run(&data::herdr_bin(), &["pane", "rename", &env("HERDR_PANE_ID"), LABEL]);
    }

    let mut term = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let result = run(&mut term, &mut app, rx);
    execute!(std::io::stdout(), DisableMouseCapture)?;
    ratatui::restore();
    result
}

fn run(term: &mut ratatui::DefaultTerminal, app: &mut App, rx: Receiver<Msg>) -> std::io::Result<()> {
    while !app.quit {
        while let Ok(m) = rx.try_recv() {
            app.apply(m);
        }
        // Synchronized output: the terminal swaps in the whole frame at once, so no tearing.
        execute!(std::io::stdout(), BeginSynchronizedUpdate)?;
        term.draw(|f| ui::render(f, app))?;
        execute!(std::io::stdout(), EndSynchronizedUpdate)?;

        let spinning = app.rows.iter().any(|r| {
            r.top_status() == "working" || r.pr.as_ref().is_some_and(|p| p.ci == "PENDING" || p.ci == "EXPECTED")
        });
        let wait = if app.animating() || spinning { 40 } else { 250 };
        if event::poll(Duration::from_millis(wait))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => key(app, k),
                Event::Mouse(m) => mouse(app, m),
                _ => {}
            }
        }
    }
    Ok(())
}

fn key(app: &mut App, k: KeyEvent) {
    if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    match std::mem::replace(&mut app.mode, Mode::Normal) {
        Mode::Filter => match k.code {
            KeyCode::Esc => {
                app.filter.clear();
                app.rebuild();
            }
            KeyCode::Enter => {}
            KeyCode::Backspace => {
                app.filter.pop();
                app.rebuild();
                app.mode = Mode::Filter;
            }
            KeyCode::Char(c) => {
                app.filter.push(c);
                app.rebuild();
                app.mode = Mode::Filter;
            }
            _ => app.mode = Mode::Filter,
        },
        Mode::NewBranch(mut s) => match k.code {
            KeyCode::Esc => {}
            KeyCode::Enter if !s.trim().is_empty() => app.create(s.trim().to_string()),
            KeyCode::Backspace => {
                s.pop();
                app.mode = Mode::NewBranch(s);
            }
            KeyCode::Char(c) if !c.is_whitespace() => {
                s.push(c);
                app.mode = Mode::NewBranch(s);
            }
            _ => app.mode = Mode::NewBranch(s),
        },
        Mode::ConfirmDelete(path) => {
            if k.code == KeyCode::Char('y') {
                app.delete(path);
            }
        }
        Mode::Normal => match k.code {
            KeyCode::Char('j') | KeyCode::Down => app.move_sel(1),
            KeyCode::Char('k') | KeyCode::Up => app.move_sel(-1),
            KeyCode::Enter => app.open_selected(),
            KeyCode::Tab | KeyCode::Char(' ') => app.expanded = !app.expanded,
            KeyCode::Char('r') => {
                app.kick();
                app.flash("refreshing…", true);
            }
            KeyCode::Char('/') => app.mode = Mode::Filter,
            KeyCode::Esc => {
                app.filter.clear();
                app.rebuild();
            }
            KeyCode::Char('n') => app.mode = Mode::NewBranch(String::new()),
            KeyCode::Char('d') => match app.selected_row() {
                Some(r) if r.linked => app.mode = Mode::ConfirmDelete(r.path.clone()),
                Some(_) => app.flash("main checkout can't be removed", false),
                None => {}
            },
            KeyCode::Char('c') => app.claude(),
            KeyCode::Char('o') => app.open_pr(),
            KeyCode::Char('y') => app.yank(),
            KeyCode::Char('q') => app.hide(),
            _ => {}
        },
    }
}

fn mouse(app: &mut App, m: MouseEvent) {
    let at = Position::new(m.column, m.row);
    let hit = app.hits.iter().find(|(r, _)| r.contains(at)).map(|(_, p)| p.clone());
    match m.kind {
        MouseEventKind::Down(event::MouseButton::Left) => {
            let Some(path) = hit else { return };
            let double =
                app.last_click.as_ref().is_some_and(|(p, t)| *p == path && t.elapsed() < Duration::from_millis(400));
            app.selected = Some(path.clone());
            if double {
                app.open_selected();
                app.last_click = None;
            } else {
                app.last_click = Some((path, Instant::now()));
            }
        }
        MouseEventKind::Moved => app.hover = hit,
        MouseEventKind::ScrollDown => app.move_sel(1),
        MouseEventKind::ScrollUp => app.move_sel(-1),
        _ => {}
    }
}

/// One-shot summary for herdr's `tab_bar_right` command entry (runs every few seconds on the
/// server). Prints nothing while the panel is visible in the active tab, so the entry disappears.
/// Also publishes `$wt_pr` workspace tokens for the sidebar's Space rows.
fn status(state: Option<String>) -> std::io::Result<()> {
    let ws = env("HERDR_ACTIVE_WORKSPACE_ID");
    let tab = env("HERDR_ACTIVE_TAB_ID");
    let state = PathBuf::from(state.unwrap_or_else(|| env("HERDR_PLUGIN_STATE_DIR")));
    let Some(src) = data::herdr_json(&["worktree", "list", "--workspace", &ws]) else { return Ok(()) };
    let root = src["result"]["source"]["repo_root"].as_str().unwrap_or_default().to_string();
    let url = data::run("git", &["-C", &root, "remote", "get-url", "origin"])
        .or_else(|| {
            let first = data::run("git", &["-C", &root, "remote"])?;
            data::run("git", &["-C", &root, "remote", "get-url", first.lines().next()?])
        })
        .unwrap_or_default();
    let prs: Vec<data::PrInfo> = github_slug(url.trim())
        .and_then(|slug| std::fs::read_to_string(state.join(format!("prs-{}.json", slug.replace('/', "_")))).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();

    let (tx, _rx) = mpsc::channel();
    let kicks = (0..3).map(|_| mpsc::channel().0).collect();
    let mut app = App::new(
        ws,
        String::new(),
        String::new(),
        root,
        String::new(),
        String::new(),
        Arc::new(Mutex::new(Vec::new())),
        tx,
        kicks,
    );
    app.apply(Msg::Pr(prs, 0));
    app.apply(Msg::Herdr(data::herdr_snapshot(&app.ws)));
    publish_tokens(&app, &state);

    let panel_here = data::herdr_json(&["pane", "list"])
        .and_then(|p| p["result"]["panes"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .any(|p| p["tab_id"] == tab.as_str() && p["label"] == LABEL);
    if panel_here || app.rows.len() < 2 {
        return Ok(());
    }
    let count = |pred: &dyn Fn(&app::Row) -> bool| app.rows.iter().filter(|r| pred(r)).count();
    let mut line = format!("⎇ {}", app.rows.len());
    for (n, glyph) in [
        (count(&|r| r.top_status() == "blocked"), "◉"),
        (count(&|r| r.ci_failed()), "✗"),
        (count(&|r| r.top_status() == "done"), "✓"),
        (count(&|r| r.top_status() == "working"), "⠿"),
    ] {
        if n > 0 {
            line.push_str(&format!(" {glyph}{n}"));
        }
    }
    println!("{line}");
    Ok(())
}

/// `$wt_pr` per open worktree workspace, e.g. "#1309 ✓ 💬5". Only changed values are sent;
/// the last sent set lives in the state dir so a 5s status cadence stays one read per tick.
fn publish_tokens(app: &App, state: &std::path::Path) {
    let file = state.join("tokens.json");
    let mut sent: std::collections::HashMap<String, String> =
        std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let mut changed = false;
    for r in &app.rows {
        let Some(ws) = &r.open_ws else { continue };
        let value = r.pr.as_ref().map(|p| {
            let ci = match p.ci.as_str() {
                "SUCCESS" => "✓",
                "FAILURE" | "ERROR" => "✗",
                "PENDING" | "EXPECTED" => "…",
                _ => "",
            };
            let threads = if p.t > 0 { format!(" 💬{}", p.t) } else { String::new() };
            format!("#{} {ci}{threads}", p.n).trim_end().to_string()
        });
        if sent.get(ws) == value.as_ref() {
            continue;
        }
        let bin = data::herdr_bin();
        let src = ["--source", PLUGIN_ID];
        let ok = match &value {
            Some(v) => data::run(
                &bin,
                &[&["workspace", "report-metadata", ws], &src[..], &["--token", &format!("wt_pr={v}")]].concat(),
            ),
            None => data::run(
                &bin,
                &[&["workspace", "report-metadata", ws], &src[..], &["--clear-token", "wt_pr"]].concat(),
            ),
        };
        if ok.is_some() {
            match value {
                Some(v) => sent.insert(ws.clone(), v),
                None => sent.remove(ws),
            };
            changed = true;
        }
    }
    if changed {
        let _ = std::fs::write(&file, serde_json::to_string(&sent).unwrap_or_default());
    }
}

/// Render one frame after the first data from every source arrives (or 25s passes).
fn snapshot(size: &str) -> std::io::Result<()> {
    let (w, h) = size.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))).unwrap_or((38, 40));
    let (mut app, rx) = setup();
    let want_pr = !env("HERDR_PLUGIN_STATE_DIR").is_empty();
    let (mut herdr, mut git, mut pr) = (false, false, !want_pr);
    let deadline = Instant::now() + Duration::from_secs(25);
    while !(herdr && git && pr) && Instant::now() < deadline {
        if let Ok(m) = rx.recv_timeout(Duration::from_millis(200)) {
            match &m {
                Msg::Herdr(_) => herdr = true,
                Msg::Git(g) => git = herdr && !g.is_empty(),
                Msg::Pr(_, _) => pr = true,
                Msg::Status(..) => {}
            }
            app.apply(m);
        }
    }
    app.expanded = std::env::var("WT_EXPANDED").is_ok();
    // TestBackend is infallible.
    let Ok(mut term) = Terminal::new(TestBackend::new(w, h));
    for _ in 0..2 {
        let Ok(_) = term.draw(|f| ui::render(f, &mut app));
    }
    let buf = term.backend().buffer().clone();
    let mut out = std::io::stdout().lock();
    for y in 0..h {
        let line: String = (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect();
        writeln!(out, "{}", line.trim_end())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::github_slug;

    #[test]
    fn slug_from_ssh_and_https_remotes() {
        assert_eq!(github_slug("git@github.com:getedgeful/edgeful-app.git").as_deref(), Some("getedgeful/edgeful-app"));
        assert_eq!(github_slug("https://github.com/o/r").as_deref(), Some("o/r"));
        assert_eq!(github_slug("git@gitlab.com:o/r.git"), None);
    }
}
