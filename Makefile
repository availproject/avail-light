SHELL := /bin/bash

build:
	pushd bin/full-node; cargo build --release; popd

run:
	export FullNodeURL="http://localhost:9999" && ./target/release/full-node --tmp --chain westend --output none
