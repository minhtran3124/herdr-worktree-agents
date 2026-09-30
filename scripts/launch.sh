#!/bin/sh
# Pane entrypoint: load the user's config.env, prefer the Rust board, fall back to bash.
cfg="${HERDR_PLUGIN_CONFIG_DIR:-}/config.env"
if [ -f "$cfg" ]; then
  set -a
  . "$cfg"
  set +a
fi
bin="$HERDR_PLUGIN_ROOT/board/target/release/wt-board"
if [ -x "$bin" ]; then
  "$bin"
  rc=$?
  # 126/127: the binary cannot run here (wrong architecture, stale build); use the bash board.
  [ "$rc" -eq 126 ] || [ "$rc" -eq 127 ] || exit "$rc"
fi
exec bash "$HERDR_PLUGIN_ROOT/board.sh"
