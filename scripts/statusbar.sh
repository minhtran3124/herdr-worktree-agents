#!/usr/bin/env bash
# Usage: statusbar.sh install | remove
#   install  add a managed block to herdr's config.toml: a tab-bar summary (`wt-board --status`)
#            and a `$wt_pr` row in the sidebar's Space rows; validate, reload, roll back on failure
#   remove   drop the block, clear the sidebar tokens this plugin published, reload
set -euo pipefail

H=${HERDR_BIN_PATH:-herdr}
ID=${HERDR_PLUGIN_ID:?run from herdr}
ROOT=${HERDR_PLUGIN_ROOT:?run from herdr}
STATE=${HERDR_PLUGIN_STATE_DIR:?run from herdr}
CONF=${HERDR_CONFIG:-$HOME/.config/herdr/config.toml}
BIN=$ROOT/board/target/release/wt-board
BEGIN="# >>> $ID"
END="# <<< $ID"

say() {
  echo "$1"
  "$H" notification show "Worktree Agents" --body "$1" >/dev/null 2>&1 || true
}

# Config minus our block (prefix match, so older marker lines with a trailing note still match).
without_block() {
  [ -f "$CONF" ] || return 0
  awk -v b="$BEGIN" -v e="$END" 'index($0, b) == 1 { skip = 1 } !skip { print } index($0, e) == 1 { skip = 0 }' "$CONF"
}

# Write $1 as the new config and restore the previous one if herdr rejects it.
# `.bak-worktrees` is written once, before the plugin's first edit, and never overwritten.
commit() {
  local next=$1 prev
  mkdir -p "$(dirname "$CONF")"
  touch "$CONF"
  prev=$(mktemp)
  cp "$CONF" "$prev"
  [ -f "$CONF.bak-worktrees" ] || cp "$prev" "$CONF.bak-worktrees"
  printf '%s\n' "$next" >"$CONF.tmp.$$" && mv "$CONF.tmp.$$" "$CONF"
  if ! out=$("$H" config check 2>&1); then
    cp "$prev" "$CONF"
    rm -f "$prev"
    say "config rejected, restored previous config.toml: ${out##*$'\n'}"
    exit 1
  fi
  rm -f "$prev"
  "$H" server reload-config >/dev/null 2>&1 || true
}

block() {
  cat <<EOF
$BEGIN (managed by the Worktree Agents plugin; remove with its statusbar-remove action)
[ui]
tab_bar_right = [
  { type = "command", command = "'$BIN' --status --state '$STATE'", interval_seconds = 5, timeout_seconds = 4 },
]

[ui.sidebar.spaces]
rows = [["state_icon", "workspace"], ["branch", "git_status"], [{ token = "\$wt_pr", fg = "#c678dd" }]]
$END
EOF
}

install() {
  if [ ! -x "$BIN" ]; then
    say "the tab-bar summary needs the Rust board: run 'cargo build --release' in $ROOT/board"
    exit 1
  fi
  local rest
  rest=$(without_block)
  # TOML forbids defining a table twice; merging into a user's own [ui] is left to them.
  if grep -Eq '^[[:space:]]*(\[ui\]|\[ui\.sidebar\.spaces\]|ui\.|tab_bar_right[[:space:]]*=)' <<<"$rest"; then
    say "config.toml already has its own [ui] settings; add the snippet from the plugin log by hand"
    block
    exit 1
  fi
  commit "${rest%$'\n'}"$'\n\n'"$(block)"
  say "tab-bar summary and sidebar PR row added"
}

remove() {
  local tokens=$STATE/tokens.json ws
  if [ -f "$tokens" ]; then
    for ws in $(jq -r 'keys[]' "$tokens" 2>/dev/null); do
      "$H" workspace report-metadata "$ws" --source "$ID" --clear-token wt_pr >/dev/null 2>&1 || true
    done
    rm -f "$tokens"
  fi
  if [ -f "$CONF" ] && grep -qF "$BEGIN" "$CONF"; then
    commit "$(without_block)"
  fi
  say "tab-bar summary and sidebar PR row removed"
}

case ${1:-} in
  install) install ;;
  remove) remove ;;
  *) echo "usage: statusbar.sh install|remove" >&2; exit 2 ;;
esac
