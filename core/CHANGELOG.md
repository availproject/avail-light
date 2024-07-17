# Changelog

## [1.11.0]

- Persist generated P2P keypair in database
- Add `client_id` and `execution_id` to metric attributes
- Add `client_id` and rename `id` to `execution_id` in JSON logs
- Add `--avail-path` CLI parameter
- Add `operation-mode` to v2 diagnostics API
- Add `multiaddress` attribute to metrics
- Enable `kademlia-rocksdb` by default
- Update operating mode metrics attribute on kademlia mode change
- Fix storing the state of the verified data range
- Re-enable automatic Kademlia mode switching from client to server if the pre-requisites are met

## [1.10.2](https://github.com/availproject/avail-light/releases/tag/v1.10.1) - 2024-06-28

- Prevent light clients in Kademlia client mode from getting added to routing table at mDNS step.
- Fix a connectivity bug that caused peers to be removed from the routing table on connection error

## [1.10.1](https://github.com/availproject/avail-light/releases/tag/v1.10.1) - 2024-06-26

- Add peer multiaddress endpoint
- Replace logic behind adding new peers in the Identify protocol handler to use Kademlia protocol name string instead of Identify agent string

## [1.10.0](https://github.com/availproject/avail-light/releases/tag/v1.10.0) - 2024-06-24

- Application wide state is now persisted and not being kept in heap
- Persistence failures are handled within specific implementation
- Don't fail on any failed signature in the justification, only if there is no supermajority of valid signatures. Log the failed signature details.
- Add `run id` to the logs, unique per run and generated on startup, if the log format is JSON
- Fixed initialization of the `avail.light.up` counter

## [1.9.2](https://github.com/availproject/avail-light/releases/tag/v1.9.2) - 2024-06-20

- Change `avail.light.up` metric type to counter
- Add `--logs-json` CLI flag
- Change the way peer counting is done and expose it through the P2P diagnostic API. Add the count of peers with external addresses.
- Add `--block-matrix-partition` CLI parameter
- Introduce network name to metrics
- Support enforcing minimum protocol version for agents on p2p network
- Fix default configuration for http_server_port

## [1.9.1](https://github.com/availproject/avail-light/releases/tag/v1.9.1) - 2024-06-10

- Add `hex` network support to the `--network` CLI parameter
- Introduce `avail.light` namespace to the metrics
- Postpone flushing aggregated counters to maintenance step

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
