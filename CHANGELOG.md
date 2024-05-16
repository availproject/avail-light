# Changelog

## v1.9.0

- Deprecate `--avail-passphrase` CLI parameter and `avail_secret_seed_phrase` configuration parameter and introduce `--avail-suri` and `avail_secret_uri` alternatives.
- Introduce `incomplete` block status for blocks without data transactions or for blocks lacking commitments due to a failure
- Add first two endpoints of a new API for the P2P diagnostics

## [v1.8.1](https://github.com/availproject/avail-light/releases/tag/v1.8.1) - 2024-04-26

### Added

- Support persistent Kademlia DHT under the `kademlia-rocksdb` feature toggle
