#!/bin/sh
# Install-time build. A failed [[build]] aborts `herdr plugin install`, so a missing Rust
# toolchain is not an error: the pane launcher falls back to the bash board.
cd "$(dirname "$0")/.." || exit 1
if command -v cargo >/dev/null 2>&1; then
  exec cargo build --release --locked --manifest-path board/Cargo.toml
fi
echo "cargo not found: skipping the Rust board, the bash board will be used." >&2
echo "Install Rust (https://rustup.rs) and reinstall the plugin for the full panel." >&2
