//! Shared light client structs and enums.

use crate::network::p2p::MemoryStoreConfig;
use crate::network::rpc::{Event, Node as RpcNode};
use crate::utils::{extract_app_lookup, extract_kate};
use avail_core::DataLookup;
use avail_subxt::{primitives::Header as DaHeader, utils::H256};
use bip39::{Language, Mnemonic, MnemonicType};
use clap::Parser;
use codec::{Decode, Encode};
use color_eyre::{
	eyre::{eyre, WrapErr},
	Report, Result,
};
use kate_recovery::{
	commitments,
	matrix::{Dimensions, Partition},
};
use libp2p::kad::Mode as KadMode;
use libp2p::{Multiaddr, PeerId};
use serde::{de::Error, Deserialize, Serialize};
use sp_core::crypto::Ss58Codec;
use sp_core::{blake2_256, bytes, ed25519};
use std::fmt::{self, Display, Formatter};
use std::fs;
use std::num::{NonZeroU8, NonZeroUsize};
use std::ops::Range;
use std::str::FromStr;
use std::time::{Duration, Instant};
use subxt::ext::sp_core::{sr25519::Pair, Pair as _};
use tokio::sync::broadcast;
use tokio_retry::strategy::{jitter, ExponentialBackoff, FibonacciBackoff};

const CELL_SIZE: usize = 32;
const PROOF_SIZE: usize = 48;
pub const CELL_WITH_PROOF_SIZE: usize = CELL_SIZE + PROOF_SIZE;

pub const DEV_FLAG_GENHASH: &str = "DEV";
pub const IDENTITY_PROTOCOL: &str = "/avail_kad/id/1.0.0";
pub const IDENTITY_AGENT_BASE: &str = "avail-light-client";
pub const IDENTITY_AGENT_CLIENT_TYPE: &str = "rust-client";

#[derive(Parser)]
#[command(version)]
pub struct CliOpts {
	/// Path to the yaml configuration file
	#[arg(short, long, value_name = "FILE")]
	pub config: Option<String>,
	/// Path to the toml identity file
	#[arg(short, long, value_name = "FILE", default_value = "identity.toml")]
	pub identity: String,
	/// AppID for application client
	#[arg(long, value_name = "app-id")]
	pub app_id: Option<u32>,
	/// Testnet or devnet selection
	#[arg(short, long, value_name = "network")]
	pub network: Option<Network>,
	/// Run a clean light client, deleting existing avail_path folder
	#[arg(long)]
	pub clean: bool,
	/// Enable finality sync
	#[arg(short, long, value_name = "finality_sync_enable")]
	pub finality_sync_enable: bool,
	/// P2P port
	#[arg(short, long)]
	pub port: Option<u16>,
	/// Enable websocket transport
	#[arg(long, value_name = "ws_transport_enable")]
	pub ws_transport_enable: bool,
	/// Log level
	#[arg(long)]
	pub verbosity: Option<LogLevel>,
	/// Avail secret seed phrase password
	#[arg(long)]
	pub avail_passphrase: Option<String>,
	/// Seed string for libp2p keypair generation
	#[arg(long)]
	pub seed: Option<String>,
	/// ed25519 private key for libp2p keypair generation
	#[arg(long)]
	pub private_key: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeVersion {
	apis: Vec<(String, u32)>,
	authoring_version: u32,
	impl_name: String,
	pub impl_version: u32,
	pub spec_name: String,
	pub spec_version: u32,
	transaction_version: u32,
}

/// Light to app client channel message struct
#[derive(Clone, Debug)]
pub struct BlockVerified {
	pub header_hash: H256,
	pub block_num: u32,
	pub dimensions: Dimensions,
	pub lookup: DataLookup,
	pub commitments: Vec<[u8; 48]>,
	pub confidence: Option<f64>,
}

pub struct ClientChannels {
	pub block_sender: broadcast::Sender<BlockVerified>,
	pub rpc_event_receiver: broadcast::Receiver<Event>,
}

impl TryFrom<(DaHeader, Option<f64>)> for BlockVerified {
	type Error = Report;
	fn try_from((header, confidence): (DaHeader, Option<f64>)) -> Result<Self, Self::Error> {
		let hash: H256 = Encode::using_encoded(&header, blake2_256).into();
		let enc_lookup = extract_app_lookup(&header.extension)
			.map_err(|e| eyre!("Invalid DataLookup: {}", e))?
			.encode();
		let lookup = DataLookup::decode(&mut enc_lookup.as_slice())?;
		let (rows, cols, _, commitment) = extract_kate(&header.extension);

		Ok(BlockVerified {
			header_hash: hash,
			block_num: header.number,
			dimensions: Dimensions::new(rows, cols).ok_or_else(|| eyre!("Invalid dimensions"))?,
			lookup,
			commitments: commitments::from_slice(&commitment)?,
			confidence,
		})
	}
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(try_from = "String")]
pub enum KademliaMode {
	Client,
	Server,
}

impl From<KademliaMode> for KadMode {
	fn from(value: KademliaMode) -> Self {
		match value {
			KademliaMode::Client => KadMode::Client,
			KademliaMode::Server => KadMode::Server,
		}
	}
}

impl Display for KademliaMode {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		match self {
			KademliaMode::Client => write!(f, "client"),
			KademliaMode::Server => write!(f, "server"),
		}
	}
}

