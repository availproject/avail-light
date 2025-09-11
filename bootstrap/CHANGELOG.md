# Changelog

## [0.5.5]

- Set autonat to enabled, flag external_address parameter as mandatory
- Update `avail-light-core` to 1.2.11

## [0.5.4](https://github.com/availproject/avail-light/releases/tag/avail-light-bootstrap-v0.5.4) - 2025-09-08

- Update `avail-light-core` to 1.2.10

## [0.5.3](https://github.com/availproject/avail-light/releases/tag/avail-light-bootstrap-v0.5.3) - 2025-07-18

- Fixed bootstrap logging filter
- Update `avail-light-core` to 1.2.9

## [0.5.2](https://github.com/availproject/avail-light/releases/tag/avail-light-bootstrap-v0.5.2) - 2025-04-09

- Update `avail-light-core` to 1.2.6

## [0.5.1](https://github.com/availproject/avail-light/releases/tag/avail-light-bootstrap-v0.5.1) - 2025-03-06

- Update `avail-light-core` to 1.2.3

## [0.5.0](https://github.com/availproject/avail-light/releases/tag/avail-light-bootstrap-v0.5.0) - 2025-01-23

- Use telemetry from the avail-light-core 1.2.0

## [0.4.2](https://github.com/availproject/avail-light/releases/tag/avail-light-bootstrap-v0.4.2) - 2024-12-20

- Temporary remove WebRTC support to reduce memory usage

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
