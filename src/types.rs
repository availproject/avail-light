//! Shared light client structs and enums.

use crate::network::rpc::Node as RpcNode;
use crate::utils::{extract_app_lookup, extract_kate};
use anyhow::anyhow;
use anyhow::{Context, Result};
use avail_core::DataLookup;
use avail_subxt::{primitives::Header as DaHeader, utils::H256};
use bip39::{Language, Mnemonic, MnemonicType};
use clap::Parser;
use codec::{Decode, Encode};
use kate_recovery::{
	commitments,
	matrix::{Dimensions, Partition},
};
use libp2p::{Multiaddr, PeerId};
use serde::{de::Error, Deserialize, Serialize};
use sp_core::crypto::Ss58Codec;
use sp_core::{blake2_256, bytes, ed25519};
use std::fs;
use std::num::NonZeroUsize;
use std::ops::Range;
use std::str::FromStr;
use std::time::{Duration, Instant};
use subxt::ext::sp_core::{sr25519::Pair, Pair as _};

const CELL_SIZE: usize = 32;
const PROOF_SIZE: usize = 48;
pub const CELL_WITH_PROOF_SIZE: usize = CELL_SIZE + PROOF_SIZE;

#[derive(Parser)]
#[command(version)]
pub struct CliOpts {
	/// Path to the yaml configuration file
	#[arg(short, long, value_name = "FILE")]
	pub config: Option<String>,
	/// AppID for application client
	#[arg(long, value_name = "appId")]
	pub app_id: Option<u32>,
	/// Testnet or devnet selection
	#[arg(short, long, value_name = "network")]
	pub network: Option<Network>,
	/// Run a clean light client, deleting existing avail_path folder
	#[arg(long)]
	pub clean: bool,
	/// P2P port
	#[arg(short, long)]
	pub port: Option<u16>,
	/// Log level
	#[arg(long)]
	pub verbosity: Option<LogLevel>,
	/// Avail secret seed phrase password
	#[arg(long)]
	pub avail_passphrase: Option<String>,
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

impl TryFrom<(DaHeader, Option<f64>)> for BlockVerified {
	type Error = anyhow::Error;
	fn try_from((header, confidence): (DaHeader, Option<f64>)) -> Result<Self, Self::Error> {
		let hash: H256 = Encode::using_encoded(&header, blake2_256).into();
		let enc_lookup = extract_app_lookup(&header.extension)
			.map_err(|e| anyhow!("Invalid DataLookup: {}", e))?
			.encode();
		let lookup = DataLookup::decode(&mut enc_lookup.as_slice())?;
		let (rows, cols, _, commitment) = extract_kate(&header.extension);

		Ok(BlockVerified {
			header_hash: hash,
			block_num: header.number,
			dimensions: Dimensions::new(rows, cols).context("Invalid dimensions")?,
			lookup,
			commitments: commitments::from_slice(&commitment)?,
			confidence,
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
	type Error = anyhow::Error;

	fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
		let Some((_, peer_id)) = value.rsplit_once('/') else {
			return Err(anyhow!("Invalid multiaddress string"));
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

/// Representation of a configuration used by this project.
#[derive(Serialize, Deserialize, Debug, Clone)]
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
	/// Sets application-specific version of the protocol family used by the peer. (default: "/avail_kad/id/1.0.0")
	pub identify_protocol: String,
	/// Sets agent version that is sent to peers. (default: "avail-light-client/rust-client")
	pub identify_agent: String,
	/// Vector of Light Client bootstrap nodes, used to bootstrap DHT. If not set, light client acts as a bootstrap node, waiting for first peer to connect for DHT bootstrap (default: empty).
	pub bootstraps: Vec<MultiaddrConfig>,
	/// Defines a period of time in which periodic bootstraps will be repeated. (default: 300 sec)
	pub bootstrap_period: u64,
	/// Vector of Relay nodes, which are used for hole punching
	pub relays: Vec<MultiaddrConfig>,
	/// WebSocket endpoint of full node for subscribing to latest header, etc (default: [ws://127.0.0.1:9944]).
	pub full_node_ws: Vec<String>,
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
	/// Disables proof verification in general, if set to true, otherwise proof verification is performed. (default: false).
	pub disable_proof_verification: bool,
	/// Maximum number of parallel tasks spawned for GET and PUT operations on DHT (default: 20).
	pub dht_parallelization_limit: usize,
	/// Number of records to be inserted into DHT simultaneously (default: 1000)
	pub put_batch_size: usize,
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
	/// The maximum size of record values, in bytes. (default: 8192).
	pub max_kad_record_size: u64,
	/// The maximum number of provider records for which the local node is the provider. (default: 1024).
	pub max_kad_provided_keys: u64,
	#[cfg(feature = "crawl")]
	#[serde(flatten)]
	pub crawl: crate::crawl_client::CrawlConfig,
}

pub struct Delay(pub Option<Duration>);

/// Light client configuration (see [RuntimeConfig] for details)
pub struct LightClientConfig {
	pub full_nodes_ws: Vec<String>,
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
			full_nodes_ws: val.full_node_ws.clone(),
			confidence: val.confidence,
			disable_rpc: val.disable_rpc,
			dht_parallelization_limit: val.dht_parallelization_limit,
			query_proof_rpc_parallel_tasks: val.query_proof_rpc_parallel_tasks,
			block_processing_delay: Delay(block_processing_delay),
			block_matrix_partition: val.block_matrix_partition,
			disable_proof_verification: val.disable_proof_verification,
			max_cells_per_rpc: val.max_cells_per_rpc.unwrap_or(30),
			ttl: val.kad_record_ttl,
		}
	}
}

pub struct LibP2PConfig {
	pub secret_key: Option<SecretKey>,
	pub port: u16,
	pub identify: IdentifyConfig,
	pub autonat: AutoNATConfig,
	pub kademlia: KademliaConfig,
	pub relays: Vec<(PeerId, Multiaddr)>,
	pub bootstrap_interval: Duration,
	pub connection_idle_timeout: Duration,
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
		}
	}
}

/// Kademlia configuration (see [RuntimeConfig] for details)
pub struct KademliaConfig {
	pub record_ttl: u64,
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
}

impl From<&RuntimeConfig> for KademliaConfig {
	fn from(val: &RuntimeConfig) -> Self {
		Self {
			record_ttl: val.kad_record_ttl,
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
		}
	}
}

/// Libp2p AutoNAT configuration (see [RuntimeConfig] for details)
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

pub struct IdentifyConfig {
	pub agent_version: String,
	pub protocol_version: String,
}

impl From<&RuntimeConfig> for IdentifyConfig {
	fn from(val: &RuntimeConfig) -> Self {
		Self {
			agent_version: val.identify_agent.clone(),
			protocol_version: val.identify_protocol.clone(),
		}
	}
}

/// Sync client configuration (see [RuntimeConfig] for details)
#[derive(Clone)]
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
			http_server_port: 7000,
			port: 37000,
			secret_key: None,
			autonat_only_global_ips: false,
			autonat_refresh_interval: 30,
			autonat_retry_interval: 10,
			autonat_throttle: 1,
			autonat_boot_delay: 5,
			identify_protocol: "/avail_kad/id/1.0.0".to_string(),
			identify_agent: "avail-light-client/rust-client".to_string(),
			bootstraps: vec![],
			bootstrap_period: 300,
			relays: Vec::new(),
			full_node_ws: vec!["ws://127.0.0.1:9944".to_owned()],
			app_id: None,
			confidence: 92.0,
			avail_path: "avail_path".to_owned(),
			log_level: "INFO".to_owned(),
			log_format_json: false,
			ot_collector_endpoint: "http://127.0.0.1:4317".to_string(),
			disable_rpc: false,
			disable_proof_verification: false,
			dht_parallelization_limit: 20,
			put_batch_size: 1000,
			query_proof_rpc_parallel_tasks: 8,
			block_processing_delay: None,
			block_matrix_partition: None,
			sync_start_block: None,
			sync_finality_enable: true,
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
			max_kad_record_size: 8192,
			max_kad_provided_keys: 1024,
			#[cfg(feature = "crawl")]
			crawl: crate::crawl_client::CrawlConfig::default(),
			origin: "external".to_string(),
		}
	}
}

