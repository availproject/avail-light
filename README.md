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
* Random sampling and proof verification of a predetermined number of cells (`{row, col}` pairs) on each new block. After successful block verification, confidence is calculated for a number of *cells* (`N`) in a matrix, with `N` depending on the percentage of certainty the light client wants to achieve.
* Data reconstruction through application client (WIP).
* HTTP endpoints exposing relevant data, both from the light and application clients

### Modes of Operation

1. **Light-client Mode**: The basic mode of operation and is always active no matter the mode selected. If an `App_ID` is not provided (or is =0), this mode will commence. On each header received the client does random sampling using two mechanisms:

    1. DHT - client first tries to retrieve cells via Kademlia.
	2. RPC - if DHT retrieve fails, the client uses RPC calls to Avail nodes to retrieve the needed cells. The cells not already found in the DHT will be uploaded.

	Once the data is received, light client verifies individual cells and calculates the confidence, which is then stored locally.

2. **App-Specific Mode**: If an **`App_ID` > 0** is given in the config file, the application client (part ot the light client) downloads all the relevant app data, reconstructs it and persists it locally. Reconstructed data is then available to accessed via an HTTP endpoint. (WIP)

3. **Fat-Client Mode**: The client retrieves larger contiguous chunks of the matrix on each block via RPC calls to an Avail node, and stores them on the DHT. This mode is activated when the `block_matrix_partition` parameter is set in the config file, and is mainly used with the `disable_proof_verification` flag because of the resource cost of cell validation. 
**IMPORTANT**: disabling proof verification introduces a trust assumption towards the node, that the data provided is correct. 

## Installation

Start by cloning this repo in your local setup:

```ssh
git clone git@github.com:maticnetwork/avail-light.git
```

Create one yaml configuration file in the root of the project & put following content. Config example is for a bootstrap client, detailed config specs can be found bellow.

```bash
touch config.yaml
```

```yaml
log_level = "info"
http_server_host = "127.0.0.1"
http_server_port = "7000"

libp2p_seed = 1
libp2p_port = "37000"

full_node_rpc = ["http://127.0.0.1:9933"]
full_node_ws = ["ws://127.0.0.1:9944"]
app_id = 0
confidence = 92.0
avail_path = "avail_path"
prometheus_port = 9520
bootstraps = []
```

Now, run the client:

```bash
cargo run -- -c config.yaml  
```

