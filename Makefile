SHELL := /bin/bash

build:
	pushd bin/full-node; cargo build --release; popd

run:
	export FullNodeURL=https://polygon-da-rpc.matic.today && ./target/release/full-node --tmp --node-key 0000000000000000000000000000000000000000000000000000000000000008 --chain chain_spec.json --output logs
