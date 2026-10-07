#!/usr/bin/env bash
# Builds target/release/yt-lite.app (release build, ad-hoc signed).
# Usage: scripts/bundle-macos.sh [--install]   (--install copies it to /Applications)
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
cargo build --release

APP=target/release/yt-lite.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/yt-lite "$APP/Contents/MacOS/yt-lite"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>yt-lite</string>
  <key>CFBundleDisplayName</key><string>yt-lite</string>
  <key>CFBundleIdentifier</key><string>local.yt-lite</string>
  <key>CFBundleExecutable</key><string>yt-lite</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.entertainment</string>
</dict>
</plist>
PLIST

# Ad-hoc signature so Gatekeeper and the Keychain treat it as one app.
codesign --force --deep --sign - "$APP"
echo "Built $APP"

if [[ "${1:-}" == "--install" ]]; then
  rm -rf /Applications/yt-lite.app
  cp -R "$APP" /Applications/
  echo "Installed to /Applications/yt-lite.app"
fi
