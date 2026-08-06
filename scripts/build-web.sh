#!/usr/bin/env bash
# Stage the official prebuilt Kimi Code web bundle at web-dist/.
#
# Usage: scripts/build-web.sh
# Env:   KIMI_CODE_REPO  — path of the official kimi-code checkout
#                          (default: ~/project/kimi-code)
set -euo pipefail
cd "$(dirname "$0")/.."

REPO="${KIMI_CODE_REPO:-$HOME/project/kimi-code}"
SOURCE="$REPO/apps/kimi-code/dist-web"
if [ ! -f "$SOURCE/index.html" ]; then
  echo "error: official web bundle not found at $SOURCE (set KIMI_CODE_REPO)" >&2
  exit 1
fi
if ! grep -R -q --include='*.js' '"kimi_origin"' "$SOURCE/assets"; then
  echo "error: bundle at $SOURCE lacks the official kimi_origin desktop handoff" >&2
  exit 1
fi

# Stage atomically: a cargo build that runs while web-dist is being replaced
# must never embed a half-copied bundle (that white-screens the app).
rm -rf web-dist.tmp
cp -R "$SOURCE" web-dist.tmp
bash scripts/check-web-performance.sh web-dist.tmp
rm -rf web-dist
mv web-dist.tmp web-dist
echo "✓ web-dist/ updated from official bundle at $SOURCE"