## Config reference
```yaml
log_level = "info"
# Light client HTTP server host name (default: 127.0.0.1)
http_server_host = "127.0.0.1"
# Light client HTTP server port (default: 7000).
http_server_port = "7000"
# Secret key for libp2p keypair. Can be either set to `seed` or to `key`.
# If set to seed, keypair will be generated from that seed.
# If set to key, a valid ed25519 private key must be provided, else the client will fail
# If `secret_key` is not set, random seed will be used.
secret_key = { key =  "1498b5467a63dffa2dc9d9e069caf075d16fc33fdd4c3b01bfadae6433767d93" }
# Libp2p service port range (port, range) (default: 37000).
libp2p_port = "37000"
# Vector of IPFS bootstrap nodes, used to bootstrap DHT. If not set, light client acts as a bootstrap node, waiting for first peer to connect for DHT bootstrap (default: empty).
bootstraps = [["12D3KooWMm1c4pzeLPGkkCJMAgFbsfQ8xmVDusg272icWsaNHWzN", "/ip4/127.0.0.1/tcp/37000"]]

# RPC endpoint of a full node for proof queries, etc. (default: http://127.0.0.1:9933).
full_node_rpc = ["http://127.0.0.1:9933"]
# WebSocket endpoint of a full node for subscribing to the latest header, etc (default: ws://127.0.0.1:9944).
full_node_ws = ["ws://127.0.0.1:9944"]
# ID of application used to start application client. If app_id is not set, or set to 0, application client is not started (default: 0).
app_id = 0
# Confidence threshold, used to calculate how many cells needs to be sampled to achieve desired confidence (default: 92.0).
confidence = 92.0
# File system path where RocksDB used by light client, stores its data.
avail_path = "avail_path"
# Prometheus service port, used for emitting metrics to prometheus server. (default: 9520)
prometheus_port = 9520
# If set to true, logs are displayed in JSON format, which is used for structured logging. Otherwise, plain text format is used (default: false).
log_format_json = false
# Fraction and number of the block matrix part to fetch (e.g. 2/20 means second 1/20 part of a matrix). This is the parameter that determines whether the client behaves as fat client or light client
block_matrix_partition = "1/20"
# Disables proof verification in general, if set to true, otherwise proof verification is performed. (default: false).
disable_proof_verification = false
# Disables fetching of cells from RPC, set to true if client expects cells to be available in DHT (default: false)
disable_rpc = false
# Number of parallel queries for cell fetching via RPC from node (default: 8).
query_proof_rpc_parallel_tasks = 8
# Maximum number of cells per request for proof queries (default: 30).
max_cells_per_rpc = 30
# Maximum number of parallel tasks spawned for GET and PUT operations on DHT (default: 20).
dht_parallelization_limit = 20
# Number of seconds to postpone block processing after the block finalized message arrives. (default: 0).
block_processing_delay = 0
# How many blocks before the latest block should the client sync. If parameter is empty, or set to 0, syncing is disabled. (default: 0).
sync_blocks_depth = 0
# Time-to-live for DHT entries in seconds (default: 24h).
# Default value is set for light clients. Due to the heavy duty nature of the fat clients, it is recommended to be set far bellow this value - not greater than 1hr.
# Record TTL, publication and replication intervals are co-dependent: TTL >> publication_interval >> replication_interval.
record_ttl = 86400
# Sets the (re-)publication interval of stored records, in seconds. This interval should be significantly shorter than the record TTL, ensure records do not expire prematurely. (default: 12h).
# Default value is set for light clients. Fat client value needs to be inferred from the TTL value.
# This interval should be significantly shorter than the record TTL, to ensure records do not expire prematurely.
publication_interval = 43200
# Sets the (re-)replication interval for stored records, in seconds. This interval should be significantly shorter than the publication interval, to ensure persistence between re-publications. (default: 3h).
# Default value is set for light clients. Fat client value needs to be inferred from the TTL and publication interval values.
# This interval should be significantly shorter than the publication interval, to ensure persistence between re-publications.
replication_interval = 10800
# The replication factor determines to how many closest peers a record is replicated. (default: 20).
replication_factor = 20
# Sets the amount of time to keep connections alive when they're idle. (default: 30s).
# NOTE: libp2p default value is 10s, but because of Avail block time of 20s the value has been increased
connection_idle_timeout = 30
# Sets the timeout for a single Kademlia query. (default: 60s).
query_timeout = 60
# Sets the allowed level of parallelism for iterative Kademlia queries. (default: 3).
query_parallelism = 3
# Sets the Kademlia caching strategy to use for successful lookups. If set to 0, caching is disabled. (default: 1).
caching_max_peers = 1
# Require iterative queries to use disjoint paths for increased resiliency in the presence of potentially adversarial nodes. (default: false).
disjoint_query_paths = false
# The maximum number of records. (default: 2400000).
# The default value has been calculated to sustain ~1hr worth of cells, in case of blocks with max sizes being produces in 20s block time for fat clients
# (256x512) * 3 * 60
max_kad_record_number = 2400000,
# The maximum size of record values, in bytes. (default: 100).
max_kad_record_size = 100,
# The maximum number of provider records for which the local node is the provider. (default: 1024).
max_kad_provided_keys = 1024
```

## Notes

- When running the first light client in a network, it becomes a bootstrap client. Once its execution is started, it is paused until a second light client has been started and connected to it, so that the DHT bootstrap mechanism can complete successfully. 
- Immediately after starting a fresh light client, block sync is executed to a block depth set in the `sync_blocks_depth` config parameter. The sync client is using both the DHT and RPC for that purpose.
- In order to spin up a fat client, config needs to contain the `block_matrix_partition` parameter set to a fraction of matrix. It is recommended to set the `disable_proof_verification` to true, because of the resource costs of proof verification.
- `sync_blocks_depth` needs to be set correspondingly to the max number of blocks the connected node is caching (if downloading data via RPC).
- Prometheus is used for exposing detailed metrics about the light client



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


Given a block number (as _(hexa-)_ decimal number), return the extrinsic in hex string format, if app id is specified in the config

```bash
curl -s localhost:7000/v1/appdata/<block-number>
```

Result:

```json
{"block":386,"extrinsics":["{hex_encoded_extrinsic}"]}
```

Query parameter `decode=true` can be used to return submitted data in base64 encoded string:

```bash
curl -s localhost:7000/v1/appdata/<block-number>?decode=true
```

Result:

```json
{"block":386,"extrinsics":["{base64_encoded_submit_data}"]}
```


Returns the Mode of the Light Client

```bash
curl -s localhost:7000/v1/mode
```

Returns the status of a latest block

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
