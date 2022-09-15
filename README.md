<div align="Center">
<h1>avail-light</h1>
<h3> Light client for the Polygon Avail blockchain</h3>
</div>

<br>

[![Build status](https://github.com/maticnetwork/avail-light/actions/workflows/default.yml/badge.svg)](https://github.com/maticnetwork/avail-light/actions/workflows/default.yml) [![Code coverage](https://codecov.io/gh/maticnetwork/avail-light/branch/main/graph/badge.svg?token=7O2EA7QMC2)](https://codecov.io/gh/maticnetwork/avail-light)

![demo](./img/lc.png)

## Introduction

`avail-light` is a data availability light client with the following functionalities:

* Listening on the Avail network for finalized blocks
* Calculating confidence for a number of *cells* (`N`) in a matrix, where *cell* is defined as a `{row, col}` pair.
* Random sampling and proof verification of a predetermined number of cells on each new block. `N` depends on the percentage of certainty the light client wants to achieve.
* Data reconstruction through application client (WIP).

### Modes of Operation

1. **Light-client Mode**: The basic mode of operation and is always active no matter the mode selected. If an `App_ID` is not provided (or is =0), this mode will commence. On each header received the client does random sampling using two mechanisms.

    1. DHT - client first tries to retrieve cells via Kademlia.
	2. RPC - if DHT retrieve fails, the client uses RPC calls to Avail nodes to retrieve the needed cells.

	Once the data is received, light client verifies individual cells and calculates the confidence, which is then stored locally.

2. **App-Specific Mode**: If an **`App_ID` > 0** is given in the config file, the application client (part ot the light client) downloads all the relevant app data, reconstructs it and persists it locally. Reconstructed data is then available to accessed via an HTTP endpoint. (WIP)

3. **Fat-Client Mode**: The client retrieves larger contiguous chunks of the matrix on each block via RPC calls to an Avail node, and stores them on the DHT. This mode is activated when the `partition` parameter is set in the config file, and is mainly used with the `disable_proof_verification` flag because of the resource cost of cell validation. 
**IMPORTANT**: disabling proof verification introduces a trust assumption towards the node, that the data provided is correct. 

## Installation

Start by cloning this repo in your local setup:

```ssh
git clone git@github.com:maticnetwork/avail-light.git
```

Create one yaml configuration file in the root of the project & put following content (config example for a fat client):

```bash
touch config.yaml
```

```yaml
log_level = "info"
# Light client HTTP server host name (default: 127.0.0.1)
http_server_host = "127.0.0.1"
# Light client HTTP server port (default: 7000).
http_server_port = "7001"

# Seed for IPFS keypair. If not set, or seed is 0, random seed is generated
ipfs_seed = 2
# IPFS service port range (port, range) (default: 37000).
ipfs_port = "37001"
# File system path where IPFS service stores data (default: avail_ipfs_node_1)
ipfs_path = "avail_ipfs_store"

# RPC endpoint of a full node for proof queries, etc. (default: http://127.0.0.1:9933).
full_node_rpc = ["http://127.0.0.1:9933"]
# WebSocket endpoint of full node for subscribing to latest header, etc (default: ws://127.0.0.1:9944).
full_node_ws = ["ws://127.0.0.1:9944"]
# ID of application used to start application client. If app_id is not set, or set to 0, application client is not started (default: 0).
app_id = 0
# Confidence threshold, used to calculate how many cells needs to be sampled to achieve desired confidence (default: 92.0).
confidence = 92.0
# File system path where RocksDB used by light client, stores its data.
avail_path = "avail_path"
# Prometheus service port, used for emmiting metrics to prometheus server. (default: 9520)
prometheus_port = 9521
# Fraction and number of the block matrix part to fetch (e.g. 2/20 means second 1/20 part of a matrix)
block_matrix_partition = "1/20"
# Disables proof verification in general, if set to true, otherwise proof verification is performed. (default: false).
disable_proof_verification = true
# Number of parallel queries for cell fetching via RPC from node (default: 8).
query_proof_rpc_parallel_tasks = 300
# Maximum number of cells per request for proof queries (default: 30).
max_cells_per_rpc = 1024
# Maximum number of parallel tasks spawned, when fetching from DHT (default: 4096).
max_parallel_fetch_tasks = 1024
# Vector of IPFS bootstrap nodes, used to bootstrap DHT. If not set, light client acts as a bootstrap node, waiting for first peer to connect for DHT bootstrap (default: empty).
bootstraps = [["/ip4/127.0.0.1/tcp/39000"]]
```

Now, run the client:

```bash
cargo run -- -c config.yaml  
```

## Notes

- When running the first light client in a network, it becomes a bootstrap client. Once its execution is started, it is paused until a second light client has been started and connected to it, so that the DHT bootstrap mechanism can complete successfully. 

## Usage

Given a block number (as _(hexa-)_ decimal number), return confidence obtained by the light client for this block:

```bash
curl -s localhost:7000/v1/confidence/ <block-number>
```

Result:

```json
{
    "number": 223,
    "confidence": 99.90234375,
    "serialisedConfidence": "958776730446"
}
```

> `serialisedConfidence` is calculated as:
> `blockNumber << 32 | int32(confidence * 10 ** 7)`, where confidence is represented out of 10 ** 9.


Given a block number (as _(hexa-)_ decimal number), return the content of the data if specified in config in hex string format

```bash
curl -s localhost:7000/v1/appdata/ <block-number>
```

Result:

```json
{"block":386,"extrinsics":[{"app_id":1,"signature":{"Sr25519":"be86221cc07a461537570637d75a0569c2210286e85c693e3b31d94211b1ef1eaf451b13072066f745f70801ad6af0dcdf2e42b7bf77be2dc6709196b4d45889"},"data":"0x313537626233643536653339356537393237633664"}]}
```

Return the Mode of the Light Client

```bash
curl -s localhost:7000/v1/Mode
```

Return the status of a latest block 

```bash
curl -s localhost:7000/v1/status
```

Returns the latest block 

```bash
curl -s localhost:7000/v1/latest_block
```

## Test Code Coverage Report

We are using [grcov](https://github.com/mozilla/grcov) to aggregate code coverage information and generate reports.

To install grcov, run:

```bash
cargo install grcov
```

Source code coverage data is generated when running tests with:

```bash
env RUSTFLAGS="-C instrument-coverage" \
	LLVM_PROFILE_FILE="tests-coverage-%p-%m.profraw" \
	cargo test
```

To generate the report, run:

```bash
grcov . -s . \
	--binary-path ./target/debug/ \
	-t html \
	--branch \
	--ignore-not-existing -o \
	./target/debug/coverage/
```

To clean up generate coverage information files, run:

```bash
find . -name \*.profraw -type f -exec rm -f {} +
```

Open `index.html` from the `./target/debug/coverage/` folder to review coverage data.
