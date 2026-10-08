#!/usr/bin/env bash
# Code-signs a yt-lite binary or .app with a stable identity so macOS Keychain
# "Always Allow" survives rebuilds. (Ad-hoc signatures change with every build,
# so Keychain sees each build as a new app and prompts again.)
#
# Identity: $YT_LITE_SIGN_IDENTITY (name or SHA-1), else the first
# "Apple Development" certificate (Xcode creates one when you sign in with your
# Apple ID), else ad-hoc.
# Usage: scripts/macos-sign.sh <binary or .app>
set -euo pipefail
target="$1"

identity="${YT_LITE_SIGN_IDENTITY:-}"
if [[ -z "$identity" ]]; then
  identity=$(security find-identity -v -p codesigning 2>/dev/null \
    | awk '/"Apple Development/ {print $2; exit}')
fi
if [[ -z "$identity" ]]; then
  echo "macos-sign: no Apple Development certificate found; signing ad-hoc (Keychain will prompt after each rebuild)" >&2
  identity="-"
fi

codesign --force --sign "$identity" --identifier local.yt-lite "$target" 2>/dev/null \
  || codesign --force --sign - --identifier local.yt-lite "$target"
