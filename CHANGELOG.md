# Changelog

## [1.9.1](https://github.com/availproject/avail-light/releases/tag/v1.9.1) - 2024-06-10

- Add `hex` network support to the `--network` CLI parameter
- Introduce `avail.light` namespace to the metrics
- Postpone flushing aggregated counters to maintanence step

## [v1.9.0](https://github.com/availproject/avail-light/releases/tag/v1.9.0) - 2024-06-04

- Add metric aggregation on client side in order to decrease the telemetry server load
- Add `avail.light.starts` metric counter which allows measuring number of restarts
- Add `http_server_port` CLI parameter for setting the http server port. Default HTTP server port set to 7007
- Optimize expired kademlia records from RocksDB using compaction phase for removal
- Add first two endpoints of a new API for the P2P diagnostics
- Deprecate `--avail-passphrase` CLI parameter and `avail_secret_seed_phrase` configuration parameter and introduce `--avail-suri` and `avail_secret_uri` alternatives
- Introduce `incomplete` block status for blocks without data transactions or for blocks lacking commitments due to a failure

## [v1.8.1](https://github.com/availproject/avail-light/releases/tag/v1.8.1) - 2024-04-26

### Added

- Support persistent Kademlia DHT under the `kademlia-rocksdb` feature toggle
