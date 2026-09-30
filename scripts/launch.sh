#!/bin/sh
# Pane entrypoint: load the user's config.env, prefer the Rust board, fall back to bash.
cfg="${HERDR_PLUGIN_CONFIG_DIR:-}/config.env"
if [ -f "$cfg" ]; then
  set -a
  . "$cfg"
  set +a
fi
bin="$HERDR_PLUGIN_ROOT/board/target/release/wt-board"
[ -x "$bin" ] && exec "$bin"
exec bash "$HERDR_PLUGIN_ROOT/board.sh"
