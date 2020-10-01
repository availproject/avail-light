#!/usr/bin/env sh

# Note: this script shows how to build the browser node, but using this script is in no way mandatory

cargo build --release --target wasm32-unknown-unknown
wasm-bindgen ../../target/wasm32-unknown-unknown/release/substrate_lite_browser_node.wasm --out-dir pkg --target web
python -m http.server 8000 & xdg-open http://localhost:8000
