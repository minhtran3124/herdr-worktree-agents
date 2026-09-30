#!/usr/bin/env bash
# Usage: panel.sh toggle | ensure | off
#   toggle  hide everywhere when the active tab shows the panel, else re-enable and open it here
#   ensure  (startup / workspace.focused / worktree.created hooks) auto-open in the active
#           tab when the workspace repo has >= WORKTREES_MIN checkouts and the panel is missing
#   off     hide everywhere (the board's `q` key)
set -euo pipefail

cfg="${HERDR_PLUGIN_CONFIG_DIR:-}/config.env"
if [ -f "$cfg" ]; then set -a; . "$cfg"; set +a; fi

H=${HERDR_BIN_PATH:-herdr}
PLUGIN=${HERDR_PLUGIN_ID:-minhtran.worktrees}
STATE=${HERDR_PLUGIN_STATE_DIR:?run from herdr}
OFF=$STATE/disabled
LABEL="⎇ worktrees"
WIDTH=${WORKTREES_WIDTH:-38}
MIN=${WORKTREES_MIN:-2}

# Herdr has no "list plugin panes" API, so the board labels its own pane.
board_panes() {
  "$H" pane list | jq -r --arg l "$LABEL" '.result.panes[] | select(.label == $l) | "\(.tab_id) \(.pane_id)"'
}

# "<workspace_id> <active_tab_id>" of the focused workspace.
active() {
  "$H" workspace list | jq -r '.result.workspaces[] | select(.focused) | "\(.workspace_id) \(.active_tab_id)"'
}

# Non-git workspaces make worktree list fail; single-checkout repos are not worth a panel.
has_worktrees() {
  local n
  n=$("$H" worktree list --workspace "$1" 2>/dev/null | jq '.result.worktrees | length') || return 1
  (( n >= MIN ))
}

open_in_tab() {
  local tab=$1 anchor layout target new_id
  anchor=$("$H" pane list | jq -r --arg t "$tab" '[.result.panes[] | select(.tab_id == $t)][0].pane_id // empty')
  [ -n "$anchor" ] || return 0
  layout=$("$H" pane layout --pane "$anchor")
  # Top-right pane: furthest right edge, then smallest y.
  target=$(jq -r '.result.layout.panes | max_by([.rect.x + .rect.width, -.rect.y]) | .pane_id' <<<"$layout")
  new_id=$("$H" plugin pane open --plugin "$PLUGIN" --entrypoint board --placement split \
    --target-pane "$target" --direction right --no-focus | jq -r '.result.plugin_pane.pane.pane_id')
  shrink "$new_id"
}

# A fresh split is 50/50; pull the board's left edge right until it is ~WIDTH columns.
# Resize amounts are fractions of the parent split (the narrowest right-split holding the board).
shrink() {
  local id=$1 w parent amount
  read -r w parent < <("$H" pane layout --pane "$id" | jq -r --arg id "$id" '.result.layout
    | (.panes[] | select(.pane_id == $id) | .rect) as $r
    | [.splits[] | select(.direction == "right" and .rect.x <= $r.x and .rect.x + .rect.width >= $r.x + $r.width
        and .rect.y <= $r.y and .rect.y + .rect.height >= $r.y + $r.height) | .rect.width] | min as $p
    | "\($r.width) \($p)"')
  (( w > WIDTH )) || return 0
  amount=$(awk -v w="$w" -v t="$WIDTH" -v p="$parent" 'BEGIN { printf "%.3f", (w - t) / p }')
  "$H" pane resize --pane "$id" --direction right --amount "$amount" >/dev/null || true
}

close_all() {
  # Re-linking the plugin drops herdr's ownership record, so fall back to a plain pane close.
  board_panes | while read -r _ id; do
    "$H" plugin pane close "$id" >/dev/null 2>&1 || "$H" pane close "$id" >/dev/null 2>&1 || true
  done
}

# $1 = force: skip the worktree-count check (explicit toggle on).
ensure() {
  local force=${1:-} ws tab
  [ -f "$OFF" ] && return 0
  read -r ws tab < <(active) || return 0
  [ -n "$tab" ] || return 0
  board_panes | awk '{print $1}' | grep -qxF "$tab" && return 0
  [ -n "$force" ] || has_worktrees "$ws" || return 0
  open_in_tab "$tab"
}

# Rapid workspace switches fire overlapping hooks; serialize them. macOS has no flock(1):
# mkdir is atomic there, and a stuck lock is ignored after ~5s rather than hanging a hook.
if command -v flock >/dev/null 2>&1; then
  exec 9>"$STATE/lock"
  flock 9
else
  for _ in $(seq 50); do mkdir "$STATE/lock.d" 2>/dev/null && break; sleep 0.1; done
  trap 'rmdir "$STATE/lock.d" 2>/dev/null' EXIT
fi

case ${1:-} in
  toggle)
    read -r _ tab < <(active) || true
    if board_panes | awk '{print $1}' | grep -qxF "${tab:-}"; then touch "$OFF"; close_all
    else rm -f "$OFF"; ensure force; fi ;;
  ensure) ensure ;;
  off) touch "$OFF"; close_all ;;
  *) echo "usage: panel.sh toggle|ensure|off" >&2; exit 2 ;;
esac
