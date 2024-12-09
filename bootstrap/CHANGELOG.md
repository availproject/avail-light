# Changelog

## [0.4.1](https://github.com/availproject/avail-light/releases/tag/avail-light-bootstrap-v0.4.1) - 2024-11-29

- Update opentelemetry sdk version
- Decrease connection idle timeout to 10s

## [0.4.0](https://github.com/availproject/avail-light/releases/tag/avail-light-bootstrap-v0.4.0) - 2024-11-05

- Fix missing swarm config
- Integrate upstream `rust-libp2p` `0.54` changes to the bootstrap process
- Add `/p2p/local/info` endpoint
- Add webrtc support to bootstrap

## [0.3.0](https://github.com/availproject/avail-light/releases/tag/avail-light-bootstrap-v0.3.0) - 2024-09-27

- Bump `otel` version to `0.24.0`

## [0.2.0](https://github.com/availproject/avail-light/releases/tag/avail-light-bootstrap-v0.2.0) - 2024-08-01

- Added new configuration parameter `bootstraps` which explicitly adds a list of known listen address of provided bootstraps to the routing table
