//! Shared light client structs and enums.

use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use anyhow::Context;
use avail_subxt::api::runtime_types::da_primitives::header::extension::HeaderExtension;
use avail_subxt::api::runtime_types::da_primitives::kate_commitment::KateCommitment;
use avail_subxt::primitives::Header as DaHeader;
use codec::Encode;
use kate_recovery::commitments;
use kate_recovery::matrix::Dimensions;
use kate_recovery::{index::AppDataIndex, matrix::Partition};
use libp2p::Multiaddr;
use serde::{Deserialize, Serialize};
use sp_core::{blake2_256, H256};

const CELL_SIZE: usize = 32;
const PROOF_SIZE: usize = 48;
pub const CELL_WITH_PROOF_SIZE: usize = CELL_SIZE + PROOF_SIZE;

#[derive(Serialize, Deserialize, Debug)]
pub struct RuntimeVersionResult {
	apis: Vec<(String, u32)>,
	#[serde(rename = "authoringVersion")]
	authoring_version: u32,
	#[serde(rename = "implName")]
	impl_name: String,
	#[serde(rename = "implVersion")]
	pub impl_version: u32,
	#[serde(rename = "specName")]
	pub spec_name: String,
	#[serde(rename = "specVersion")]
	pub spec_version: u32,
	#[serde(rename = "transactionVersion")]
	transaction_version: u32,
}

/// Light to app client channel message struct
pub struct ClientMsg {
	pub header_hash: H256,
	pub block_num: u32,
	pub dimensions: Dimensions,
	pub lookup: AppDataIndex,
	pub commitments: Vec<[u8; 48]>,
}

impl TryFrom<DaHeader> for ClientMsg {
	type Error = anyhow::Error;
	fn try_from(header: DaHeader) -> Result<Self, Self::Error> {
		let hash: H256 = Encode::using_encoded(&header, blake2_256).into();
		let HeaderExtension::V1(xt) = header.extension;
		let KateCommitment { rows, cols, .. } = xt.commitment;

		Ok(ClientMsg {
			header_hash: hash,
			block_num: header.number,
			dimensions: Dimensions::new(rows, cols).context("Invalid dimensions")?,
			lookup: AppDataIndex {
				size: xt.app_lookup.size,
				index: xt
					.app_lookup
					.index
					.into_iter()
					.map(|e| (e.app_id.0, e.start))
					.collect(),
			},
			commitments: commitments::from_slice(&xt.commitment.commitment)?,
		})
	}
}

/// Client mode
///
/// * `LightClient` - light client is running
/// * `AppClient` - app client is running alongside the light client
#[derive(Serialize, Clone)]
pub enum Mode {
	LightClient,
	AppClient(u32),
}

impl From<Option<u32>> for Mode {
	fn from(app_id: Option<u32>) -> Self {
		match app_id {
			None => Mode::LightClient,
			Some(0) => Mode::LightClient,
			Some(app_id) => Mode::AppClient(app_id),
		}
	}
}

mod block_matrix_partition_format {
	use kate_recovery::matrix::Partition;
	use serde::{self, Deserialize, Deserializer, Serializer};

	pub fn serialize<S>(partition: &Option<Partition>, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		match partition {
			Some(partition) => {
				let Partition { fraction, number } = partition;
				let s = format!("{number}/{fraction}");
				serializer.serialize_str(&s)
			},
			None => serializer.serialize_none(),
		}
	}

	pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Partition>, D::Error>
	where
		D: Deserializer<'de>,
	{
		let s = String::deserialize(deserializer)?;
		if s.is_empty() || s.to_ascii_lowercase().contains("none") {
			return Ok(None);
		}
		let parts = s.split('/').collect::<Vec<_>>();
		if parts.len() != 2 {
			return Err(serde::de::Error::custom(format!("Invalid value {s}")));
		}
		let number = parts[0].parse::<u8>().map_err(serde::de::Error::custom)?;
		let fraction = parts[1].parse::<u8>().map_err(serde::de::Error::custom)?;
		if number != 0 {
			Ok(Some(Partition { number, fraction }))
		} else {
			Ok(None)
		}
	}
}

