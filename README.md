<div align="Center">
<h1>avail-light</h1>
<h3> Light client for the Polygon Avail blockchain</h3>
</div>

<br>

[![Build status](https://github.com/maticnetwork/avail-light/actions/workflows/default.yml/badge.svg)](https://github.com/maticnetwork/avail-light/actions/workflows/default.yml) [![Code coverage](https://codecov.io/gh/maticnetwork/avail-light/branch/main/graph/badge.svg?token=7O2EA7QMC2)](https://codecov.io/gh/maticnetwork/avail-light)

![demo](./img/prod_demo.png)

## Introduction

Naive approach for building one AVAIL light client, which will do following

- Listen for newly mined blocks
- As soon as new block is available, attempts to eventually gain confidence by asking for proof from full client _( via JSON RPC interface )_ for `N` many cells where cell is defined as `{row, col}` pair. The number of cells are randomly sampled to fetch the confidence eventually.

### Modes of Operation

1. **Light-client Mode**: The basic mode of operation and is always active in whichever mode is operational. If an `App_ID` is not provided, this mode will commence. The client on each header it receives will do random sampling using RPC calls. It gets random cells with proofs in return, which verifies and calculates the confidence.
2. **App-Specific Mode**: If an **`App_ID` > 0** is given in the config file, the client finds out the `cols` related to the provided `App_ID` using `app_data_lookup` in the header. The client then fetches 50% of cells from `cols`, verifies them, and uses them to decode and reconstruct the `app_extrinsics_data`. 
3. **Fat-Client Mode**: The client retrieves the entire extended matrix using IPFS (if available) or fetches via RPC calls. It verifies all the cells and computes the CID mapping for the IPFS Pinning. It then decodes the extended matrix and reconstructs the `app_specific_data` related to all `App_IDs`.

## Installation

Start by cloning this repo in your local setup:

```ssh
git clone git@github.com:maticnetwork/avail-light.git
```

Create one yaml configuration file in the root of the project & put following content:

```bash
touch config.yaml
```

```yaml
http_server_host = "127.0.0.1"
http_server_port = 7000

ipfs_seed = 1
ipfs_port = 37000
ipfs_path = "avail_ipfs_store"

# put full_node_rpc = https://devnet-avail.polygon.technology/ incase you are connecting to devnet
full_node_rpc = ["http://127.0.0.1:9933"]
# put full_node_ws = wss://devnet-avail.polygon.technology/ws incase you are connecting to devnet
full_node_ws = ["ws://127.0.0.1:9944"]
# None in case of default Light Client Mode
app_id = 0

confidence = 92.0
avail_path = "avail_path"

bootstraps = [["12D3KooWMm1c4pzeLPGkkCJMAgFbsfQ8xmVDusg272icWsaNHWzN", "/ip4/127.0.0.1/tcp/39000"]]

# See https://docs.rs/log/0.4.14/log/enum.LevelFilter.html for possible log level values
log_level = "INFO"
```

Now, run the client:

```bash
cargo run -- -c config.yaml  
```

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

>  `serialisedConfidence` is calculated as: 
> `blockNumber << 32 | int32(confidence * 10 ** 7)`, where confidence is represented out of 10 ** 9.

## Test Code Coverage Report

We are using [grcov](https://github.com/mozilla/grcov) to aggregate code coverage information and generate reports.

To install grcov, run:

	$> cargo install grcov

Source code coverage data is generated when running tests with:

	$> env RUSTFLAGS="-C instrument-coverage" \
		LLVM_PROFILE_FILE="tests-coverage-%p-%m.profraw" \
		cargo test

To generate the report, run:

	$> grcov . -s . \
		--binary-path ./target/debug/ \
		-t html \
		--branch \
		--ignore-not-existing -o \
		./target/debug/coverage/

To clean up generate coverage information files, run:

	$> find . -name \*.profraw -type f -exec rm -f {} +

Open `index.html` from the `./target/debug/coverage/` folder to review coverage data.
