# Avail Light P2P Load Test Client

A P2P-only client for load testing the Avail DHT network by uploading pre-generated blocks at a configurable rate.

## Overview

This client connects to the Avail P2P network and continuously uploads randomly generated data blocks to the DHT. It's designed for stress testing and performance evaluation of the DHT network without requiring RPC connections.

## Features

- P2P-only client (no RPC required)
- Configurable upload rate (blocks per second)
- Pre-generated blocks with configurable size (up to 512KB)
- Cycles through a configurable number of unique blocks
- Real-time statistics reporting
- Graceful shutdown support

## Usage

```bash
# Run with default settings (1 block/sec, 512KB blocks)
cargo run --bin p2p-load-test

# Run with custom rate and block size
cargo run --bin p2p-load-test -- --rate 10 --block-size 262144

# Run for a specific duration (60 seconds)
cargo run --bin p2p-load-test -- --duration 60

# Use specific network
cargo run --bin p2p-load-test -- --network hex

# Custom P2P port
cargo run --bin p2p-load-test -- --p2p-port 30333

# Target specific peers for DHT storage
cargo run --bin p2p-load-test -- --target-peers "12D3KooWRkGLz4YbVmrsWK75VjFTs8NvaBu42xhAmQaP4KeJpw1L,12D3KooWBmFN8Dh3irQvpZi4LJWqg5RPmFKPPMqH3V5aUtxezFEJ"

# Add manual peers to routing table
cargo run --bin p2p-load-test -- --manual-peers "/ip4/192.168.1.100/tcp/30333/p2p/12D3KooWRkGLz4YbVmrsWK75VjFTs8NvaBu42xhAmQaP4KeJpw1L"

# Combine both for targeted testing
cargo run --bin p2p-load-test -- \
  --manual-peers "/ip4/192.168.1.100/tcp/30333/p2p/12D3KooWRkGLz4YbVmrsWK75VjFTs8NvaBu42xhAmQaP4KeJpw1L" \
  --target-peers "12D3KooWRkGLz4YbVmrsWK75VjFTs8NvaBu42xhAmQaP4KeJpw1L"
```

## CLI Options

- `--rate <RATE>`: Rate of DHT puts per second (default: 1)
- `--block-size <SIZE>`: Size of the data block in bytes, max 512KB (default: 524288)
- `--duration <SECONDS>`: Duration of the test in seconds, 0 for infinite (default: 0)
- `--block-count <COUNT>`: Number of unique blocks to cycle through (default: 10)
- `--network <NETWORK>`: Network to connect to (default: hex)
- `--p2p-port <PORT>`: P2P port to listen on
- `--seed <SEED>`: Seed string for libp2p keypair generation
- `--db-path <PATH>`: RocksDB store location (default: ./db)
- `--verbosity <LEVEL>`: Log verbosity level
- `--logs-json`: Output logs in JSON format
- `--target-peers <PEER_IDS>`: Comma-separated list of peer IDs to target for DHT storage
- `--manual-peers <MULTIADDRS>`: Comma-separated list of multiaddresses to add to the routing table

## Peer Targeting Options

The load test client provides two distinct options for working with specific peers:

### `--manual-peers`

- **Purpose**: Adds peer addresses to your local Kademlia routing table
- **Format**: Comma-separated list of full multiaddresses including peer IDs
- **Example**: `/ip4/192.168.1.100/tcp/30333/p2p/12D3KooWRkGLz4YbVmrsWK75VjFTs8NvaBu42xhAmQaP4KeJpw1L`
- **Effect**: These peers are added to your node's routing table, making them discoverable and reachable for DHT operations
- **Use case**: When you want to ensure your node knows about specific peers that it might not discover through bootstrap nodes

### `--target-peers`

- **Purpose**: Specifies which peers to directly send DHT records to
- **Format**: Comma-separated list of peer IDs only (without network addresses)
- **Example**: `12D3KooWRkGLz4YbVmrsWK75VjFTs8NvaBu42xhAmQaP4KeJpw1L,12D3KooWBmFN8Dh3irQvpZi4LJWqg5RPmFKPPMqH3V5aUtxezFEJ`
- **Effect**: When storing data in the DHT, records are sent directly to these specific peers instead of using the standard Kademlia algorithm
- **Use case**: For testing specific nodes or ensuring certain peers receive the data

### Key Differences

1. **Network vs Storage**: `--manual-peers` affects network connectivity, while `--target-peers` affects where data is stored
2. **Standard vs Non-standard**: `--manual-peers` uses standard Kademlia operations, while `--target-peers` uses a non-standard direct storage method
3. **Format**: `--manual-peers` requires full network addresses, while `--target-peers` only needs peer IDs

### Combined Usage

You can use both options together for comprehensive testing:

1. Use `--manual-peers` to ensure connectivity to specific nodes
2. Use `--target-peers` to direct DHT storage to those (or other) specific nodes

This is particularly useful for isolated testing environments or when evaluating specific node performance.

## Statistics

The client reports statistics every 10 seconds including:

- Total blocks sent
- Total cells sent
- Error count
- Blocks per second
- Cells per second

## Implementation Details

- Pre-generates a configurable number of blocks on startup
- Each block is divided into cells (80B for legacy grid, 2KB for multi-proof grid)
- Cycles through pre-generated blocks to maintain consistent load
- Uses the same DHT insertion mechanism as the fat client
- Supports both single-cell and multi-proof cell configurations
