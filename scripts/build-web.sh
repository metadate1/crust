#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
mkdir -p "$ROOT/target"
CARGO_OUTPUT="${CARGO_TARGET_DIR:-$ROOT/target}"
if [[ "$CARGO_OUTPUT" != /* ]]; then
  CARGO_OUTPUT="$ROOT/$CARGO_OUTPUT"
fi
LOCK="$ROOT/target/web-dist.lock"
if ! mkdir "$LOCK" 2>/dev/null; then
  LOCK_PID="$(cat "$LOCK/pid" 2>/dev/null || true)"
  echo "another crust web build is already running${LOCK_PID:+ (PID $LOCK_PID)}" >&2
  echo "remove $LOCK only after confirming that no build process is alive" >&2
  exit 1
fi
echo "$$" > "$LOCK/pid"

STAGE=""
BACKUP="$ROOT/target/web-dist.previous"
cleanup() {
  if [[ -d "$BACKUP" && ! -d "$ROOT/dist" ]]; then
    mv "$BACKUP" "$ROOT/dist"
  fi
  if [[ -n "$STAGE" ]]; then
    rm -rf "$STAGE"
  fi
  rm -rf "$BACKUP" "$LOCK"
}
trap cleanup EXIT

if [[ -d "$BACKUP" && ! -d "$ROOT/dist" ]]; then
  mv "$BACKUP" "$ROOT/dist"
else
  rm -rf "$BACKUP"
fi
SOURCE_SHA="$(node "$ROOT/scripts/build-info.mjs" fingerprint "$ROOT")"

cargo build --locked --release --target wasm32-unknown-unknown -p crust-web
STAGE="$(mktemp -d "$ROOT/target/web-dist.XXXXXX")"

cp -R "$ROOT/web/." "$STAGE/"
wasm-bindgen \
  --target web \
  --no-typescript \
  --out-dir "$STAGE/pkg" \
  --out-name crust_web \
  "$CARGO_OUTPUT/wasm32-unknown-unknown/release/crust_web.wasm"

node "$ROOT/scripts/build-info.mjs" write "$ROOT" "$STAGE" "$SOURCE_SHA"

if [[ -d "$ROOT/dist" ]]; then
  mv "$ROOT/dist" "$BACKUP"
fi
if ! mv "$STAGE" "$ROOT/dist"; then
  if [[ -d "$BACKUP" ]]; then
    mv "$BACKUP" "$ROOT/dist"
  fi
  exit 1
fi
rm -rf "$BACKUP"
rm -rf "$LOCK"
trap - EXIT
