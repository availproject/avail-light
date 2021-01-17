Lightweight Substrate and Polkadot client.

# Introduction

`smoldot` is a prototype of an alternative client of [Substrate](https://github.com/paritytech/substrate)-based chains, including [Polkadot](https://github.com/paritytech/polkadot/).

In order to simplify the code, two main design decisions have been made compared to Substrate:

- No native runtime. The execution time of the `wasmtime` library is satisfying enough that having a native runtime isn't critical anymore.

- No pluggable architecture. `smoldot` supports a certain hardcoded list of consensus algorithms, at the moment Babe, Aura, and GrandPa. Support for other algorithms can only be added by modifying the code of smoldot, and it is not possible to plug a custom algorithm from outside.

## How to test

There exists two clients: the full client and the wasm light node.

### Full client

The full client is a binary similar to the official Polkadot client, and can be tested with `cargo run`.

> Note: The `Cargo.toml` contains a section `[profile.dev] opt-level = 2`, and as such `cargo run` alone should give performances close to the ones in release mode.

### Wasm light node

The wasm light node can be tested with `cd bin/wasm-node/javascript` and `npm start`. This will start a WebSocket server capable of answering JSON-RPC requests. You can then navigate to <https://polkadot.js.org/apps/?rpc=ws%3A%2F%2F127.0.0.1%3A9944> in order to interact with the Westend chain.

> Note: The `npm start` command starts a small JavaScript shim, on top of the wasm light node, that hardcodes the chain to Westend and starts the WebSocket server. The wasm light node itself can connect to a variety of different chains (not only Westend) and doesn't start any server.

# Objectives

There exists multiple objectives behind this repository:

- Write a client implementation that is as comprehensive as possible, to make it easier to understand the various components of a Substrate/Polkadot client. A large emphasis is put on documentation, and the documentation of the `main` branch is automatically deployed [here](https://paritytech.github.io/smoldot/smoldot/index.html).
- Implement a client that is lighter than Substrate, in terms of memory consumption, number of threads, and code size, in order to compile it to WebAssembly and distribute it in webpages.
- Experiment with a new code architecture, to maybe upstream some components to Substrate and Polkadot.

# Status

As a quick overview, at the time of writing of this README, the following is supported:

- Verifying Babe and Aura blocks.
- "Executing" blocks, by calling `Core_execute_block`.
- Verifying GrandPa justifications.
- "Optimistic syncing", in other words syncing by assuming that there isn't any fork.
- Verifying storage trie proofs.
- The WebSocket JSON-RPC server is in progress, but its design is still changing.
- An informant.
- A telemetry client (mostly copy-pasted from Substrate and substrate-telemetry).
- An unfinished new networking stack.

The following isn't done yet:

- Authoring blocks isn't supported.
- There is no transaction pool.
- Anything related to GrandPa networking messages. Finality can only be determined by asking a full node for a justification.
- No actual database for the full client.
- The changes trie isn't implemented (it is not enabled on Westend, Kusama and Polkadot at the moment).
- A Prometheus server. While not difficult to implement, it seems a bit overkill to have one at the moment.
