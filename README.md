# avail-light

Light client for Data Availability Blockchain of Polygon 💻

![demo](./img/prod_demo.png)

## Introduction

Naive approach for building one DA light client, which will do following

- Listen for newly mined blocks
- As soon as new block is available, attempts to eventually gain confidence by asking for proof from full client _( via JSON RPC interface )_ for `N` many cells where cell is defined as `{row, col}` pair
- For lower numbered blocks, for which no confidence is yet gained, does batch processing in reverse order i.e. prioritizing latest blocks over older ones

## Installation

- First clone this repo in your local setup
- Create one yaml configuration file in root of project & put following content

```bash
touch config.yaml
```

```yaml
http_server_host = "127.0.0.1"
http_server_port = 7000

ipfs_seed = 1
ipfs_port = 37000
ipfs_path = "avail_ipfs_store"

full_node_rpc = "http://127.0.0.1:9933"
full_node_ws = "ws://127.0.0.1:9944"
app_id = 0
confidence = 92.0

bootstraps = [["12D3KooWMm1c4pzeLPGkkCJMAgFbsfQ8xmVDusg272icWsaNHWzN", "/ip4/127.0.0.1/tcp/39000"]]
```

- Now, let's run client

```bash
cargo run
```

## Usage

Given block number ( as _(hexa-)_ decimal number ) returns confidence obtained by light client for this block

```bash
curl -s localhost:7000/v1/confidence/ _block-number_
```

```json
{
    "number": 223,
    "confidence": 99.90234375,
    "serialisedConfidence": "958776730446"
}
```

---

**Note :** Serialised confidence calculated as: 
> `blockNumber << 32 | int32(confidence * 10 ** 7)`, where confidence is represented as out of 10 ** 9



