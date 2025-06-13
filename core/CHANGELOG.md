# Changelog

## [1.2.9]

- Update avail-rust, kate and kate-recovery to latest versions
- Check previous and next validator set in case when current fails
- Moved computation of dimensions to block verified
- Added p2p client restart capability
- Added polynomial multiproof features to core
- Fixed issue with publication and replication intervals in None case
- Removed dusk-plonk dependencies from core
- Behaviour struct can now be pre-configured based on the client needs

## [1.2.8](https://github.com/availproject/avail-light/tree/avail-light-core-v1.2.8) - 2025-05-30

- Added flag to disable inserts into DHT on failed fetch
- Improved updater logging
- Increase default `ping` protocol interval from 20 to 60 seconds
- Added network mode configuration option that can disable the use of RCP or P2P cell fetching
- Made replicaton and publicaton intervals optional
- Added configurable random restarts to the maintenance phase
- Exposed additional Identify protocol configuration, increased the timeouts between requests
- Exposed additional AutoNat configurations, increased AutoNat server throttling
- Allow insecure URLs to be used when creating RPC client

## [1.2.7](https://github.com/availproject/avail-light/tree/avail-light-core-v1.2.7) - 2025-05-12

- Changed the default AutoNat `only_global_ips` from `false` to `true` so the servers (and clients) discard AutoNat probes
  from the private-only peers
- Updated the default AutoNat configuration with increased timeouts
- Added counters for initial and switched RPC host connections to telemetry metrics
- Added put records counter to telemetry metrics
- Removed retry strategy from the Subxt Avail client
- Added RPC client which doesn't subscribe to finalized headers
- Added kbucket pending timeout to kademlia configuration

## [1.2.6](https://github.com/availproject/avail-light/tree/avail-light-core-v1.2.6) - 2025-04-09

- Removed the mDNS behaviour because of the issue: [Bug: Arithmetic overflow](https://github.com/libp2p/rust-libp2p/issues/5943)
- Download and restart avail light client when update is available

## [1.2.5](https://github.com/availproject/avail-light/tree/avail-light-core-v1.2.5) - 2025-03-21

- Return correct result on some RPC functions

## [1.2.4](https://github.com/availproject/avail-light/tree/avail-light-core-v1.2.4) - 2025-03-17

- Update avail-rust dependency to v0.1.5
- Check for avail light client updates on github

## [1.2.3](https://github.com/availproject/avail-light/tree/avail-light-core-v1.2.3) - 2025-03-06

- Update `rust-libp2p` and related libraries to `0.55`

## [1.2.2](https://github.com/availproject/avail-light/tree/avail-light-core-v1.2.2) - 2025-02-20

- Support v2 API in WASM
- Update avail-rust dependency to v0.1.0

## [1.2.1](https://github.com/availproject/avail-light/tree/avail-light-core-v1.2.1) - 2025-02-10

- Batch rows RPC requests
- Remove UPNP behaviour

## [1.2.0](https://github.com/availproject/avail-light/tree/avail-light-core-v1.2.0) - 2025-02-06

- Remove multiaddress metric attribute
- Add timestamp to v2 header API
- Increase maximum kademlia record size to allow row with 512 cells
- Fix issue with multiple telemetry gauge callbacks

## [1.1.0](https://github.com/availproject/avail-light/tree/avail-light-core-v1.1.0) - 2024-12-20

- Temporary remove WebRTC support to reduce memory usage
- Update opentelemetry sdk version to 0.27.1
- Enable WASM compilation of the network, light_client and related mods

## [1.0.5](https://github.com/availproject/avail-light/tree/avail-light-core-v1.0.5) - 2024-11-29

- Rename `MultiaddrConfig` type to `PeerAddress` for better clarity
- Enable WASM compilation on proof mod
- Enable WASM compilation on utils and shutdown mods
- Allocate new port on each new dial attempt
- Set different dial conditions for bootstrap process and diagnostics API
- Move p2p diagnostics APIs to its own module
- Decrease connection idle timeout to 10s
- Enforce project name as its own distinct type
- Update `avail-rust` to the latest version (WASM compatibility updates)

## 1.0.4

- Enforce alphanumeric and snake case constraints on otel initialization handler
- Fix put record metric event in case of errors
- Fix telemetry naming, put project name as prefix instead of suffix
- Add timeouts to RPC subscriptions
- Integrate upstream `rust-libp2p` `0.54` changes to the bootstrap process

## 1.0.3

- Return current node version on the API instead of preconfigured
- Remove node version check
- Fix skipping of only available node during retries in case of temporary failure
- Increase maximum retry delay to 10 seconds

# Changelog (pre-monorepo)

## 1.12.0

- Add `nonce` management to the light client
- Update `libp2p` to v0.54
- Add `WebRTC` listener with config parameter and CLI option for port setting
- Remove old crate patches that were required when `subxt` was directly used
- Dial peer at mdns step to trigger identify
- Update expected system version to 2.2
- Add swarm event counter as a new metric - `avail.light.event_loop_event`
- Add digest to v2 header API
- Bump `otel` version to `0.24.0`
- Introduce public address filter for external addresses and add additional log entry
- Refactor the `/peers/get-multiaddress` endpoint so that it returns all of the peers multi-addresses
- Fix `operating_mode` metric attribute not switching properly when Kademlia mode changes

## 1.11.1

- Add `mainnet` CLI option for connecting to Avail mainnet
- Add `--client-alias` CLI parameter and `client-alias` config parameter for setting human readable alias of the client

## 1.11.0

- Return empty data on `v2/blocks/{block_number}/data` endpoint in case of `incomplete` blocks
- Remove deprecation notice from `--avail-passphrase` and fix `--avail-suri` CLI parameter usage
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
