#!/usr/bin/env bash
# Worktree board: every git worktree of this workspace's repo, with live agent state,
# PR/CI/unresolved threads and ahead/behind. Rows needing you sort to the top.
# Keys: j/k or arrows move, Enter opens/focuses, r refreshes, q hides the panel everywhere.
# Fallback for machines without the Rust board. Needs bash 4+ (macOS: `brew install bash`).
set -uo pipefail

H=${HERDR_BIN_PATH:-herdr}
WS=${HERDR_WORKSPACE_ID:?run from herdr}
STATE=${HERDR_PLUGIN_STATE_DIR:?run from herdr}
LABEL="⎇ worktrees"
GH=${WORKTREES_GH:-/usr/bin/gh}   # bare `gh` may be a pyenv shim that rejects --jq
[ -x "$GH" ] || GH=gh
PR_TTL=${WORKTREES_PR_TTL:-60}
TIMEOUT=""   # macOS has no timeout(1) by default
command -v timeout >/dev/null 2>&1 && TIMEOUT="timeout 20"
GIT_EVERY=${WORKTREES_GIT_EVERY:-10}

"$H" pane rename "$HERDR_PANE_ID" "$LABEL" >/dev/null 2>&1 || true

# herdr only links a workspace to a worktree when herdr opened it, so also match by checkout path.
HERE=$(jq -r '.workspace_cwd // empty' <<<"${HERDR_PLUGIN_CONTEXT_JSON:-null}" 2>/dev/null)
[ -n "$HERE" ] && HERE=$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)

R=$'\e[0m' DIM=$'\e[2m' B=$'\e[1m' RED=$'\e[31m' GRN=$'\e[32m' YEL=$'\e[33m' MAG=$'\e[35m' CYN=$'\e[36m'
SPIN=(◐ ◓ ◑ ◒)
US=$'\x1f'   # field separator: unlike tab, IFS never collapses empty fields around it

repo="" root="" base="" slug="" cache=/dev/null
rows=()          # path, branch, state, agent_status, agent, agent_count, pr, ci, threads (US-separated)
declare -A GIT   # path -> "dirty ahead behind commit_epoch"
sel_path=""
tick=0