impl TryFrom<String> for KademliaMode {
	type Error = color_eyre::Report;

	fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
		match value.to_lowercase().as_str() {
			"client" => Ok(KademliaMode::Client),
			"server" => Ok(KademliaMode::Server),
			_ => Err(eyre!(
				"Wrong Kademlia mode. Expecting 'client' or 'server'."
			)),
		}
	}
}

/// Client mode
///
/// * `LightClient` - light client is running
/// * `AppClient` - app client is running alongside the light client
#[derive(Serialize, Deserialize, Debug, Clone)]
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

pub mod block_matrix_partition_format {
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
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(try_from = "String")]
pub struct CompactMultiaddress((PeerId, Multiaddr));

impl TryFrom<String> for CompactMultiaddress {
	type Error = Report;

	fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
		let Some((_, peer_id)) = value.rsplit_once('/') else {
			return Err(eyre!("Invalid multiaddress string"));
		};
		let peer_id = PeerId::from_str(peer_id)?;
		let multiaddr = Multiaddr::from_str(&value)?;
		Ok(CompactMultiaddress((peer_id, multiaddr)))
	}
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(
	untagged,
	expecting = "Valid multiaddress/peer_id string or a tuple (peer_id, multiaddress) expected"
)]
pub enum MultiaddrConfig {
	Compact(CompactMultiaddress),
	PeerIdAndMultiaddr((PeerId, Multiaddr)),
}

