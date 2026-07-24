#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
mkdir -p "$ROOT/target"
OUTPUT="$ROOT/target/browser-test-dist"
LOCK="$ROOT/target/browser-test-dist.lock"
if ! mkdir "$LOCK" 2>/dev/null; then
  LOCK_PID="$(cat "$LOCK/pid" 2>/dev/null || true)"
  echo "another crust browser-harness build is already running${LOCK_PID:+ (PID $LOCK_PID)}" >&2
  echo "remove $LOCK only after confirming that no build process is alive" >&2
  exit 1
fi
echo "$$" > "$LOCK/pid"

STAGE=""
BACKUP="$ROOT/target/browser-test-dist.previous"
cleanup() {
  if [[ -d "$BACKUP" && ! -d "$OUTPUT" ]]; then
    mv "$BACKUP" "$OUTPUT"
  fi
  if [[ -n "$STAGE" ]]; then
    rm -rf "$STAGE"
  fi
  rm -rf "$BACKUP" "$LOCK"
}
trap cleanup EXIT

if [[ -d "$BACKUP" && ! -d "$OUTPUT" ]]; then
  mv "$BACKUP" "$OUTPUT"
else
  rm -rf "$BACKUP"
fi
SOURCE_SHA="$(node "$ROOT/scripts/build-info.mjs" fingerprint "$ROOT")"

cargo build \
  --locked \
  --release \
  --target wasm32-unknown-unknown \
  -p crust-web \
  --features browser-test-harness
STAGE="$(mktemp -d "$ROOT/target/browser-test-dist.XXXXXX")"

cp -R "$ROOT/web/." "$STAGE/"
wasm-bindgen \
  --target web \
  --no-typescript \
  --out-dir "$STAGE/pkg" \
  --out-name crust_web \
  "$ROOT/target/wasm32-unknown-unknown/release/crust_web.wasm"

node "$ROOT/scripts/build-info.mjs" write "$ROOT" "$STAGE" "$SOURCE_SHA"

if [[ -d "$OUTPUT" ]]; then
  mv "$OUTPUT" "$BACKUP"
fi
if ! mv "$STAGE" "$OUTPUT"; then
  if [[ -d "$BACKUP" ]]; then
    mv "$BACKUP" "$OUTPUT"
  fi
  exit 1
fi
STAGE=""
rm -rf "$BACKUP" "$LOCK"
trap - EXIT

echo "crust browser test harness: $OUTPUT"