# Resolve repo, compare base (<remote>/HEAD) and GitHub slug once.
init_repo() {
  local json r url
  json=$("$H" worktree list --workspace "$WS" 2>/dev/null) || return 1
  repo=$(jq -r '.result.source.repo_name' <<<"$json")
  root=$(jq -r '.result.source.repo_root' <<<"$json")
  for r in $(git -C "$root" remote 2>/dev/null); do
    base=$(git -C "$root" symbolic-ref -q --short "refs/remotes/$r/HEAD") && break
  done
  [ -n "$base" ] || base=main
  url=$(git -C "$root" remote get-url "${base%%/*}" 2>/dev/null || true)
  [[ $url =~ github\.com[:/]([^/]+/[^/]+)$ ]] && slug=${BASH_REMATCH[1]%.git}
  [ -n "$slug" ] && cache=$STATE/prs-${slug//\//_}.json
}

# One GraphQL call covers PR number, CI rollup and unresolved threads for every open PR.
# The cache is shared by every board on this repo; touching it claims the refresh slot.
pr_fetch() {
  [ -n "$slug" ] || return 0
  # `date -r FILE` reads the mtime on both GNU and BSD date.
  if [ -f "$cache" ] && (( $(date +%s) - $(date -r "$cache" +%s) < PR_TTL )); then return 0; fi
  touch "$cache"
  (
    out=$($TIMEOUT "$GH" api graphql -F owner="${slug%%/*}" -F name="${slug#*/}" -f query='
      query($owner:String!,$name:String!){repository(owner:$owner,name:$name){
        pullRequests(states:OPEN,first:100,orderBy:{field:UPDATED_AT,direction:DESC}){nodes{
          number headRefName
          reviewThreads(first:100){nodes{isResolved}}
          commits(last:1){nodes{commit{statusCheckRollup{state}}}}}}}}' \
      --jq '[.data.repository.pullRequests.nodes[] | {b: .headRefName, n: .number,
             ci: (.commits.nodes[0].commit.statusCheckRollup.state // ""),
             t: ([.reviewThreads.nodes[] | select(.isResolved | not)] | length)}]') &&
      printf '%s' "$out" >"$cache.tmp.$$" && mv "$cache.tmp.$$" "$cache"
  ) >/dev/null 2>&1 &
}

git_scan() {
  local r p dirty behind ahead ct
  GIT=()
  for r in "${rows[@]}"; do
    p=${r%%$US*}
    [ -d "$p" ] || continue
    dirty=$(git -C "$p" status --porcelain 2>/dev/null | wc -l)
    read -r behind ahead < <(git -C "$p" rev-list --left-right --count "$base...HEAD" 2>/dev/null || echo "0 0")
    ct=$(git -C "$p" log -1 --format=%ct 2>/dev/null || echo 0)
    GIT[$p]="$dirty ${ahead:-0} ${behind:-0} ${ct:-0}"
  done
}

# Join worktrees + agent panes + PR cache; rank blocked > CI fail > done > working > rest.
load() {
  local wt panes prs
  wt=$("$H" worktree list --workspace "$WS" 2>/dev/null) || { rows=(); return; }
  panes=$("$H" pane list 2>/dev/null) || panes='{"result":{"panes":[]}}'
  prs=$(jq -c . "$cache" 2>/dev/null) || prs='[]'
  [ -n "$prs" ] || prs='[]'
  mapfile -t rows < <(jq -rn --argjson wt "$wt" --argjson panes "$panes" --argjson prs "$prs" \
    --arg ws "$WS" --arg here "$HERE" '
    def urg: {"blocked":0,"working":1,"done":2,"idle":3}[.] // 4;
    ($wt.result.worktrees | map(.path)) as $paths
    | [ $panes.result.panes[] | select(.agent != null)
        | (.foreground_cwd // .cwd // "") as $c
        | {owner: ([ $paths[] | select($c == . or ($c | startswith(. + "/"))) ] | max_by(length)),
           s: (.agent_status // "unknown"), a: .agent}
        | select(.owner != null) ] as $agents
    | [ $wt.result.worktrees[] | . as $w
        | ([ $agents[] | select(.owner == $w.path) ] | sort_by(.s | urg)) as $mine
        | (if $w.is_detached then null else ([ $prs[] | select(.b == $w.branch) ][0]) end) as $pr
        | {path: .path,
           branch: (if .is_detached then "(detached)" else .branch end),
           state: (if .is_prunable then "prunable"
                   elif .open_workspace_id == $ws or .path == $here then "here"
                   elif .open_workspace_id then "open" else "closed" end),
           as: ($mine[0].s // ""), a: ($mine[0].a // ""), an: ($mine | length),
           pr: ($pr.n // ""), ci: ($pr.ci // ""), t: ($pr.t // 0)}
        | .rank = (if .as == "blocked" then 0
                   elif (.ci == "FAILURE" or .ci == "ERROR") then 1
                   elif .as == "done" then 2 elif .as == "working" then 3 else 4 end) ]
    | sort_by(.rank)[]
    | [.path, .branch, .state, .as, .a, .an, .pr, .ci, .t] | map(tostring) | join("\u001f")')
}

age() {
  local s=$(( $(date +%s) - $1 ))
  if (( $1 == 0 )); then echo ""; elif (( s < 60 )); then echo now
  elif (( s < 3600 )); then echo "$(( s / 60 ))m"; elif (( s < 86400 )); then echo "$(( s / 3600 ))h"
  else echo "$(( s / 86400 ))d"; fi
}

# line <left> <left-width> <right> <right-width>: widths are visible columns, not bytes.
line() {
  local pad=$(( COLS - $2 - $4 ))
  (( pad < 1 )) && pad=1
  buf+="$1${R}$(printf '%*s' "$pad" '')$3${R}"$'\e[K\n'
}

render() {
  local i path branch state as a an pr ci t dirty ahead behind ct
  local sel bar mark right rw name txt color rtxt ciicon rule
  COLS=$(tput cols)
  rule="${DIM}$(printf '─%.0s' $(seq 1 "$COLS"))${R}"$'\e[K\n'
  buf=$'\e[H'
  rtxt="vs ${base}  ${#rows[@]} wt "
  line " ${B}${CYN}${R} ${B}${repo}" $(( 3 + ${#repo} )) "${DIM}${rtxt}" "${#rtxt}"
  buf+=$rule
  for i in "${!rows[@]}"; do
    IFS=$'\x1f' read -r path branch state as a an pr ci t <<<"${rows[$i]}"
    read -r dirty ahead behind ct <<<"${GIT[$path]:-0 0 0 0}"
    sel=""; [ "$path" = "$sel_path" ] && sel=1
    bar=" "; [ -n "$sel" ] && bar="${CYN}▌${R}"
    case $state in
      here) mark="${GRN}●" ;; open) mark="${CYN}○" ;; prunable) mark="${RED}✗" ;; *) mark=" " ;;
    esac

    # Line 1: marker, branch, dirty + ahead/behind.
    right="" rtxt=""
    (( dirty > 0 )) && { right+="${YEL}✎${dirty} "; rtxt+="✎${dirty} "; }
    (( ahead > 0 )) && { right+="${GRN}↑${ahead}"; rtxt+="↑${ahead}"; }
    (( behind > 0 )) && { right+="${RED}↓${behind}"; rtxt+="↓${behind}"; }
    right+=" " rtxt+=" "
    rw=${#rtxt}
    name=$branch
    (( ${#name} > COLS - 4 - rw )) && name="${name:0:COLS-5-rw}…"
    line "${bar}${mark}${R} ${sel:+$B}${name}" $(( 3 + ${#name} )) "$right" "$rw"

    # Line 2: agent state + last commit age.
    a=${a:-agent}
    case $as in
      blocked) color=$RED$B txt="! $a waiting on you" ;;
      working) color=$YEL txt="${SPIN[tick % 4]} $a working" ;;
      done) color=$GRN txt="✓ $a done" ;;
      idle) color=$DIM txt="· $a idle" ;;
      "") color=$DIM txt="· no agent" ;;
      *) color=$DIM txt="· $a" ;;
    esac
    (( an > 1 )) && txt+=" +$(( an - 1 ))"
    ct=$(age "$ct")
    line "${bar}  ${color}${txt}" $(( 3 + ${#txt} )) "${DIM}${ct} " $(( ${#ct} + 1 ))

    # Line 3: PR, CI, unresolved review threads.
    if [ -n "$pr" ]; then
      case $ci in
        SUCCESS) ciicon="${GRN}✓ CI" ;;
        FAILURE|ERROR) ciicon="${RED}✗ CI" ;;
        PENDING|EXPECTED) ciicon="${YEL}${SPIN[tick % 4]} CI" ;;
        *) ciicon="${DIM}– CI" ;;
      esac
      txt="${MAG}#${pr}${R} ${ciicon}${R}"
      (( t > 0 )) && txt+=" ${DIM}·${R} ${YEL}💬${t}"
      buf+="${bar}  ${txt}${R}"$'\e[K\n'
    fi
    buf+="${bar}"$'\e[K\n'
  done
  buf+=$rule
  buf+=" ${DIM}↵ open  r refresh  q hide${R}"$'\e[K\e[J'
  printf '%s' "$buf"
}

# Selection follows the worktree path, so re-sorting never jumps it to another row.
move() {
  local i n=${#rows[@]} cur=0
  (( n > 0 )) || return 0
  for i in "${!rows[@]}"; do [ "${rows[$i]%%$US*}" = "$sel_path" ] && cur=$i; done
  (( cur += $1 ))
  (( cur < 0 )) && cur=0
  (( cur >= n )) && cur=$(( n - 1 ))
  sel_path=${rows[$cur]%%$US*}
}

activate() {
  [ -n "$sel_path" ] || return
  "$H" worktree open --workspace "$WS" --path "$sel_path" --focus >/dev/null 2>&1
}

cleanup() { printf '\e[?25h'; stty echo 2>/dev/null; kill $(jobs -p) 2>/dev/null; }
trap cleanup EXIT
trap render WINCH
printf '\e[?25l\e[2J'
stty -echo 2>/dev/null

init_repo || repo="(not a git workspace)"
pr_fetch; load; move 0; git_scan; render
while true; do
  if IFS= read -rsn1 -t 1 key; then
    if [ "$key" = $'\e' ]; then read -rsn2 -t 0.01 rest; key+=$rest; fi
    case $key in
      k|$'\e[A') move -1 ;;
      j|$'\e[B') move 1 ;;
      '') activate; load ;;
      r) [ -n "$slug" ] && rm -f "$cache"; pr_fetch; load; git_scan ;;
      q) bash "$HERDR_PLUGIN_ROOT/panel.sh" off; exit 0 ;;
    esac
  else
    (( tick++ ))
    (( tick % 2 == 0 )) && { pr_fetch; load; move 0; }
    (( tick % GIT_EVERY == 0 )) && git_scan
  fi
  render
done
