#!/usr/bin/env bash
# Build the customized kimi-web bundle from the fork and stage it at web-dist/.
#
# Usage: scripts/build-web.sh
# Env:   KIMI_CODE_FORK  — path of the kimi-code fork clone
#                          (default: ~/project/kimi-code)
set -euo pipefail
cd "$(dirname "$0")/.."

FORK="${KIMI_CODE_FORK:-$HOME/project/kimi-code}"
if [ ! -d "$FORK/apps/kimi-web" ]; then
  echo "error: fork not found at $FORK (set KIMI_CODE_FORK)" >&2
  exit 1
fi

# pnpm via corepack (honors the repo's packageManager pin). Prefer the
# known-good local Node when present, otherwise use whatever is on PATH.
if [ -d "$HOME/.nvm/versions/node/v24.18.0/bin" ]; then
  export PATH="$HOME/.nvm/versions/node/v24.18.0/bin:$PATH"
fi
if ! command -v corepack >/dev/null 2>&1; then
  echo "error: corepack not found — install Node.js >= 22 first" >&2
  exit 1
fi

corepack pnpm -C "$FORK" install --prefer-offline
corepack pnpm -C "$FORK/apps/kimi-web" run build

rm -rf web-dist
cp -R "$FORK/apps/kimi-web/dist" web-dist
echo "✓ web-dist/ updated from $FORK"
