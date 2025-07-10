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
