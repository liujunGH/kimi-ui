#!/usr/bin/env bash
# Build "Kimi Code.app" from the release binary (no tauri-cli needed):
#   Info.plist + binary + icon.icns + ad-hoc codesign.
# Usage: packaging/make-app.sh
set -euo pipefail
cd "$(dirname "$0")/.."

APP_NAME="Kimi Code.app"

# Customized kimi-web bundle (from the fork) is embedded into the release
# binary at compile time; it must exist before `cargo build`.
if [ ! -d web-dist ]; then
  echo "error: web-dist/ missing — run scripts/build-web.sh first" >&2
  exit 1
fi

cargo build --release

rm -rf "$APP_NAME"
mkdir -p "$APP_NAME/Contents/MacOS" "$APP_NAME/Contents/Resources"
cp target/release/kimi-ui "$APP_NAME/Contents/MacOS/kimi-ui"
cp packaging/Info.plist "$APP_NAME/Contents/Info.plist"

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
iconutil -c icns "$ICONSET" -o "$APP_NAME/Contents/Resources/icon.icns"

codesign --force --deep --sign - "$APP_NAME"

# Install: replace the installed app wholesale. A plain `cp -R` MERGES into
# the existing bundle, which accumulated hundreds of stale web assets across
# deployments (the app had silently grown to 79MB).
if [ "${1:-}" = "--install" ]; then
  rm -rf "/Applications/$APP_NAME"
  cp -R "$APP_NAME" /Applications/
  echo "✓ installed to /Applications/$APP_NAME"
fi

echo "✓ built $APP_NAME"