mod port_range_format {
	use serde::{self, Deserialize, Deserializer, Serializer};

	pub fn serialize<S>(port_range: &(u16, u16), serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		if port_range.1 == 0 || port_range.1 <= port_range.0 {
			let s = format!("{port}", port = port_range.0);
			serializer.serialize_str(&s)
		} else {
			let s = format!(
				"{port1}-{port2}",
				port1 = port_range.0,
				port2 = port_range.1
			);
			serializer.serialize_str(&s)
		}
	}

	pub fn deserialize<'de, D>(deserializer: D) -> Result<(u16, u16), D::Error>
	where
		D: Deserializer<'de>,
	{
		let s = String::deserialize(deserializer)?;
		let parts = s.split('-').collect::<Vec<_>>();
		let res = match parts.len() {
			1 => {
				let port = parts[0].parse::<u16>().map_err(serde::de::Error::custom)?;
				Ok((port, 0))
			},
			2 => {
				let port1 = parts[0].parse::<u16>().map_err(serde::de::Error::custom)?;
				let port2 = parts[1].parse::<u16>().map_err(serde::de::Error::custom)?;
				if port2 > port1 {
					Ok((port1, port2))
				} else {
					Err(serde::de::Error::custom(format!("Invalid value {s}")))
				}
			},
			_ => Err(serde::de::Error::custom(format!("Invalid value {s}"))),
		};
		res
	}
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum SecretKey {
	Seed { seed: String },
	Key { key: String },
}

/// Representation of a configuration used by this project.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct RuntimeConfig {
	/// Light client HTTP server host name (default: 127.0.0.1).
	pub http_server_host: String,
	/// Light client HTTP server port (default: 7000).
	#[serde(with = "port_range_format")]
	pub http_server_port: (u16, u16),
	/// Secret key for libp2p keypair. Can be either set to `seed` or to `key`.
	/// If set to seed, keypair will be generated from that seed.
	/// If set to key, a valid ed25519 private key must be provided, else the client will fail
	/// If `secret_key` is not set, random seed will be used.
	pub secret_key: Option<SecretKey>,
	/// Libp2p service port range (port, range) (default: 37000).
	#[serde(with = "port_range_format")]
	pub libp2p_port: (u16, u16),
	/// RPC endpoint of a full node for proof queries, etc. (default: http://127.0.0.1:9933).
	pub full_node_rpc: Vec<String>,
	/// Configures LibP2P TCP port reuse for local sockets, which implies reuse of listening ports for outgoing connections to enhance NAT traversal capabilities (default: false)
	pub libp2p_tcp_port_reuse: bool,
	/// Configures LibP2P AutoNAT behaviour to reject probes as a server for clients that are observed at a non-global ip address (default: false)
	pub libp2p_autonat_only_global_ips: bool,
	/// WebSocket endpoint of full node for subscribing to latest header, etc (default: [ws://127.0.0.1:9944]).
	pub full_node_ws: Vec<String>,
	/// ID of application used to start application client. If app_id is not set, or set to 0, application client is not started (default: 0).
	pub app_id: Option<u32>,
	/// Confidence threshold, used to calculate how many cells needs to be sampled to achieve desired confidence (default: 92.0).
	pub confidence: f64,
	/// Vector of IPFS bootstrap nodes, used to bootstrap DHT. If not set, light client acts as a bootstrap node, waiting for first peer to connect for DHT bootstrap (default: empty).
	pub bootstraps: Vec<(String, Multiaddr)>,
	/// File system path where RocksDB used by light client, stores its data.
	pub avail_path: String,
	/// Log level, default is `INFO`. See `<https://docs.rs/log/0.4.14/log/enum.LevelFilter.html>` for possible log level values. (default: `INFO`).
	pub log_level: String,
	/// If set to true, logs are displayed in JSON format, which is used for structured logging. Otherwise, plain text format is used (default: false).
	pub log_format_json: bool,
	/// Prometheus service port, used for emitting metrics to prometheus server. (default: 9520).
	pub prometheus_port: Option<u16>,
	/// Disables fetching of cells from RPC, set to true if client expects cells to be available in DHT (default: false).
	pub disable_rpc: bool,
	/// Disables proof verification in general, if set to true, otherwise proof verification is performed. (default: false).
	pub disable_proof_verification: bool,
	/// Maximum number of parallel tasks spawned for GET and PUT operations on DHT (default: 20).
	pub dht_parallelization_limit: usize,
	/// Number of parallel queries for cell fetching via RPC from node (default: 8).
	pub query_proof_rpc_parallel_tasks: usize,
	/// Number of seconds to postpone block processing after block finalized message arrives (default: 0).
	pub block_processing_delay: Option<u32>,
	/// Fraction and number of the block matrix part to fetch (e.g. 2/20 means second 1/20 part of a matrix) (default: None)
	#[serde(with = "block_matrix_partition_format")]
	pub block_matrix_partition: Option<Partition>,
	/// How many blocks behind latest block to sync. If parameter is empty, or set to 0, synching is disabled (default: 0).
	pub sync_blocks_depth: Option<u32>,
	/// Maximum number of cells per request for proof queries (default: 30).
	pub max_cells_per_rpc: Option<usize>,
	/// Threshold for the number of cells fetched via DHT for the app client (default: 5000)
	pub threshold: usize,

