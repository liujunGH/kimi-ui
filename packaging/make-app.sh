#!/usr/bin/env bash
# Build "Kimi Code.app" into build/ (no tauri-cli needed), then optionally
# atomically install it to /Applications with --install.
#
# Why the dance:
# - never ship a half-copied bundle (install is a single mv, same volume);
# - never keep TWO copies of bundle id dev.kimiui.desktop on disk — macOS
#   LaunchServices dedupes by bundle id, so a stray local copy hijacks
#   "open" / Dock clicks to the stale instance ("app won't start").
#
# Usage:
#   packaging/make-app.sh              # build only → build/Kimi Code.app
#   packaging/make-app.sh --install    # build + atomically install
set -euo pipefail
cd "$(dirname "$0")/.."

APP_NAME="Kimi Code.app"
STAGE="build/$APP_NAME"

# Customized kimi-web bundle (from the fork), staged by scripts/build-web.sh.
if [ ! -d web-dist ]; then
  echo "error: web-dist/ missing — run scripts/build-web.sh first" >&2
  exit 1
fi

cargo build --release

rm -rf "$STAGE"
mkdir -p "$STAGE/Contents/MacOS" "$STAGE/Contents/Resources"
cp target/release/kimi-ui "$STAGE/Contents/MacOS/kimi-ui"
cp packaging/Info.plist "$STAGE/Contents/Info.plist"
# NOTE: web-dist is embedded into the binary at compile time — do NOT copy it
# into Resources/ (dead weight, and a stale copy there shadows nothing).

ICONSET=icons/icon.iconset
rm -rf "$ICONSET"
mkdir -p "$ICONSET"
sips -z 16 16   icons/icon.png --out "$ICONSET/icon_16x16.png"      >/dev/null
sips -z 32 32   icons/icon.png --out "$ICONSET/icon_16x16@2x.png"   >/dev/null
sips -z 32 32   icons/icon.png --out "$ICONSET/icon_32x32.png"      >/dev/null
sips -z 64 64   icons/icon.png --out "$ICONSET/icon_32x32@2x.png"   >/dev/null
sips -z 128 128 icons/icon.png --out "$ICONSET/icon_128x128.png"    >/dev/null
sips -z 256 256 icons/icon.png --out "$ICONSET/icon_128x128@2x.png" >/dev/null
sips -z 256 256 icons/icon.png --out "$ICONSET/icon_256x256.png"    >/dev/null
sips -z 512 512 icons/icon.png --out "$ICONSET/icon_256x256@2x.png" >/dev/null
sips -z 512 512 icons/icon.png --out "$ICONSET/icon_512x512.png"    >/dev/null
cp icons/icon.png "$ICONSET/icon_512x512@2x.png"
iconutil -c icns "$ICONSET" -o "$STAGE/Contents/Resources/icon.icns"

codesign --force --deep --sign - "$STAGE"

if [ "${1:-}" = "--install" ]; then
  pkill -f "/Applications/$APP_NAME/Contents/MacOS/kimi-ui" 2>/dev/null || true
  rm -rf "/Applications/$APP_NAME"
  mv "$STAGE" "/Applications/$APP_NAME"
  echo "✓ installed to /Applications/$APP_NAME"
else
  echo "✓ built $STAGE (append --install to deploy)"
fi
