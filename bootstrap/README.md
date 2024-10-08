<div align="Center">
<h1>avail-light-bootstrap</h1>
<h3>Bootstrap Node for the Avail blockchain Light client</h3>
</div>

<br>

## Introduction

`avail-light-bootstrap` is a Bootstrap node for Avail Light Client. Bootstraps act as the initial point of contact for other Avail clients, helping them discover other peers in the network.

These network entry points allow other newly joined nodes to discover new peers and to connect to them.

To start a Bootstrap node, run:

```bash
cargo run -- -c config.yaml
```

## Config reference

```yaml
# Bootstrap HTTP server host name (default: 127.0.0.1)
http_server_host = "127.0.0.1"
# Bootstrap HTTP server port (default: 7700).
http_server_port = 7700
# Set the Log Level
log_level = "info"
# If set to true, logs are displayed in JSON format, which is used for structured logging. Otherwise, plain text format is used (default: false).
log_format_json = false
# Secret key used to generate keypair. Can be either set to `seed` or to `key`. (default: seed="1")
# If set to seed, keypair will be generated from that seed.
# If set to key, a valid ed25519 private key must be provided, else the client will fail
# If `secret_key` is not set, random seed will be used.
# Default bootstrap peerID is 12D3KooWStAKPADXqJ7cngPYXd2mSANpdgh1xQ34aouufHA2xShz
secret_key = { seed="1" }
# P2P service port (default: 39000).
port = 39000
# Sets application-specific version of the protocol family used by the peer. (default: "/avail_kad/id/1.0.0")
identify_protocol = "/avail_kad/id/1.0.0"
# Sets agent version that is sent to peers. (default: "avail-light-client/rust-client")
identify_agent = "avail-light-client/rust-client"
# Sets the amount of time to keep Kademlia connections alive when they're idle. (default: 30s).
kad_connection_idle_timeout = 30
# Sets the timeout for a single Kademlia query. (default: 60s).
kad_query_timeout = 60
# OpenTelemetry Collector endpoint (default: `http://otelcollector.avail.tools:4317`)
ot_collector_endpoint = "http://otelcollector.avail.tools:4317"
# Defines a period of time in which periodic metric network dump events will be repeated. (default: 15s)
metrics_network_dump_interval = 15
# Defines a period of time in which periodic bootstraps will be repeated. (default: 300s)
bootstrap_period = 300
```