	/// Kademlia configuration - WARNING: Changing the default values might cause the peer to suffer poor performance!
	/// Default Kademlia config values have been copied from rust-libp2p Kademila defaults
	///
	/// Time-to-live for DHT entries in seconds (default: 24h).
	/// Default value is set for light clients. Due to the heavy duty nature of the fat clients, it is recommended to be set far bellow this
	/// value - not greater than 1hr.
	/// Record TTL, publication and replication intervals are co-dependent, meaning that TTL >> publication_interval >> replication_interval.
	pub kad_record_ttl: u64,
	/// Sets the (re-)publication interval of stored records in seconds. (default: 12h).
	/// Default value is set for light clients. Fat client value needs to be inferred from the TTL value.
	/// This interval should be significantly shorter than the record TTL, to ensure records do not expire prematurely.
	pub publication_interval: u32,
	/// Sets the (re-)replication interval for stored records in seconds. (default: 3h).
	/// Default value is set for light clients. Fat client value needs to be inferred from the TTL and publication interval values.
	/// This interval should be significantly shorter than the publication interval, to ensure persistence between re-publications.
	pub replication_interval: u32,
	/// The replication factor determines to how many closest peers a record is replicated. (default: 20).
	pub replication_factor: u16,
	/// Sets the amount of time to keep connections alive when they're idle. (default: 30s).
	/// NOTE: libp2p default value is 10s, but because of Avail block time of 20s the value has been increased
	pub connection_idle_timeout: u32,
	/// Sets the timeout for a single Kademlia query. (default: 60s).
	pub query_timeout: u32,
	/// Sets the allowed level of parallelism for iterative Kademlia queries. (default: 3).
	pub query_parallelism: u16,
	/// Sets the Kademlia caching strategy to use for successful lookups. (default: 1).
	/// If set to 0, caching is disabled.
	pub caching_max_peers: u16,
	/// Require iterative queries to use disjoint paths for increased resiliency in the presence of potentially adversarial nodes. (default: false).
	pub disjoint_query_paths: bool,
	/// The maximum number of records. (default: 2400000).
	/// The default value has been calculated to sustain ~1hr worth of cells, in case of blocks with max sizes being produces in 20s block time for fat clients
	/// (256x512) * 3 * 60
	pub max_kad_record_number: u64,
	/// The maximum size of record values, in bytes. (default: 8096).
	pub max_kad_record_size: u64,
	/// The maximum number of provider records for which the local node is the provider. (default: 1024).
	pub max_kad_provided_keys: u64,
}

