#!/usr/bin/env bash
# Cargo runner on macOS (see .cargo/config.toml): signs the yt-lite binary with
# a stable identity before `cargo run` launches it. Other binaries (tests,
# examples) run unchanged.
set -euo pipefail
if [[ "$(basename "$1")" == "yt-lite" ]]; then
  "$(dirname "$0")/macos-sign.sh" "$1"
fi
exec "$@"
