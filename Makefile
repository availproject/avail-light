SHELL := /bin/bash

build:
	pushd bin/full-node; cargo build --release; popd

run_temp:
	export FullNodeURL=https://polygon-da-rpc.matic.today && ./target/release/full-node --tmp --node-key 0000000000000000000000000000000000000000000000000000000000000008 --chain bin/polygon-avail.json --output logs

run:
	export FullNodeURL=https://polygon-da-rpc.matic.today && ./target/release/full-node --node-key 0000000000000000000000000000000000000000000000000000000000000008 --chain bin/polygon-avail.json --output logs