pub struct Delay(Option<Duration>);

/// Light client configuration (see [RuntimeConfig] for details)
pub struct LightClientConfig {
	pub full_node_ws: Vec<String>,
	pub confidence: f64,
	pub disable_rpc: bool,
	pub dht_parallelization_limit: usize,
	pub query_proof_rpc_parallel_tasks: usize,
	pub block_processing_delay: Delay,
	pub block_matrix_partition: Option<Partition>,
	pub disable_proof_verification: bool,
	pub max_cells_per_rpc: usize,
	pub ttl: u64,
}

fn delay_for(target: Duration, elapsed: Duration) -> Option<Duration> {
	(target > elapsed).then(|| target - elapsed)
}

impl Delay {
	pub fn sleep_duration(&self, from: Instant) -> Option<Duration> {
		delay_for(self.0?, from.elapsed())
	}
}

impl From<&RuntimeConfig> for LightClientConfig {
	fn from(val: &RuntimeConfig) -> Self {
		let block_processing_delay = val
			.block_processing_delay
			.map(|v| Duration::from_secs(v.into()));

		LightClientConfig {
			full_node_ws: val.full_node_ws.clone(),
			confidence: val.confidence,
			disable_rpc: val.disable_rpc,
			dht_parallelization_limit: val.dht_parallelization_limit,
			query_proof_rpc_parallel_tasks: val.query_proof_rpc_parallel_tasks,
			block_processing_delay: Delay(block_processing_delay),
			block_matrix_partition: val.block_matrix_partition.clone(),
			disable_proof_verification: val.disable_proof_verification,
			max_cells_per_rpc: val.max_cells_per_rpc.unwrap_or(30),
			ttl: val.kad_record_ttl,
		}
	}
}

pub struct LibP2PConfig {
	pub libp2p_secret_key: Option<SecretKey>,
	pub libp2p_port: (u16, u16),
	pub libp2p_tcp_port_reuse: bool,
	pub libp2p_autonat_only_global_ips: bool,
	pub kademlia: KademliaConfig,
}

impl From<&RuntimeConfig> for LibP2PConfig {
	fn from(rtcfg: &RuntimeConfig) -> Self {
		Self {
			libp2p_secret_key: rtcfg.secret_key.clone(),
			libp2p_port: rtcfg.libp2p_port,
			libp2p_tcp_port_reuse: rtcfg.libp2p_tcp_port_reuse,
			libp2p_autonat_only_global_ips: rtcfg.libp2p_autonat_only_global_ips,
			kademlia: rtcfg.into(),
		}
	}
}

/// Kademlia configuration (see [RuntimeConfig] for details)
pub struct KademliaConfig {
	pub record_ttl: u64,
	pub record_replication_factor: NonZeroUsize,
	pub record_replication_interval: Option<Duration>,
	pub publication_interval: Option<Duration>,
	pub connection_idle_timeout: Duration,
	pub query_timeout: Duration,
	pub query_parallelism: NonZeroUsize,
	pub caching_max_peers: u16,
	pub disjoint_query_paths: bool,
	pub max_kad_record_number: usize,
	pub max_kad_record_size: usize,
	pub max_kad_provided_keys: usize,
}

