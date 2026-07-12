#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"

cargo build --locked --release --target wasm32-unknown-unknown -p crust-web
rm -rf "$ROOT/dist"
mkdir -p "$ROOT/dist"
cp -R "$ROOT/web/." "$ROOT/dist/"
wasm-bindgen \
  --target web \
  --no-typescript \
  --out-dir "$ROOT/dist/pkg" \
  --out-name crust_web \
  "$ROOT/target/wasm32-unknown-unknown/release/crust_web.wasm"

