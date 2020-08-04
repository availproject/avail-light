#!/usr/bin/env sh

cargo +nightly build --release --target wasm32-unknown-unknown --no-default-features --features wasm-bindings
wasm-bindgen ../target/wasm32-unknown-unknown/release/substrate_lite.wasm --out-dir pkg --target web
python -m http.server 8000