impl From<&RuntimeConfig> for KademliaConfig {
	fn from(val: &RuntimeConfig) -> Self {
		KademliaConfig {
			record_ttl: val.kad_record_ttl,
			record_replication_factor: std::num::NonZeroUsize::new(val.replication_factor as usize)
				.expect("Invalid replication factor"),
			record_replication_interval: Some(Duration::from_secs(val.replication_interval.into())),
			publication_interval: Some(Duration::from_secs(val.publication_interval.into())),
			connection_idle_timeout: Duration::from_secs(val.connection_idle_timeout.into()),
			query_timeout: Duration::from_secs(val.query_timeout.into()),
			query_parallelism: std::num::NonZeroUsize::new(val.query_parallelism as usize)
				.expect("Invalid query parallelism value"),
			caching_max_peers: val.caching_max_peers,
			disjoint_query_paths: val.disjoint_query_paths,
			max_kad_record_number: val.max_kad_record_number as usize,
			max_kad_record_size: val.max_kad_record_size as usize,
			max_kad_provided_keys: val.max_kad_provided_keys as usize,
		}
	}
}

/// Sync client configuration (see [RuntimeConfig] for details)
pub struct SyncClientConfig {
	pub confidence: f64,
	pub disable_rpc: bool,
	pub dht_parallelization_limit: usize,
	pub ttl: u64,
}

impl From<&RuntimeConfig> for SyncClientConfig {
	fn from(val: &RuntimeConfig) -> Self {
		SyncClientConfig {
			confidence: val.confidence,
			disable_rpc: val.disable_rpc,
			dht_parallelization_limit: val.dht_parallelization_limit,
			ttl: val.kad_record_ttl,
		}
	}
}

/// App client configuration (see [RuntimeConfig] for details)
pub struct AppClientConfig {
	pub dht_parallelization_limit: usize,
	pub disable_rpc: bool,
	pub threshold: usize,
}

impl From<&RuntimeConfig> for AppClientConfig {
	fn from(val: &RuntimeConfig) -> Self {
		AppClientConfig {
			dht_parallelization_limit: val.dht_parallelization_limit,
			disable_rpc: val.disable_rpc,
			threshold: val.threshold,
		}
	}
}

impl Default for RuntimeConfig {
	fn default() -> Self {
		RuntimeConfig {
			http_server_host: "127.0.0.1".to_owned(),
			http_server_port: (7000, 0),
			libp2p_port: (37000, 0),
			secret_key: None,
			libp2p_tcp_port_reuse: false,
			libp2p_autonat_only_global_ips: false,
			full_node_rpc: vec!["http://127.0.0.1:9933".to_owned()],
			full_node_ws: vec!["ws://127.0.0.1:9944".to_owned()],
			app_id: None,
			confidence: 92.0,
			bootstraps: Vec::new(),
			avail_path: "avail_path".to_owned(),
			log_level: "INFO".to_owned(),
			log_format_json: false,
			prometheus_port: Some(9520),
			disable_rpc: false,
			disable_proof_verification: false,
			dht_parallelization_limit: 20,
			query_proof_rpc_parallel_tasks: 8,
			block_processing_delay: None,
			block_matrix_partition: None,
			sync_blocks_depth: None,
			max_cells_per_rpc: Some(30),
			kad_record_ttl: 24 * 60 * 60,
			threshold: 5000,
			replication_factor: 20,
			publication_interval: 12 * 60 * 60,
			replication_interval: 3 * 60 * 60,
			connection_idle_timeout: 30,
			query_timeout: 60,
			query_parallelism: 3,
			caching_max_peers: 1,
			disjoint_query_paths: false,
			max_kad_record_number: 2400000,
			max_kad_record_size: 8096,
			max_kad_provided_keys: 1024,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::delay_for;
	use std::time::Duration;
	use test_case::test_case;

	#[test_case(0 , 0 , None ; "No delay expected")]
	#[test_case(1 , 0 , Some(1) ; "One second delay expected")]
	#[test_case(5 , 1 , Some(4) ; "Four seconds delay expected")]
	#[test_case(1 , 5 , None ; "Delay time is elapsed, no delay expected")]
	fn test_delay_for(target: u64, elapsed: u64, expected: Option<u64>) {
		let target = Duration::from_secs(target);
		let elapsed = Duration::from_secs(elapsed);
		let expected = expected.map(Duration::from_secs);
		assert_eq!(delay_for(target, elapsed), expected);
	}
}
