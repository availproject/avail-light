<div align="Center">
<h1>avail-light-relay</h1>
<h3>Relay Server Node for the Avail blockchain Light client</h3>
</div>

<br>

## Introduction

`avail-light-relay` is a server node, which utilizes Circuit Relay transport protocol.

This node routes traffic between two peers as a third-party "relay" peer.

In peer-to-peer networks, on the whole, there will come a time when peers will be unable to traverse their NAT and/or firewalls. Which makes them publicly unavailable. To face these connectivity hurdles, when a peer isn't able to listen to a public address, it can dial out to a relay peer, which will retain a long-lived open connection.

Other peers can dial through the relay peer using a `p2p-circuit` address, which forwards called for traffic to its destination.

An important aspect here is that the employed protocol is not "transparent". Both the source and the destination are aware that traffic is being relayed. This can potentially be used to construct a path from the destination back to the source.

All participants are identified using their Peer ID, including the relay node, which leads to an obvious conclusion that this protocol is not anonymous, by any means.

## Address

A relay node is identified using a multiaddr that includes the Peer ID of the peer whose traffic is being relayed (the listening peer or "relay target"). With this in mind, we could construct an address that describes a path to the source through some specific relay with selected transport:

`/ip4/198.51.100.0/tcp/55555/p2p/SomeRelay/p2p-circuit/p2p/SomeDude`

To start a Relay node, run:

```bash
cargo run -- -c config.yaml
```

## Config reference

```yaml
# Relay HTTP server host name (default: 127.0.0.1).
http_server_host = "127.0.0.1"
# Relay HTTP server port (default: 7701).
http_server_port = 7701,
# Set the Log Level
log_level = "info"
# If set to true, logs are displayed in JSON format, which is used for structured logging. Otherwise, plain text format is used (default: false).
log_format_json = false
# Secret key used to generate keypair. Can be either set to `seed` or to `key`.
# If set to seed, keypair will be generated from that seed.
# If set to key, a valid ed25519 private key must be provided, else the client will fail
# If `secret_key` is not set, random seed will be used.
secret_key = { seed="1" }
# P2P service port (default: 37000).
p2p_port = 3700
# Sets application-specific version of the protocol family used by the peer. (default: "/avail_kad/id/1.0.0")
identify_protocol = "/avail_kad/id/1.0.0"
# Sets agent version that is sent to peers. (default: "avail-light-client/rust-client")
identify_agent = "avail-light-client/rust-client"
```
