#!/usr/bin/env bash
# Produce the in-app updater artifacts from a staged bundle:
#
#   kimi-ui-macos-arm64.app.tar.gz      the payload the updater installs
#   kimi-ui-macos-arm64.app.tar.gz.sig  its minisign signature
#   latest.json                         the manifest the updater plugin reads
#
# Expects the staged bundle at build/Kimi Code.app (run packaging/make-app.sh
# first) and a tag like v0.1.19 in GITHUB_REF_NAME (or passed as $1).
#
# Signing needs TAURI_SIGNING_PRIVATE_KEY (+ _PASSWORD). Locally that can come
# from the key file: TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/kimi-ui.key)".
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="${1:-${GITHUB_REF_NAME:-}}"
VERSION="${VERSION#v}"
if [ -z "$VERSION" ]; then
  echo "error: version missing (pass it as \$1 or set GITHUB_REF_NAME)" >&2
  exit 1
fi

APP="build/Kimi Code.app"
if [ ! -d "$APP" ]; then
  echo "error: $APP not found — run packaging/make-app.sh first" >&2
  exit 1
fi

ARTIFACT="kimi-ui-macos-arm64.app.tar.gz"
rm -f "$ARTIFACT" "$ARTIFACT.sig"

# tar.gz (not zip) because that is the archive format the updater installs;
# built from the same staged bundle the release .zip uses.
#
# --no-xattrs --no-mac-metadata matter: BSD tar would otherwise emit PAX
# extended headers carrying `com.apple.provenance` (and AppleDouble `._`
# entries). `tar -t` hides them, but the updater's Rust tar reader cannot
# unpack them and the install fails with "failed to unpack `._Kimi Code.app`".
tar --no-xattrs --no-mac-metadata -czf "$ARTIFACT" -C build "Kimi Code.app"

if ! cargo tauri --version >/dev/null 2>&1; then
  echo "installing tauri-cli for signing…" >&2
  cargo install tauri-cli --version "^2" --locked
fi
cargo tauri signer sign "$ARTIFACT"

# Release notes: the same CHANGELOG paragraph CI puts on the Release.
awk -v v="$VERSION" '$0 ~ "^## " v " " {on=1; next} /^## / {on=0} on' CHANGELOG.md > notes.md
[ -s notes.md ] || echo "本版本无单独版本说明，详见提交历史。" > notes.md

VERSION="$VERSION" python3 scripts/make-latest-json.py

echo "✓ updater artifacts ready:"
ls -la "$ARTIFACT" "$ARTIFACT.sig" latest.json
