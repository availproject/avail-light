#!/bin/bash
set -x
set -e


cargo check
cargo check --features "multiproof"

cd bootstrap && cargo check && cd ./..
cd client && cargo check && cargo check --features "multiproof"  && cd ./..
cd compatibility-tests && cargo check  && cd ./..
cd core && cargo check && cargo check --features "multiproof" && cd ./..
cd crawler && cargo check && cargo check --features "multiproof" && cd ./..
cd data && cargo check && cargo check --features "multiproof" && cd ./..
cd fat && cargo check && cargo check --features "multiproof" && cd ./..
cd monitor-client && cargo check && cd ./..
cd relay && cargo check && cd ./..

rustup target add wasm32-unknown-unknown
# cargo check --target wasm32-unknown-unknown
cd web && cargo check --target wasm32-unknown-unknown && cd ./..
