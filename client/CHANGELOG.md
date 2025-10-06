# Changelog

## [1.13.3]

- Added log to tracing adapter
- Updated `avail-light-core` to 1.2.12

## [1.13.2](https://github.com/availproject/avail-light/releases/tag/avail-light-client-v1.13.2) - 2025-09-15

- Add --external-address CLI parameter for use on servers without autonat
- Update `avail-light-core` to 1.2.11

## [1.13.1](https://github.com/availproject/avail-light/releases/tag/avail-light-client-v1.13.1) - 2025-09-08

- Update `avail-light-core` to 1.2.10

## [1.13.0](https://github.com/availproject/avail-light/releases/tag/avail-light-client-v1.13.0) - 2025-07-21

- Disabled DHT put
- Added new configs for AutoNAT service mode behaviour
- Updated `avail-light-core` to 1.2.9

## [1.12.13](https://github.com/availproject/avail-light/releases/tag/avail-light-client-v1.12.13) - 2025-05-30

- Skiped inserts into the DHT when client is permanently in server mode
- Fixed issue with restarting in docker on linux hosts
- Exposed network mode CLI option
- Exposed random restarts configuration (`false` by default)
- Updated `avail-light-core` to 1.2.8

## [1.12.12](https://github.com/availproject/avail-light/releases/tag/avail-light-client-v1.12.12) - 2025-05-12

- Counting initialized and switched RPC host connections
- Update `avail-light-core` to 1.2.7

## [1.12.11](https://github.com/availproject/avail-light/releases/tag/avail-light-client-v1.12.11) - 2025-04-09

- Add `no-update` flag to prevent auto-updates
- Add support for `operator_address` CLI parameter exported to the LC tracker
- Update `avail-light-core` to 1.2.6

## [1.12.10](https://github.com/availproject/avail-light/releases/tag/avail-light-client-v1.12.10) - 2025-03-21

- Update `avail-light-core` to 1.2.5

## [1.12.9](https://github.com/availproject/avail-light/releases/tag/avail-light-client-v1.12.9) - 2025-03-17

- Stop light client if update is available
- Update `avail-light-core` to 1.2.4

## [1.12.8](https://github.com/availproject/avail-light/releases/tag/avail-light-client-v1.12.8) - 2025-03-06

- Update `avail-light-core` to 1.2.3

## [1.12.7](https://github.com/availproject/avail-light/releases/tag/avail-light-client-v1.12.7) - 2025-02-20

- Add support for the Light Client Tracking Service
- Update `avail-light-core` to 1.2.2

## [1.12.6](https://github.com/availproject/avail-light/releases/tag/avail-light-client-v1.12.6) - 2025-02-10

- Update `avail-light-core` to 1.2.1

## [1.12.5](https://github.com/availproject/avail-light/releases/tag/avail-light-client-v1.12.5) - 2025-01-23

- Update `avail-light-core` to 1.2.0
- Remove `ot_flush_block_interval` from configuration

## [1.12.4](https://github.com/availproject/avail-light/releases/tag/avail-light-client-v1.12.4) - 2024-12-20

- Update `avail-light-core` to 1.1.0

## [1.12.3](https://github.com/availproject/avail-light/releases/tag/avail-light-client-v1.12.3) - 2024-11-29

- Update `avail-light-core` to 1.0.5

## [1.12.2](https://github.com/availproject/avail-light/releases/tag/avail-light-client-v1.12.2) - 2024-11-05

- Update `avail-light-core` to 1.0.4
- Add back origin attribute to metrics

## [1.12.1](https://github.com/availproject/avail-light/releases/tag/avail-light-client-v1.12.1) - 2024-10-22

- Update avail-light-core to 1.0.3

## [1.12.0](https://github.com/availproject/avail-light/releases/tag/avail-light-client-v1.12.0) - 2024-09-27

- Log version and commit hash on startup
- Switch to `avail-rust`
- Move `crawl` feature to separate bin
- Binary name changed from `avail-light` to `avail-light-client`
- Core changes are enumerated in [`core` CHANGELOG](../core/CHANGELOG.md) under the version 1.12.0

## [1.11.1](https://github.com/availproject/avail-light/releases/tag/avail-light-client-v1.11.1) - 2024-07-23

- Release changes are enumerated in [`core` CHANGELOG](../core/CHANGELOG.md) under the version 1.11.1

## [1.11.0](https://github.com/availproject/avail-light/releases/tag/avail-light-client-v1.11.0) - 2024-07-18

- Release changes are enumerated in [`core` CHANGELOG](../core/CHANGELOG.md) under the version 1.11.0
