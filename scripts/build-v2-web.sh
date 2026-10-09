#!/usr/bin/env bash
set -euo pipefail
ROOT="$(dirname "$(dirname "$(realpath "$0")")")"
DIST="$ROOT/v2/crates/browser/dist"
cargo build --manifest-path "$ROOT/v2/Cargo.toml" --locked --release --target wasm32-unknown-unknown -p portfolio-v2-browser
mkdir -p "$DIST"
wasm-bindgen "$ROOT/v2/target/wasm32-unknown-unknown/release/portfolio_v2_browser.wasm" --target web --out-dir "$DIST" --out-name portfolio_v2_browser
for file in index.html style.css renderer.js webgl.js pixel.js host.js worker.js assets.js; do
  cp "$ROOT/v2/crates/browser/$file" "$DIST/$file"
done
cp "$ROOT/portfolio/data/iosevka-portfolio.woff2" "$DIST/"
python3 "$ROOT/scripts/build-v2-atlas.py" "$DIST"
BUILD=$(sha256sum "$DIST/portfolio_v2_browser_bg.wasm" "$DIST/portfolio_v2_browser.js" \
  "$DIST/index.html" "$DIST/style.css" "$DIST/renderer.js" "$DIST/webgl.js" \
  "$DIST/pixel.js" "$DIST/host.js" "$DIST/worker.js" "$DIST/assets.js" "$DIST/iosevka-portfolio.woff2" "$DIST/glyph-atlas.png" "$DIST/glyph-atlas.json" \
  | cut -d ' ' -f 1 | sha256sum | cut -d ' ' -f 1)
for file in index.html style.css host.js worker.js; do
  sed -i "s/__V2_BUILD__/$BUILD/g" "$DIST/$file"
done
mkdir -p "$DIST/build/$BUILD"
for file in style.css renderer.js webgl.js pixel.js host.js worker.js assets.js portfolio_v2_browser.js portfolio_v2_browser_bg.wasm iosevka-portfolio.woff2 glyph-atlas.png glyph-atlas.json; do
  cp "$DIST/$file" "$DIST/build/$BUILD/$file"
done