impl From<&MultiaddrConfig> for (PeerId, Multiaddr) {
	fn from(value: &MultiaddrConfig) -> Self {
		match value {
			MultiaddrConfig::Compact(CompactMultiaddress(value)) => value.clone(),
			MultiaddrConfig::PeerIdAndMultiaddr(value) => value.clone(),
		}
	}
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum SecretKey {
	Seed { seed: String },
	Key { key: String },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum RetryConfig {
	#[serde(rename = "exponential")]
	Exponential(ExponentialConfig),

	#[serde(rename = "fibonacci")]
	Fibonacci(FibonacciConfig),
}

impl IntoIterator for RetryConfig {
	type Item = Duration;
	type IntoIter = std::vec::IntoIter<Self::Item>;

	fn into_iter(self) -> Self::IntoIter {
		match self {
			RetryConfig::Exponential(config) => ExponentialBackoff::from_millis(config.base)
				.factor(1000)
				.max_delay(Duration::from_millis(config.max_delay))
				.map(jitter)
				.take(config.retries)
				.collect::<Vec<Duration>>()
				.into_iter(),
			RetryConfig::Fibonacci(config) => FibonacciBackoff::from_millis(config.base)
				.factor(1000)
				.max_delay(Duration::from_millis(config.max_delay))
				.map(jitter)
				.take(config.retries)
				.collect::<Vec<Duration>>()
				.into_iter(),
		}
	}
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ExponentialConfig {
	pub base: u64,
	pub max_delay: u64,
	pub retries: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FibonacciConfig {
	pub base: u64,
	pub max_delay: u64,
	pub retries: usize,
}

/// Representation of a configuration used by this project.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct RuntimeConfig {
	/// Light client HTTP server host name (default: 127.0.0.1).
	pub http_server_host: String,
	/// Light client HTTP server port (default: 7000).
	pub http_server_port: u16,
	/// Secret key for libp2p keypair. Can be either set to `seed` or to `key`.
	/// If set to seed, keypair will be generated from that seed.
	/// If set to key, a valid ed25519 private key must be provided, else the client will fail
	/// If `secret_key` is not set, random seed will be used.
	pub secret_key: Option<SecretKey>,
	/// P2P service port (default: 37000).
	pub port: u16,
	pub ws_transport_enable: bool,
	/// Configures AutoNAT behaviour to reject probes as a server for clients that are observed at a non-global ip address (default: false)
	pub autonat_only_global_ips: bool,
	/// AutoNat throttle period for re-using a peer as server for a dial-request. (default: 1 sec)
	pub autonat_throttle: u64,
	/// Interval in which the NAT status should be re-tried if it is currently unknown or max confidence was not reached yet. (default: 10 sec)
	pub autonat_retry_interval: u64,
	/// Interval in which the NAT should be tested again if max confidence was reached in a status. (default: 30 sec)
	pub autonat_refresh_interval: u64,
	/// AutoNat on init delay before starting the fist probe. (default: 5 sec)
	pub autonat_boot_delay: u64,
	/// Vector of Light Client bootstrap nodes, used to bootstrap DHT. If not set, light client acts as a bootstrap node, waiting for first peer to connect for DHT bootstrap (default: empty).
	pub bootstraps: Vec<MultiaddrConfig>,
	/// Defines a period of time in which periodic bootstraps will be repeated. (default: 300 sec)
	pub bootstrap_period: u64,
	pub operation_mode: KademliaMode,
	/// Vector of Relay nodes, which are used for hole punching
	pub relays: Vec<MultiaddrConfig>,
	/// WebSocket endpoint of full node for subscribing to latest header, etc (default: [ws://127.0.0.1:9944]).
	pub full_node_ws: Vec<String>,
	/// Genesis hash of the network to be connected to. Set to a string beginning with "DEV" to connect to any network.
	pub genesis_hash: String,
	/// ID of application used to start application client. If app_id is not set, or set to 0, application client is not started (default: 0).
	pub app_id: Option<u32>,
	/// Confidence threshold, used to calculate how many cells need to be sampled to achieve desired confidence (default: 92.0).
	pub confidence: f64,
	/// File system path where RocksDB used by light client, stores its data.
	pub avail_path: String,
	/// Log level, default is `INFO`. See `<https://docs.rs/log/0.4.14/log/enum.LevelFilter.html>` for possible log level values. (default: `INFO`).
	pub log_level: String,
	pub origin: String,
	/// If set to true, logs are displayed in JSON format, which is used for structured logging. Otherwise, plain text format is used (default: false).
	pub log_format_json: bool,
	/// OpenTelemetry Collector endpoint (default: `http://otelcollector.avail.tools:4317`)
	pub ot_collector_endpoint: String,
	/// Disables fetching of cells from RPC, set to true if client expects cells to be available in DHT (default: false).
	pub disable_rpc: bool,
	/// Maximum number of parallel tasks spawned for GET and PUT operations on DHT (default: 20).
	pub dht_parallelization_limit: usize,
	/// Number of parallel queries for cell fetching via RPC from node (default: 8).
	pub query_proof_rpc_parallel_tasks: usize,
	/// Number of seconds to postpone block processing after block finalized message arrives (default: 0).
	pub block_processing_delay: Option<u32>,
	/// Fraction and number of the block matrix part to fetch (e.g. 2/20 means second 1/20 part of a matrix) (default: None)
	#[serde(with = "block_matrix_partition_format")]
	pub block_matrix_partition: Option<Partition>,
	/// Starting block of the syncing process. Omitting it will disable syncing. (default: None).
	pub sync_start_block: Option<u32>,
	/// Enable or disable synchronizing finality. If disabled, finality is assumed to be verified until the starting block at the point the LC is started and is only checked for new blocks. (default: true)
	pub sync_finality_enable: bool,
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
	pub connection_idle_timeout: u64,
	pub max_negotiating_inbound_streams: usize,
	pub task_command_buffer_size: usize,
	pub per_connection_event_buffer_size: usize,
	pub dial_concurrency_factor: u8,
	/// Sets the timeout for a single Kademlia query. (default: 60s).
	pub store_pruning_interval: u32,
	/// Sets the allowed level of parallelism for iterative Kademlia queries. (default: 3).
	pub query_timeout: u32,
	/// Sets the Kademlia record store pruning interval in blocks (default: 180).
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
	/// The maximum size of record values, in bytes. (default: 8192).
	pub max_kad_record_size: u64,
	/// The maximum number of provider records for which the local node is the provider. (default: 1024).
	pub max_kad_provided_keys: u64,
	/// Set the configuration based on which the retries will be orchestrated, max duration [in seconds] between retries and number of tries.
	/// (default:
	/// fibonacci:
	///     base: 1,
	///     max_delay: 10,
	///     retries: 6,
	/// )
	pub retry_config: RetryConfig,
	#[cfg(feature = "crawl")]
	#[serde(flatten)]
	pub crawl: crate::crawl_client::CrawlConfig,
}

impl RuntimeConfig {
	pub fn is_fat_client(&self) -> bool {
		self.block_matrix_partition.is_some()
	}
}

pub struct Delay(pub Option<Duration>);

/// Light client configuration (see [RuntimeConfig] for details)
pub struct LightClientConfig {
	pub confidence: f64,
	pub block_processing_delay: Delay,
}

impl Delay {
	pub fn sleep_duration(&self, from: Instant) -> Option<Duration> {
		(self.0?)
			.checked_sub(from.elapsed())
			.filter(|duration| !duration.is_zero())
	}
}

impl From<&RuntimeConfig> for LightClientConfig {
	fn from(val: &RuntimeConfig) -> Self {
		let block_processing_delay = val
			.block_processing_delay
			.map(|v| Duration::from_secs(v.into()));

		LightClientConfig {
			confidence: val.confidence,
			block_processing_delay: Delay(block_processing_delay),
		}
	}
}

/// Fat client configuration (see [RuntimeConfig] for details)
pub struct FatClientConfig {
	pub full_nodes_ws: Vec<String>,
	pub confidence: f64,
	pub disable_rpc: bool,
	pub dht_parallelization_limit: usize,
	pub query_proof_rpc_parallel_tasks: usize,
	pub block_processing_delay: Delay,
	pub block_matrix_partition: Option<Partition>,
	pub max_cells_per_rpc: usize,
}

impl From<&RuntimeConfig> for FatClientConfig {
	fn from(val: &RuntimeConfig) -> Self {
		let block_processing_delay = val
			.block_processing_delay
			.map(|v| Duration::from_secs(v.into()));

		FatClientConfig {
			full_nodes_ws: val.full_node_ws.clone(),
			confidence: val.confidence,
			disable_rpc: val.disable_rpc,
			dht_parallelization_limit: val.dht_parallelization_limit,
			query_proof_rpc_parallel_tasks: val.query_proof_rpc_parallel_tasks,
			block_processing_delay: Delay(block_processing_delay),
			block_matrix_partition: val.block_matrix_partition,
			max_cells_per_rpc: val.max_cells_per_rpc.unwrap_or(30),
		}
	}
}

#[derive(Clone)]
pub struct LibP2PConfig {
	pub secret_key: Option<SecretKey>,
	pub port: u16,
	pub identify: IdentifyConfig,
	pub autonat: AutoNATConfig,
	pub kademlia: KademliaConfig,
	pub relays: Vec<(PeerId, Multiaddr)>,
	pub bootstrap_interval: Duration,
	pub connection_idle_timeout: Duration,
	pub max_negotiating_inbound_streams: usize,
	pub task_command_buffer_size: NonZeroUsize,
	pub per_connection_event_buffer_size: usize,
	pub dial_concurrency_factor: NonZeroU8,
}

impl From<&LibP2PConfig> for libp2p::kad::Config {
	fn from(cfg: &LibP2PConfig) -> Self {
		// Use identify protocol_version as Kademlia protocol name
		let kademlia_protocol_name =
			libp2p::StreamProtocol::try_from_owned(cfg.identify.protocol_version.clone())
				.expect("Invalid Kademlia protocol name");

		// create Kademlia Config
		let mut kad_cfg = libp2p::kad::Config::default();
		kad_cfg
			.set_publication_interval(cfg.kademlia.publication_interval)
			.set_replication_interval(cfg.kademlia.record_replication_interval)
			.set_replication_factor(cfg.kademlia.record_replication_factor)
			.set_query_timeout(cfg.kademlia.query_timeout)
			.set_parallelism(cfg.kademlia.query_parallelism)
			.set_caching(libp2p::kad::Caching::Enabled {
				max_peers: cfg.kademlia.caching_max_peers,
			})
			.disjoint_query_paths(cfg.kademlia.disjoint_query_paths)
			.set_record_filtering(libp2p::kad::StoreInserts::FilterBoth)
			.set_protocol_names(vec![kademlia_protocol_name]);
		kad_cfg
	}
}

impl From<&LibP2PConfig> for MemoryStoreConfig {
	fn from(cfg: &LibP2PConfig) -> Self {
		MemoryStoreConfig {
			max_records: cfg.kademlia.max_kad_record_number, // ~2hrs
			max_value_bytes: cfg.kademlia.max_kad_record_size + 1,
			max_providers_per_key: usize::from(cfg.kademlia.record_replication_factor), // Needs to match the replication factor, per libp2p docs
			max_provided_keys: cfg.kademlia.max_kad_provided_keys,
		}
	}
}

impl From<&RuntimeConfig> for LibP2PConfig {
	fn from(val: &RuntimeConfig) -> Self {
		Self {
			secret_key: val.secret_key.clone(),
			port: val.port,
			identify: val.into(),
			autonat: val.into(),
			kademlia: val.into(),
			relays: val.relays.iter().map(Into::into).collect(),
			bootstrap_interval: Duration::from_secs(val.bootstrap_period),
			connection_idle_timeout: Duration::from_secs(val.connection_idle_timeout),
			max_negotiating_inbound_streams: val.max_negotiating_inbound_streams,
			task_command_buffer_size: std::num::NonZeroUsize::new(val.task_command_buffer_size)
				.expect("Invalid task command buffer size"),
			per_connection_event_buffer_size: val.per_connection_event_buffer_size,
			dial_concurrency_factor: std::num::NonZeroU8::new(val.dial_concurrency_factor)
				.expect("Invalid dial concurrency factor"),
		}
	}
}

/// Kademlia configuration (see [RuntimeConfig] for details)
#[derive(Clone)]
pub struct KademliaConfig {
	pub kad_record_ttl: Duration,
	pub record_replication_factor: NonZeroUsize,
	pub record_replication_interval: Option<Duration>,
	pub publication_interval: Option<Duration>,
	pub query_timeout: Duration,
	pub query_parallelism: NonZeroUsize,
	pub caching_max_peers: u16,
	pub disjoint_query_paths: bool,
	pub max_kad_record_number: usize,
	pub max_kad_record_size: usize,
	pub max_kad_provided_keys: usize,
	pub kademlia_mode: KademliaMode,
}

impl From<&RuntimeConfig> for KademliaConfig {
	fn from(val: &RuntimeConfig) -> Self {
		Self {
			kad_record_ttl: Duration::from_secs(val.kad_record_ttl),
			record_replication_factor: std::num::NonZeroUsize::new(val.replication_factor as usize)
				.expect("Invalid replication factor"),
			record_replication_interval: Some(Duration::from_secs(val.replication_interval.into())),
			publication_interval: Some(Duration::from_secs(val.publication_interval.into())),
			query_timeout: Duration::from_secs(val.query_timeout.into()),
			query_parallelism: std::num::NonZeroUsize::new(val.query_parallelism as usize)
				.expect("Invalid query parallelism value"),
			caching_max_peers: val.caching_max_peers,
			disjoint_query_paths: val.disjoint_query_paths,
			max_kad_record_number: val.max_kad_record_number as usize,
			max_kad_record_size: val.max_kad_record_size as usize,
			max_kad_provided_keys: val.max_kad_provided_keys as usize,
			kademlia_mode: val.operation_mode,
		}
	}
}

/// Libp2p AutoNAT configuration (see [RuntimeConfig] for details)
#[derive(Clone)]
pub struct AutoNATConfig {
	pub retry_interval: Duration,
	pub refresh_interval: Duration,
	pub boot_delay: Duration,
	pub throttle_server_period: Duration,
	pub only_global_ips: bool,
}

impl From<&RuntimeConfig> for AutoNATConfig {
	fn from(val: &RuntimeConfig) -> Self {
		Self {
			retry_interval: Duration::from_secs(val.autonat_retry_interval),
			refresh_interval: Duration::from_secs(val.autonat_refresh_interval),
			boot_delay: Duration::from_secs(val.autonat_boot_delay),
			throttle_server_period: Duration::from_secs(val.autonat_throttle),
			only_global_ips: val.autonat_only_global_ips,
		}
	}
}

#[derive(Clone)]
pub struct IdentifyConfig {
	pub agent_version: AgentVersion,
	/// Contains Avail genesis hash
	pub protocol_version: String,
}

#[derive(Clone)]
pub struct AgentVersion {
	pub base_version: String,
	pub client_type: String,
	// Kademlia client or server mode
	pub kademlia_mode: String,
}

impl fmt::Display for AgentVersion {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(
			f,
			"{}/{}/{}",
			self.base_version, self.client_type, self.kademlia_mode
		)
	}
}

impl FromStr for AgentVersion {
	type Err = String;

	fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
		let parts: Vec<&str> = s.split('/').collect();
		if parts.len() != 3 {
			return Err("Failed to parse agent version".to_owned());
		}

		Ok(AgentVersion {
			base_version: parts[0].to_string(),
			client_type: parts[1].to_string(),
			kademlia_mode: parts[2].to_string(),
		})
	}
}

impl From<&RuntimeConfig> for IdentifyConfig {
	fn from(val: &RuntimeConfig) -> Self {
		let mut genhash_short = val.genesis_hash.trim_start_matches("0x").to_string();
		genhash_short.truncate(6);

		let kademlia_mode = if val.is_fat_client() {
			// Fat client is implicitly server mode
			KademliaMode::Server.to_string()
		} else {
			val.operation_mode.to_string()
		};

		let agent_version = AgentVersion {
			base_version: IDENTITY_AGENT_BASE.to_string(),
			client_type: IDENTITY_AGENT_CLIENT_TYPE.to_string(),
			kademlia_mode,
		};

		Self {
			agent_version,
			protocol_version: format!(
				"{id}-{gen_hash}",
				id = IDENTITY_PROTOCOL,
				gen_hash = genhash_short
			),
		}
	}
}

/// Sync client configuration (see [RuntimeConfig] for details)
#[derive(Clone)]
pub struct SyncClientConfig {
	pub confidence: f64,
	pub disable_rpc: bool,
	pub dht_parallelization_limit: usize,
	pub is_last_step: bool,
}

impl From<&RuntimeConfig> for SyncClientConfig {
	fn from(val: &RuntimeConfig) -> Self {
		SyncClientConfig {
			confidence: val.confidence,
			disable_rpc: val.disable_rpc,
			dht_parallelization_limit: val.dht_parallelization_limit,
			is_last_step: val.app_id.is_none(),
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
			http_server_port: 7000,
			port: 37000,
			ws_transport_enable: false,
			secret_key: None,
			autonat_only_global_ips: false,
			autonat_refresh_interval: 360,
			autonat_retry_interval: 20,
			autonat_throttle: 1,
			autonat_boot_delay: 5,
			bootstraps: vec![],
			bootstrap_period: 3600,
			relays: Vec::new(),
			full_node_ws: vec!["ws://127.0.0.1:9944".to_owned()],
			genesis_hash: "DEV".to_owned(),
			app_id: None,
			confidence: 99.9,
			avail_path: "avail_path".to_owned(),
			log_level: "INFO".to_owned(),
			log_format_json: false,
			ot_collector_endpoint: "http://127.0.0.1:4317".to_string(),
			disable_rpc: false,
			dht_parallelization_limit: 20,
			query_proof_rpc_parallel_tasks: 8,
			block_processing_delay: Some(20),
			block_matrix_partition: None,
			sync_start_block: None,
			sync_finality_enable: false,
			max_cells_per_rpc: Some(30),
			kad_record_ttl: 24 * 60 * 60,
			threshold: 5000,
			replication_factor: 5,
			publication_interval: 12 * 60 * 60,
			replication_interval: 3 * 60 * 60,
			connection_idle_timeout: 30,
			max_negotiating_inbound_streams: 128,
			task_command_buffer_size: 32,
			per_connection_event_buffer_size: 7,
			dial_concurrency_factor: 8,
			store_pruning_interval: 180,
			query_timeout: 10,
			query_parallelism: 3,
			caching_max_peers: 1,
			disjoint_query_paths: false,
			max_kad_record_number: 2400000,
			max_kad_record_size: 8192,
			max_kad_provided_keys: 1024,
			#[cfg(feature = "crawl")]
			crawl: crate::crawl_client::CrawlConfig::default(),
			origin: "external".to_string(),
			operation_mode: KademliaMode::Client,
			retry_config: RetryConfig::Fibonacci(FibonacciConfig {
				base: 1,
				max_delay: 10,
				retries: 6,
			}),
		}
	}
}

#[derive(Clone)]
pub enum Network {
	Local,
}

impl FromStr for Network {
	type Err = String;

	fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
		match s {
			"local" => Ok(Network::Local),
			_ => Err("valid values are: local".to_string()),
		}
	}
}

impl Network {
	fn peer_id(&self) -> &str {
		match self {
			Network::Local => "12D3KooWStAKPADXqJ7cngPYXd2mSANpdgh1xQ34aouufHA2xShz",
		}
	}

	fn multiaddr(&self) -> &str {
		match self {
			Network::Local => "/ip4/127.0.0.1/tcp/39000",
		}
	}

	fn full_node_ws(&self) -> &str {
		match self {
			Network::Local => "ws://127.0.0.1:9944",
		}
	}

	fn ot_collector_endpoint(&self) -> &str {
		match self {
			Network::Local => "http://127.0.0.1:4317",
		}
	}

	fn genesis_hash(&self) -> &str {
		match self {
			Network::Local => "DEV",
		}
	}
}

#[derive(Clone)]
pub enum LogLevel {
	Info,
	Debug,
	Trace,
	Warn,
	Error,
}

impl FromStr for LogLevel {
	type Err = String;

	fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
		match s {
			"trace" => Ok(LogLevel::Trace),
			"debug" => Ok(LogLevel::Debug),
			"info" => Ok(LogLevel::Info),
			"warn" => Ok(LogLevel::Warn),
			"error" => Ok(LogLevel::Error),
			_ => Err("valid values are: trace, debug and info".to_string()),
		}
	}
}

