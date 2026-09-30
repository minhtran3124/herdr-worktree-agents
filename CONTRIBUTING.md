# Contributing

Thanks for helping. Bug reports, fixes and small focused features are all welcome.

## Reporting a bug

Include:

- `herdr --version`, your OS, and your terminal
- Which board is running: the Rust one (`board/target/release/wt-board` exists) or the bash fallback
- The plugin log: `herdr plugin log list --plugin minhtran.worktrees`
- What you expected, and what you saw (a screenshot of the panel helps)

## Project layout

```text
herdr-plugin.toml     manifest: pane, actions, startup and event hooks, build step
panel.sh              opens, hides and auto-opens the panel (hooks and the toggle action)
scripts/launch.sh     pane entrypoint: loads config.env, runs the Rust board or falls back to bash
scripts/build.sh      install-time build; must exit 0 when cargo is missing
scripts/statusbar.sh  adds or removes the tab-bar summary block in herdr's config.toml
board/                Rust board (ratatui)
  src/data.rs         background collectors: herdr, git, GitHub
  src/app.rs          state, row ranking, selection, animations, actions
  src/ui.rs           rendering
  src/main.rs         event loop, keys, mouse, --status and --snapshot modes
board.sh              bash fallback board (fewer features, needs bash 4+)
```

## Development setup

You need herdr 0.9.1+, Rust, `git`, `jq`, and optionally `gh` for PR data.

```bash
cargo build --release --manifest-path board/Cargo.toml
herdr plugin link "$PWD"
```

`herdr plugin link` does not run the build step, so rebuild yourself after Rust changes, then toggle
the panel twice to restart it.

- Editor settings come from `.editorconfig`; Rust formatting from `board/rustfmt.toml` (`cargo fmt`).
- Script changes (`panel.sh`, `scripts/*`) apply on the next hook or action run.
- Manifest changes need `herdr plugin unlink minhtran.worktrees` and `herdr plugin link "$PWD"`.
  Relinking makes herdr forget which open panes belong to the plugin; the toggle action still
  closes them.

## Checks before a pull request

```bash
cd board
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
bash -n ../panel.sh ../board.sh ../scripts/statusbar.sh && sh -n ../scripts/build.sh ../scripts/launch.sh
```

Report the test count, including anything skipped.

To check layout without a pane, render one frame as text:

```bash
HERDR_WORKSPACE_ID=<id> HERDR_PLUGIN_STATE_DIR="$HOME/.local/state/herdr/plugins/minhtran.worktrees" \
  cargo run --release -- --snapshot 38x30
```

To drive a running board, find its pane (`herdr pane list`, label `⎇ worktrees`) and send input:

```bash
herdr pane send-keys <pane> j tab
herdr pane send-text <pane> $'\e[<0;6;5M'   # left click at column 6, row 5
herdr pane read <pane>
```

Avoid sending `Enter`, `n`, `d`, `c`, `o` or `y` while testing unless you mean it: they open or remove
worktrees, start agents, open the browser or overwrite the clipboard.

## Guidelines

- **Keep the panel calm.** Text only, ANSI palette colors (herdr users often run
  `theme = "terminal"`), herdr's own status glyphs. Images, charts and extra rows were tried and removed
  as too noisy; propose visual additions in an issue first.
- **Tests encode the rule, not the shape.** For example: "an agent in `.worktrees/feat` belongs to
  that worktree, not the main checkout", or "the pulse starts only when an agent starts waiting".
  If you change a rule and no test fails, add one.
- **Never block the UI thread.** Anything that touches the network or runs `git` belongs in a worker
  in `data.rs`; user actions run on a background thread and report back through `Msg::Status`.
- **Linux and macOS only.** Windows is out of scope: supporting it means moving the shell scripts
  into the Rust binary and shipping prebuilt binaries. Open an issue before starting on that.
- **Portable shell.** Scripts run on Linux and macOS: no GNU-only flags (`stat -c`, `sed -i` without a
  suffix argument, `readlink -f`), and no hard dependency on `flock` or `timeout`.
- **Don't break installs.** `scripts/build.sh` must exit 0 without cargo, and the bash board must keep
  working as the fallback.
- **Touch the user's config.toml only through `scripts/statusbar.sh`.** It validates, rolls back, and
  never merges into a `[ui]` table it did not write.
- Plugin id `minhtran.worktrees` appears in the manifest and in `PLUGIN_ID` in `board/src/main.rs`;
  change both together.

## Commits and changelog

- Conventional commit prefixes: `feat:`, `fix:`, `refactor:`, `docs:`, `test:`, `chore:`.
- Add user-visible changes to `CHANGELOG.md` under `[Unreleased]`.
- Releases bump `version` in both `herdr-plugin.toml` and `board/Cargo.toml`.

## License

By contributing you agree that your contributions are licensed under the [MIT License](LICENSE).
