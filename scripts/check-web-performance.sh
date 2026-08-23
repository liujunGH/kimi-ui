#!/usr/bin/env bash
# Guard the staged official Web bundle against accidental size regressions.
#
# Usage: scripts/check-web-performance.sh [bundle-dir]
# Override a budget with MAX_WEB_FILES, MAX_WEB_BYTES, MAX_ENTRY_JS_BYTES,
# MAX_ENTRY_CSS_BYTES, or MAX_SINGLE_ASSET_BYTES.
set -euo pipefail
cd "$(dirname "$0")/.."

BUNDLE="${1:-web-dist}"
INDEX="$BUNDLE/index.html"

if [ ! -f "$INDEX" ]; then
  echo "error: web bundle index not found at $INDEX" >&2
  exit 1
fi

MAX_WEB_FILES="${MAX_WEB_FILES:-600}"
MAX_WEB_BYTES="${MAX_WEB_BYTES:-41943040}"
# Entry JS raised 2.5 MiB -> 3 MiB for the 0.38.0 official bundle
# (index JS grew 2,092,189 -> 2,808,393 bytes; see docs/performance.md).
MAX_ENTRY_JS_BYTES="${MAX_ENTRY_JS_BYTES:-3145728}"
MAX_ENTRY_CSS_BYTES="${MAX_ENTRY_CSS_BYTES:-614400}"
MAX_SINGLE_ASSET_BYTES="${MAX_SINGLE_ASSET_BYTES:-8388608}"

for value in \
  "$MAX_WEB_FILES" \
  "$MAX_WEB_BYTES" \
  "$MAX_ENTRY_JS_BYTES" \
  "$MAX_ENTRY_CSS_BYTES" \
  "$MAX_SINGLE_ASSET_BYTES"
do
  case "$value" in
    ''|*[!0-9]*)
      echo "error: performance budgets must be positive integers" >&2
      exit 1
      ;;
  esac
done

file_count="$(find "$BUNDLE" -type f | wc -l | tr -d ' ')"
total_bytes="$({
  find "$BUNDLE" -type f -exec sh -c \
    'for file do wc -c < "$file"; done' sh {} +
} | awk '{ total += $1 } END { print total + 0 }')"

entry_js="$(sed -n 's#.*src="/\(assets/[^"?]*\.js\)".*#\1#p' "$INDEX" | head -1)"
entry_css="$(sed -n 's#.*href="/\(assets/[^"?]*\.css\)".*#\1#p' "$INDEX" | head -1)"

if [ -z "$entry_js" ] || [ ! -f "$BUNDLE/$entry_js" ]; then
  echo "error: index.html does not reference a valid hashed JavaScript entry" >&2
  exit 1
fi
if [ -z "$entry_css" ] || [ ! -f "$BUNDLE/$entry_css" ]; then
  echo "error: index.html does not reference a valid hashed CSS entry" >&2
  exit 1
fi

entry_js_bytes="$(wc -c < "$BUNDLE/$entry_js" | tr -d ' ')"
entry_css_bytes="$(wc -c < "$BUNDLE/$entry_css" | tr -d ' ')"
largest_line="$({
  find "$BUNDLE/assets" -type f -exec sh -c \
    'for file do printf "%s\t%s\n" "$(wc -c < "$file" | tr -d " ")" "$file"; done' sh {} +
} | sort -nr | head -1)"
largest_bytes="${largest_line%%$'\t'*}"
largest_asset="${largest_line#*$'\t'}"

unhashed_assets="$({
  find "$BUNDLE/assets" -type f -exec basename {} \;
} | awk '$0 !~ /-[A-Za-z0-9_-]{8,}\.[A-Za-z0-9]+$/ { print }')"
if [ -n "$unhashed_assets" ]; then
  echo "error: assets/ contains non-content-hashed files; immutable caching would be unsafe:" >&2
  printf '%s\n' "$unhashed_assets" >&2
  exit 1
fi

failed=0
check_budget() {
  label="$1"
  actual="$2"
  maximum="$3"
  if [ "$actual" -gt "$maximum" ]; then
    echo "error: $label is $actual (budget: $maximum)" >&2
    failed=1
  fi
}

check_budget "file count" "$file_count" "$MAX_WEB_FILES"
check_budget "total bytes" "$total_bytes" "$MAX_WEB_BYTES"
check_budget "entry JavaScript bytes" "$entry_js_bytes" "$MAX_ENTRY_JS_BYTES"
check_budget "entry CSS bytes" "$entry_css_bytes" "$MAX_ENTRY_CSS_BYTES"
check_budget "largest asset bytes ($largest_asset)" "$largest_bytes" "$MAX_SINGLE_ASSET_BYTES"

printf 'Web performance budget\n'
printf '  files:         %s / %s\n' "$file_count" "$MAX_WEB_FILES"
printf '  total bytes:   %s / %s\n' "$total_bytes" "$MAX_WEB_BYTES"
printf '  entry JS:      %s (%s / %s bytes)\n' "$entry_js" "$entry_js_bytes" "$MAX_ENTRY_JS_BYTES"
printf '  entry CSS:     %s (%s / %s bytes)\n' "$entry_css" "$entry_css_bytes" "$MAX_ENTRY_CSS_BYTES"
printf '  largest asset: %s (%s / %s bytes)\n' "$largest_asset" "$largest_bytes" "$MAX_SINGLE_ASSET_BYTES"

if [ "$failed" -ne 0 ]; then
  exit 1
fi

echo "✓ official Web bundle stays within the performance budget"