impl ToString for LogLevel {
	fn to_string(&self) -> String {
		match self {
			LogLevel::Info => "INFO".to_string(),
			LogLevel::Debug => "DEBUG".to_string(),
			LogLevel::Trace => "TRACE".to_string(),
			LogLevel::Warn => "WARN".to_string(),
			LogLevel::Error => "ERROR".to_string(),
		}
	}
}

impl RuntimeConfig {
	/// A range bounded inclusively below and exclusively above
	pub fn sync_range(&self, end: u32) -> Range<u32> {
		let start = self.sync_start_block.unwrap_or(end);
		Range { start, end }
	}

	pub fn load_runtime_config(&mut self, opts: &CliOpts) -> Result<()> {
		if let Some(config_path) = &opts.config {
			fs::metadata(config_path).map_err(|_| eyre!("Provided config file doesn't exist."))?;
			let cfg: RuntimeConfig = confy::load_path(config_path)
				.wrap_err(format!("Failed to load configuration from {}", config_path))?;
			*self = cfg;
		}

		// Flags override the config parameters
		if let Some(network) = &opts.network {
			let bootstrap: (PeerId, Multiaddr) = (
				PeerId::from_str(network.peer_id())
					.wrap_err("unable to parse default bootstrap peerID")?,
				Multiaddr::from_str(network.multiaddr())
					.wrap_err("unable to parse default bootstrap multi-address")?,
			);
			self.full_node_ws = vec![network.full_node_ws().to_string()];
			self.bootstraps = vec![MultiaddrConfig::PeerIdAndMultiaddr(bootstrap)];
			self.ot_collector_endpoint = network.ot_collector_endpoint().to_string();
			self.genesis_hash = network.genesis_hash().to_string();
		}

		if let Some(loglvl) = &opts.verbosity {
			self.log_level = loglvl.to_string();
		}

		if let Some(port) = opts.port {
			self.port = port;
		}
		self.sync_finality_enable |= opts.finality_sync_enable;
		self.app_id = opts.app_id.or(self.app_id);
		self.ws_transport_enable |= opts.ws_transport_enable;
		if let Some(secret_key) = &opts.private_key {
			self.secret_key = Some(SecretKey::Key {
				key: secret_key.to_string(),
			});
		}

		if let Some(seed) = &opts.seed {
			self.secret_key = Some(SecretKey::Seed {
				seed: seed.to_string(),
			})
		}

		Ok(())
	}
}

