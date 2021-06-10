SHELL := /bin/bash

build:
	pushd bin/full-node; cargo build --release; popd

run:
	./target/release/full-node --tmp --chain westend --output none
