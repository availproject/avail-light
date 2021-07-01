# avail-light

Data Availability ( generic ) Light Client for Polygon Avail DA Layer

## Motivation

A need for light client which joins p2p networking layer formed by Polygon Avail Validator & Full Nodes; receives messages by implementing block announce, consensus etc. p2p protocols; verifies block content by random sampling; exposes HTTP server for responding to gained block confidence queries --- made us write this piece of software.

**For more context: [Polygon Avail](https://avail-docs.matic.today)**

> Note: It stands on shoulders of [smoldot](https://github.com/paritytech/smoldot).

## Installation

- Make sure you've `gcc`, `pkg-config`, `sqlite3` installed
- You need to also have `cargo` for building & packaging

> Here's a [guide](https://substrate.dev/docs/en/knowledgebase/getting-started) !

- After cloning the repo, run following from root of project

```bash
make build
```

- For running light client, set following environment variables

```bash
export FullNodeURL=https://polygon-da-rpc.matic.today
export Port=7000
```

---

Field | Interpretation
--- | ---
FullNodeURL | RPC Url of full/ validator node, used for asking proofs `( default: http://localhost:9999 )`
Port | HTTP Server to run on this port `( default: 7000 )`

---

- Running light client with in-memory database

```bash
make run_temp
```

- For using in-disk database ( **Recommended when in production** )

```bash
make run
```

- For directly interacting with  binary

```bash
./target/release/full-node -h
```

> Polygon Avail's chain-spec file is [here](bin/polygon-avail.json)

## Usage

### Block Confidence

- Send query

```bash
curl -s localhost:7000/v1/confidence/2 | jq # for block 2
```

- Response looks like

```json
{
  "block": 2,
  "confidence": 93.75,
  "serialisedConfidence": 9527434592
}
```

Field | Interpretation
--- | ---
block | Confidence for block **X**
confidence | Confidence factor calculated out of 100 ( i.e. percentage value )
serialisedConfidence | Calculated using `block << 32 \| int32(confidence * 10 ** 7)`

```python3
>>> (2 << 32) | int(93.75 * (10 ** 7))
9527434592
```

> `serialisedConfidence` field is used in on-chain light client ( with Chainlink Model ), where field fits in 256 bits ( i.e. as uint256 )