pub struct IdentityConfig {
	/// Avail account secret key. (secret is generated if it is not configured)
	pub avail_key_pair: Pair,
	/// Avail ss58 address
	pub avail_address: String,
}

impl IdentityConfig {
	pub fn load_or_init(path: &str, password: Option<&str>) -> Result<Self> {
		#[derive(Default, Serialize, Deserialize)]
		struct Config {
			pub avail_secret_seed_phrase: Option<String>,
		}

		let mut config: Config = confy::load_path(path)?;

		let phrase = match config.avail_secret_seed_phrase.as_ref() {
			None => Mnemonic::new(MnemonicType::Words24, Language::English).into_phrase(),
			Some(phrase) => phrase.to_owned(),
		};

		if config.avail_secret_seed_phrase.is_none() {
			config.avail_secret_seed_phrase = Some(phrase.clone());
			confy::store_path(path, &config)?;
		}

		let (avail_key_pair, _) = Pair::from_string_with_seed(&phrase, password)?;
		let avail_address = avail_key_pair.public().to_ss58check();

		Ok(IdentityConfig {
			avail_key_pair,
			avail_address,
		})
	}
}

#[derive(Clone)]
pub struct BlockRange {
	pub first: u32,
	pub last: u32,
}

impl BlockRange {
	pub fn init(last: u32) -> BlockRange {
		let first = last;
		BlockRange { first, last }
	}

