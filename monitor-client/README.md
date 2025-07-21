# Avail Light Monitor Client

The Avail Light Monitor Client is a specialized tool designed to monitor the health and connectivity of peers in the Avail network. It continuously tracks peer availability, measures network latency, and provides comprehensive metrics for network analysis.

## Overview

The monitor client performs several key functions:

1. **Peer Discovery** - Continuously discovers new peers in the DHT network
2. **Bootstrap Monitoring** - Monitors the availability of bootstrap nodes
3. **Peer Health Monitoring** - Tracks the health status of discovered peers
4. **Metrics Collection** - Exports detailed metrics via OpenTelemetry

## Architecture

The monitor consists of three main components:

### 1. Bootstrap Monitor

- Periodically dials bootstrap nodes to ensure they remain accessible
- Tracks success/failure rates of bootstrap connections
- Runs on a configurable interval (default: 60 seconds)

### 2. Peer Discovery

- Uses Kademlia DHT queries to discover new peers in the network
- Queries random peer IDs to explore different parts of the DHT
- Tracks the total number of discovered peers

### 3. Peer Monitor

- Monitors the health of discovered peers by attempting to dial them
- Calculates health scores based on the last 20 connection attempts
- Measures ping latency (RTT) for each peer
- Uses time-based thresholds for blacklisting decisions

## Configuration

The monitor client can be configured through command-line arguments or configuration file:

```yaml
# LibP2P listener port
port: 37000

# REST API port
http_port: 8090

# Monitoring intervals (in seconds)
bootstrap_interval: 60      # How often to check bootstrap nodes
peer_monitor_interval: 30   # How often to monitor peer health
peer_discovery_interval: 10 # How often to discover new peers

# Blacklist duration (in hours)
blacklist_duration_hours: 6 # Duration for health score thresholds

# OpenTelemetry configuration
otel:
  ot_collector_endpoint: "http://localhost:4317"
  ot_export_period: 5      # Export metrics every 5 seconds
  ot_export_timeout: 10
```

Example run command:

```bash
./avail-light-monitor --network mainnet --http-port 51241 --blacklist-duration-hours 6 --ot-collector-endpoint http://localhost:4317 --ot-export-period 5 --ot-export-timeout 10
```

## Metrics

The monitor exports the following metrics via OpenTelemetry:

### Global Metrics

| Metric Name                                    | Type    | Description                                    |
| ---------------------------------------------- | ------- | ---------------------------------------------- |
| `avail_light_monitor_active_peers`             | Gauge   | Number of currently active (non-blocked) peers |
| `avail_light_monitor_blocked_peers`            | Gauge   | Number of blocked peers                        |
| `avail_light_monitor_discovered_peers_total`   | Counter | Total number of peers discovered since startup |
| `avail_light_monitor_bootstrap_attempts_total` | Counter | Total number of bootstrap connection attempts  |
| `avail_light_monitor_bootstrap_failures_total` | Counter | Total number of failed bootstrap connections   |

### Per-Peer Metrics

| Metric Name                                | Type  | Labels  | Description              |
| ------------------------------------------ | ----- | ------- | ------------------------ |
| `avail_light_monitor_peer_ping_latency_ms` | Gauge | peer_id | Ping RTT in milliseconds |
| `avail_light_monitor_peer_health_score`    | Gauge | peer_id | Health score (0-100)     |
| `avail_light_monitor_peer_blocked`         | Gauge | peer_id | 1 if blocked, 0 if not   |

## Peer Health Scoring

The health score is calculated based on the last 20 connection attempts:

- **Before 20 established connections**: Health score is 50% (neutral)
- **After 20 established connections**: Health score = (successful_connections / total_connections) * 100

Health score ranges:

- **100%**: Perfect health (all recent connection attempts successful)
- **50%**: Default for peers with less than 20 connection attempts
- **0-100%**: Based on success rate of the last 20 attempts

## Peer Blocking Logic

Peers are marked as blacklisted when:

1. Their health score remains below 20% for the configured duration (default: 6 hours)
2. They have no globally reachable addresses

Blacklisted peers can be removed from the blacklist by:

1. Their health score staying above 60% for the configured duration (default: 6 hours)
2. Being rediscovered with globally reachable addresses

The blacklist duration can be configured with the `--blacklist-duration-hours` parameter.

## HTTP API

The monitor exposes a REST API on the configured HTTP port:

- `GET /peers` - List all discovered peers
- `GET /peers/count` - Get count of active/blocked peers
- `GET /peers/{peer_id}` - Get details for a specific peer
- `GET /peers/blacklisted` - List all blocked peers

## Development

The monitor is built using:

- **libp2p** - P2P networking
- **OpenTelemetry** - Metrics collection
- **actix-web** - HTTP API server

Key source files:

- `src/main.rs` - Main entry point and event handling
- `src/telemetry.rs` - Metrics implementation
- `src/peer_monitor.rs` - Peer health monitoring logic
- `src/bootstrap_monitor.rs` - Bootstrap node monitoring
- `src/peer_discovery.rs` - DHT peer discovery
- `src/types.rs` - Data structures (ServerInfo, etc.)
