# Worktree Agents

A [herdr](https://herdr.dev) plugin that keeps a panel in the top-right corner listing every git
worktree of the current project, with the state of the agents running in each one, its PR and CI,
and its git status. Worktrees that need you sort to the top.

```text
  edgeful-app         vs github/main
 4 worktrees  ⠙1                ⟳ now
──────────────────────────────────────
╭────────────────────────────────────╮
│▌ ◆ main                        ✎4  │
│▌   ⠙ claude · Herdr plugin wor… 7h │
│▌   ● claude · New comment explana… │
│▌   ● codex · Edgeful-app simulate… │
│▌   no PR                           │
╰────────────────────────────────────╯
▌   chore/report-size-bounds… ✎2 ↑3↓54
▌   · no agent                    15d
▌   no PR

▌   fix/tradingview-username… ✎4 ↑5↓15
▌   ◉ codex · needs you · Review…  2d
▌    1309 ✓  5
──────────────────────────────────────
 ↵ open  ⇥ info  n new  d del
 c claude  o PR  y copy  / find
 q hide
```

## What each card shows

| Line | Content |
|---|---|
| First | `◆` this workspace's checkout, `◇` open in another workspace, `✗` prunable; branch; `✎` changed files, `↑`/`↓` commits ahead/behind the remote default branch |
| Agents | One line per agent running in that checkout, most urgent first: state glyph (`◉` needs you, braille spinner working, `✓` done, `●` idle), **which coding agent** (`claude`, `codex`, `pi`, `opencode`, … or the pane's reported display name), and its terminal title. Up to 3 lines, then `+N more`. The first line also shows the age of the last commit |
| Last | Open PR number, CI (`✓` / `✗N` failing checks / spinner running), unresolved review threads, `✔` approved or `±` changes requested, `draft` |

`Tab` expands the selected card with the last commit subject, a diffstat bar, failing check names and
the checkout path. The card's left bar and frame are red when an agent is waiting on you or CI failed,
yellow while working, green when done. A card pulses when its agent starts waiting on you, and cards
slide into place when the order changes.

Agents are matched to the deepest worktree containing their working directory, so an agent in
`.worktrees/feat` is not counted under the main checkout.

## Platforms

| OS | Status |
|---|---|
| Linux | Supported. Tested on Ubuntu with herdr 0.9.1 and Ghostty |
| macOS | Supported. The scripts avoid GNU-only tools, but this setup is less tested than Linux |
| Windows | Not supported. The hooks and actions are shell scripts, and herdr's Windows plugin support is still a preview |

The manifest declares `platforms = ["linux", "macos"]`, so on Windows herdr answers its actions and
panes with `platform_unsupported`.

## Requirements

- herdr 0.9.1 or newer
- `git`, `jq`
- `gh`, signed in, for PR and CI data (optional; without it the PR line reads `no PR`)
- Rust (`cargo`) for the full panel. Without it the plugin installs a simpler bash panel, which needs
  bash 4+ (macOS: `brew install bash`)
- A Nerd Font for the PR and comment icons, or set `WORKTREES_ICONS=plain`. Ghostty bundles the glyphs

## Install

From GitHub (runs `scripts/build.sh`, which builds the Rust panel when `cargo` is available):

```bash
herdr plugin install <owner>/<repo>
```

From a local checkout (`link` does not run build steps, so build first):

```bash
cargo build --release --manifest-path board/Cargo.toml
herdr plugin link "$PWD"
```

The panel opens by itself the next time you focus a workspace whose repo has two or more checkouts.

### Keybinding

Plugins cannot declare keys. Add one to `~/.config/herdr/config.toml`, then run
`herdr server reload-config`:

```toml
[[keys.command]]
key = "prefix+w"
type = "plugin_action"
command = "minhtran.worktrees.toggle"
description = "worktrees panel"
```

### Tab-bar summary and sidebar PR row (optional)

When the panel is hidden, a one-line summary such as `⎇ 3 ◉1 ✗1 ⠿2` can sit at the right of the tab
bar, and each worktree workspace can show `#1309 ✓ 💬5` in herdr's sidebar:

```bash
herdr plugin action invoke minhtran.worktrees.statusbar-install
```

This appends a marked block to `config.toml`, checks it with `herdr config check`, reloads, and puts
the old file back if herdr rejects it. A copy of the config from before the plugin first touched it is kept as `config.toml.bak-worktrees`. If your config
already has its own `[ui]` section the action stops and prints the snippet to merge by hand
(`herdr plugin log list --plugin minhtran.worktrees`). Note that the block sets
`[ui.sidebar.spaces] rows`, replacing a custom Space row layout.

## Using the panel

| Key | Action |
|---|---|
| `j` `k` / arrows / mouse wheel | Move |
| `Enter` / double-click | Open or focus the worktree |
| `Tab` / `Space` | Expand details |
| `n` | New worktree from the remote default branch (`herdr worktree create`) |
| `d` | Remove the checkout, after `y` to confirm (never the main checkout; never forced) |
| `c` | Open the worktree and start `claude` in a new split |
| `o` | Open the PR in the browser |
| `y` | Copy the checkout path (OSC 52) |
| `/` | Filter by branch (fuzzy); `Esc` clears |
| `r` | Refresh now |
| `q` | Hide the panel everywhere and stop auto-opening |

The `toggle` action hides the panel everywhere when the active tab shows it; otherwise it re-enables
auto-open and opens it in the active tab, even in a single-checkout repo.

## Configuration

Copy `config.env.example` into the plugin's config directory and edit it:

```bash
cp config.env.example "$(herdr plugin config-dir minhtran.worktrees)/config.env"
```

| Setting | Default | Meaning |
|---|---|---|
| `WORKTREES_WIDTH` | `38` | Panel width in columns |
| `WORKTREES_MIN` | `2` | Auto-open only in repos with at least this many checkouts |
| `WORKTREES_PR_TTL` | `60` | Seconds between PR/CI refreshes |
| `WORKTREES_GIT_EVERY` | `10` | Seconds between git scans |
| `WORKTREES_GH` | `/usr/bin/gh`, else `gh` | GitHub CLI to use |
| `WORKTREES_ICONS` | Nerd Font | `plain` for ASCII icons |

Changes apply the next time a panel opens (toggle twice).

## How it works

- `panel.sh` runs on the `startup`, `workspace.focused` and `worktree.created` hooks. It opens the
  `board` pane as a right split of the top-right pane and narrows it to `WORKTREES_WIDTH`. herdr has no
  API that lists plugin panes, so each board names its pane `⎇ worktrees` and `panel.sh` finds boards
  by that label.
- The board (`board/`, Rust + ratatui) polls three sources on separate threads: herdr (worktrees and
  agent panes, every 1.5s), git (every `WORKTREES_GIT_EVERY`) and GitHub (one GraphQL query for all
  open PRs, every `WORKTREES_PR_TTL`). The PR result is cached in the plugin state directory and shared
  by every panel on the repo, so several open panels still make one GitHub call per interval.
- `wt-board --status` is the one-shot command behind the tab-bar entry. It reads the same cache, prints
  nothing while the active tab shows the panel, and publishes `$wt_pr` workspace tokens.

State (the `disabled` flag, the PR cache, published sidebar tokens) lives in the plugin state
directory, `$HERDR_PLUGIN_STATE_DIR` (on Linux `~/.local/state/herdr/plugins/minhtran.worktrees`).
Nothing is written to the plugin checkout.

## Uninstall

```bash
herdr plugin action invoke minhtran.worktrees.statusbar-remove   # if you installed the summary
herdr plugin uninstall minhtran.worktrees
```

Remove the keybinding from `config.toml` if you added one.

## Troubleshooting

- Hook and action output: `herdr plugin log list --plugin minhtran.worktrees`
- The panel never opens: the repo needs `WORKTREES_MIN` checkouts, or you hid it with `q`. Run the
  toggle action to re-enable it.
- A panel still runs old code after an update: toggle twice. After `herdr plugin unlink` + `link`,
  herdr forgets which panes belong to the plugin; the toggle action still closes them.
- PR line always `no PR`: check `gh auth status`, or point `WORKTREES_GH` at a working `gh`.

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup, checks and guidelines. In short:

```bash
cd board
cargo test
cargo fmt --check
cargo clippy --all-targets
# Render one frame as text, without a pane:
HERDR_WORKSPACE_ID=<id> HERDR_PLUGIN_STATE_DIR="$HOME/.local/state/herdr/plugins/minhtran.worktrees" \
  cargo run --release -- --snapshot 38x30
```

A running board can be driven with `herdr pane send-keys <pane> j` and raw SGR mouse input through
`herdr pane send-text <pane> $'\e[<0;6;5M'`.

To list the plugin in the [herdr marketplace](https://herdr.dev/plugins/), push it to a public GitHub
repository and add the `herdr-plugin` topic.

## License

MIT. See [LICENSE](LICENSE).