	pub fn contains(&self, block_number: u32) -> bool {
		self.first <= block_number && block_number <= self.last
	}
}

#[derive(Default)]
pub struct State {
	pub synced: Option<bool>,
	pub latest: u32,
	pub header_verified: Option<BlockRange>,
	pub confidence_achieved: Option<BlockRange>,
	pub data_verified: Option<BlockRange>,
	pub sync_latest: Option<u32>,
	pub sync_header_verified: Option<BlockRange>,
	pub sync_confidence_achieved: Option<BlockRange>,
	pub sync_data_verified: Option<BlockRange>,
	pub finality_synced: bool,
	pub connected_node: RpcNode,
}

pub trait OptionBlockRange {
	fn set(&mut self, block_number: u32);
	fn first(&self) -> Option<u32>;
	fn last(&self) -> Option<u32>;
	fn contains(&self, block_number: u32) -> bool;
}

impl OptionBlockRange for Option<BlockRange> {
	fn set(&mut self, block_number: u32) {
		match self {
			Some(range) => range.last = block_number,
			None => *self = Some(BlockRange::init(block_number)),
		};
	}

	fn first(&self) -> Option<u32> {
		self.as_ref().map(|range| range.first)
	}

	fn last(&self) -> Option<u32> {
		self.as_ref().map(|range| range.last)
	}