#[derive(Clone)]
pub enum Network {
	Local,
	Goldberg,
}

impl FromStr for Network {
	type Err = String;

	fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
		match s {
			"local" => Ok(Network::Local),
			"goldberg" => Ok(Network::Goldberg),
			_ => Err("valid values are: local, biryani, goldberg".to_string()),
		}
	}
}

impl Network {
	fn peer_id(&self) -> &str {
		match self {
			Network::Local => "12D3KooWStAKPADXqJ7cngPYXd2mSANpdgh1xQ34aouufHA2xShz",
			Network::Goldberg => "12D3KooWBkLsNGaD3SpMaRWtAmWVuiZg1afdNSPbtJ8M8r9ArGRT",
		}
	}

	fn multiaddr(&self) -> &str {
		match self {
			Network::Local => "/ip4/127.0.0.1/udp/39000/quic-v1",
			Network::Goldberg => {
				"/dns/bootnode.1.lightclient.goldberg.avail.tools/udp/37000/quic-v1"
			},
		}
	}

	fn full_node_ws(&self) -> &str {
		match self {
			Network::Local => "ws://127.0.0.1:9944",
			Network::Goldberg => "wss://goldberg.avail.tools:443/ws",
		}
	}

	fn ot_collector_endpoint(&self) -> &str {
		match self {
			Network::Local => "http://127.0.0.1:4317",
			Network::Goldberg => "http://otel.lightclient.goldberg.avail.tools:4317",
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
			fs::metadata(config_path)
				.map_err(|_| anyhow!("Provided config file doesn't exist."))?;
			let cfg: RuntimeConfig = confy::load_path(config_path)
				.context(format!("Failed to load configuration from {}", config_path))?;
			*self = cfg;
		}

		// Flags override the config parameters
		if let Some(network) = &opts.network {
			let bootstrap: (PeerId, Multiaddr) = (
				PeerId::from_str(network.peer_id())
					.context("unable to parse default bootstrap peerID")?,
				Multiaddr::from_str(network.multiaddr())
					.context("unable to parse default bootstrap multi-address")?,
			);
			self.full_node_ws = vec![network.full_node_ws().to_string()];
			self.bootstraps = vec![MultiaddrConfig::PeerIdAndMultiaddr(bootstrap)];
			self.ot_collector_endpoint = network.ot_collector_endpoint().to_string();
		}

		if let Some(loglvl) = &opts.verbosity {
			self.log_level = loglvl.to_string();
		}

		if let Some(port) = opts.port {
			self.port = port;
		}

		self.app_id = opts.app_id.or(self.app_id);

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

#[derive(Debug, Decode, Encode)]
pub struct FinalitySyncCheckpoint {
	pub number: u32,
	pub set_id: u64,
	pub validator_set: Vec<ed25519::Public>,
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
	pub _votes_ancestries: Vec<DaHeader>,
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