	fn contains(&self, block_number: u32) -> bool {
		self.as_ref()
			.map(|range| range.contains(block_number))
			.unwrap_or(false)
	}
}

#[derive(Debug, Encode)]
pub enum SignerMessage {
	_DummyMessage(u32),
	PrecommitMessage(Precommit),
}

#[derive(Clone, Debug, Decode, Encode, Deserialize)]
pub struct Precommit {
	pub target_hash: H256,
	/// The target block's number
	pub target_number: u32,
}

#[derive(Clone, Debug, Decode, Deserialize)]
pub struct SignedPrecommit {
	pub precommit: Precommit,
	/// The signature on the message.
	pub signature: ed25519::Signature,
	/// The Id of the signer.
	pub id: ed25519::Public,
}
#[derive(Clone, Debug, Decode, Deserialize)]
pub struct Commit {
	pub target_hash: H256,
	/// The target block's number.
	pub target_number: u32,
	/// Precommits for target block or any block after it that justify this commit.
	pub precommits: Vec<SignedPrecommit>,
}

#[derive(Clone, Debug, Decode)]
pub struct GrandpaJustification {
	pub round: u64,
	pub commit: Commit,
	pub votes_ancestries: Vec<DaHeader>,
}

impl<'de> Deserialize<'de> for GrandpaJustification {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		let encoded = bytes::deserialize(deserializer)?;
		Self::decode(&mut &encoded[..])
			.map_err(|codec_err| D::Error::custom(format!("Invalid decoding: {:?}", codec_err)))
	}
}

pub struct TimeToLive(pub Duration);

impl TimeToLive {
	/// Expiry at instant from now
	pub fn expires(&self) -> Option<Instant> {
		Instant::now().checked_add(self.0)
	}
}
