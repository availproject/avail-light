//! Shared light client structs and enums.

use anyhow::Context;
use avail_subxt::api::runtime_types::da_primitives::header::extension::HeaderExtension;
use avail_subxt::api::runtime_types::da_primitives::kate_commitment::KateCommitment;
use avail_subxt::primitives::Header as DaHeader;
use codec::{Decode, Encode};
use ipfs_embed::{Block as IpfsBlock, DefaultParams, Multiaddr, PeerId};
use kate_recovery::com::{AppDataIndex, ExtendedMatrixDimensions};
use serde::{Deserialize, Deserializer, Serialize};
use sp_core::{blake2_256, H256};

/// IPFS events wrapper
#[derive(Debug, Eq, PartialEq)]
pub enum Event {
	NewListener,
	NewListenAddr(Multiaddr),
	ExpiredListenAddr(Multiaddr),
	ListenerClosed,
	NewExternalAddr(Multiaddr),
	ExpiredExternalAddr(Multiaddr),
	Discovered(PeerId),
	Unreachable(PeerId),
	Connected(PeerId),
	Disconnected(PeerId),
	Subscribed(PeerId, String),
	Unsubscribed(PeerId, String),
	Block(IpfsBlock<DefaultParams>),
	Flushed,
	Synced,
	Bootstrapped,
	NewInfo(PeerId),
	Other,
}

impl std::fmt::Display for Event {
	fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
		match self {
			Self::NewListener => write!(f, "<new-listener")?,
			Self::NewListenAddr(addr) => write!(f, "<new-listen-addr {}", addr)?,
			Self::ExpiredListenAddr(addr) => write!(f, "<expired-listen-addr {}", addr)?,
			Self::ListenerClosed => write!(f, "<listener-closed")?,
			Self::NewExternalAddr(addr) => write!(f, "<new-external-addr {}", addr)?,
			Self::ExpiredExternalAddr(addr) => write!(f, "<expired-external-addr {}", addr)?,
			Self::Discovered(peer) => write!(f, "<discovered {}", peer)?,
			Self::Unreachable(peer) => write!(f, "<unreachable {}", peer)?,
			Self::Connected(peer) => write!(f, "<connected {}", peer)?,
			Self::Disconnected(peer) => write!(f, "<disconnected {}", peer)?,
			Self::Subscribed(peer, topic) => write!(f, "<subscribed {} {}", peer, topic)?,
			Self::Unsubscribed(peer, topic) => write!(f, "<unsubscribed {} {}", peer, topic)?,
			Self::Block(block) => {
				write!(f, "<block {} ", block.cid())?;
				for byte in block.data() {
					write!(f, "{:02x}", byte)?;
				}
			},
			Self::Flushed => write!(f, "<flushed")?,
			Self::Synced => write!(f, "<synced")?,
			Self::Bootstrapped => write!(f, "<bootstrapped")?,
			Self::NewInfo(peer) => write!(f, "<newinfo {}", peer)?,
			Self::Other => write!(f, "<other")?,
		}
		Ok(())
	}
}

const CELL_SIZE: usize = 32;
const PROOF_SIZE: usize = 48;
const CELL_WITH_PROOF_SIZE: usize = CELL_SIZE + PROOF_SIZE;

impl std::str::FromStr for Event {
	type Err = anyhow::Error;

	fn from_str(s: &str) -> anyhow::Result<Self> {
		let mut parts = s.split_whitespace();
		Ok(match parts.next() {
			Some("<new-listener") => Self::NewListener,
			Some("<new-listen-addr") => {
				let addr = parts.next().context("Missing new-listen-addr")?.parse()?;
				Self::NewListenAddr(addr)
			},
			Some("<expired-listen-addr") => {
				let addr = parts
					.next()
					.context("Missing expired-listen-addr")?
					.parse()?;
				Self::ExpiredListenAddr(addr)
			},
			Some("<listener-closed") => Self::ListenerClosed,
			Some("<new-external-addr") => {
				let addr = parts.next().context("Missing new-external-addr")?.parse()?;
				Self::NewExternalAddr(addr)
			},
			Some("<expired-external-addr") => {
				let addr = parts
					.next()
					.context("Missing expired-external-addr")?
					.parse()?;
				Self::ExpiredExternalAddr(addr)
			},
			Some("<discovered") => {
				let peer = parts.next().context("Missing discovered peer")?.parse()?;
				Self::Discovered(peer)
			},
			Some("<unreachable") => {
				let peer = parts.next().context("Missing unreachable peer")?.parse()?;
				Self::Unreachable(peer)
			},
			Some("<connected") => {
				let peer = parts.next().context("Missing connected peer")?.parse()?;
				Self::Connected(peer)
			},
			Some("<disconnected") => {
				let peer = parts.next().context("Missing disconnected peer")?.parse()?;
				Self::Disconnected(peer)
			},
			Some("<subscribed") => {
				let peer = parts.next().context("Missing subscribed peer")?.parse()?;
				let topic = parts
					.next()
					.context("Missing subscribed topic")?
					.to_string();
				Self::Subscribed(peer, topic)
			},
			Some("<unsubscribed") => {
				let peer = parts.next().context("Missing unsubscribed peer")?.parse()?;
				let topic = parts
					.next()
					.context("Missing unsubscribed topic")?
					.to_string();
				Self::Unsubscribed(peer, topic)
			},
			Some("<block") => {
				let cid = parts.next().context("Missing block cid")?.parse()?;
				let str_data = parts.next().context("Missing str_data")?;
				let mut data = Vec::with_capacity(str_data.len() / 2);
				for chunk in str_data.as_bytes().chunks(2) {
					let s = std::str::from_utf8(chunk)?;
					data.push(u8::from_str_radix(s, 16)?);
				}
				let block = IpfsBlock::new(cid, data)?;
				Self::Block(block)
			},
			Some("<flushed") => Self::Flushed,
			Some("<synced") => Self::Synced,
			Some("<bootstrapped") => Self::Bootstrapped,
			Some("<newinfo") => {
				let peer = parts.next().context("Missing newinfo")?.parse()?;
				Self::NewInfo(peer)
			},
			_ => return Err(anyhow::anyhow!("Invalid event `{s}`")),
		})
	}
}

/// Response of RPC get block hash
#[derive(Deserialize, Debug)]
pub struct BlockHashResponse {
	#[serde(flatten)]
	_jsonrpcheader: JsonRPCHeader,
	pub result: String,
}

/// Response of RPC get chain
#[derive(Deserialize, Debug)]
pub struct GetChainResponse {
	#[serde(flatten)]
	_jsonrpcheader: JsonRPCHeader,
	pub result: String,
}

/// Response of RPC get block header
#[derive(Deserialize, Debug)]
pub struct BlockHeaderResponse {
	#[serde(flatten)]
	_jsonrpcheader: JsonRPCHeader,
	pub result: DaHeader,
}

/// Root of extrinsics in header
#[derive(Serialize, Deserialize, Debug, Clone, Decode)]
pub struct ExtrinsicsRoot {
	pub cols: u16,
	pub rows: u16,
	pub hash: String,
	pub commitment: Vec<u8>,
	#[serde(rename = "dataRoot")]
	pub data_root: H256,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Digest {
	logs: Vec<String>,
}

#[derive(Deserialize, Debug)]
struct JsonRPCHeader {
	#[serde(rename = "jsonrpc")]
	_jsonrpc: String,
	#[serde(rename = "id")]
	_id: u32,
}

#[derive(Deserialize)]
struct QueryProofErrorData {
	code: i32,
	message: String,
}

/// Query proof error
#[derive(Deserialize)]
pub struct QueryProofError {
	#[serde(flatten)]
	_jsonrpcheader: JsonRPCHeader,
	error: QueryProofErrorData,
}

impl QueryProofError {
	pub fn message(&self) -> String {
		let code = &self.error.code;
		let message = &self.error.message;
		format!("Query proof failed with code {code}: {message}")
	}
}

/// Query proof result
#[derive(Deserialize)]
pub struct QueryProofResult {
	#[serde(flatten)]
	_jsonrpcheader: JsonRPCHeader,
	pub result: Vec<u8>,
}

impl QueryProofResult {
	pub fn by_cell(&self, cells_len: usize) -> impl Iterator<Item = &[u8; 80]> {
		assert_eq!(CELL_WITH_PROOF_SIZE * cells_len, self.result.len());
		self.result
			.chunks_exact(CELL_WITH_PROOF_SIZE)
			.map(|chunk| chunk.try_into().expect("chunks of 80 bytes size"))
	}
}

/// Response of RPC query proof
#[derive(Deserialize)]
#[serde(untagged)]
pub enum QueryProofResponse {
	Proofs(QueryProofResult),
	Error(QueryProofError),
}

/// Result of subscription to header
#[derive(Deserialize, Debug)]
pub struct QueryResult {
	#[serde(rename = "result")]
	pub header: DaHeader,
	#[serde(rename = "subscription")]
	_subscription: String,
}

/// Response of subscription to header
#[allow(dead_code)]
#[derive(Deserialize, Debug)]
pub struct Response {
	jsonrpc: String,
	method: String,
	pub params: QueryResult,
}

/// Subscription response.
///
/// It is the first response after a call to `subscribe_xxx` on RPC
#[allow(dead_code)]
#[derive(Deserialize, Debug)]
pub struct SubscriptionResponse {
	jsonrpc: String,
	pub id: u32,
	#[serde(rename = "result")]
	pub subscription_id: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RuntimeVersionResponse {
	jsonrpc: String,
	pub result: RuntimeVersionResult,
	id: u32,
}

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
#[derive(Deserialize, Debug)]
pub struct SystemVersionResponse {
	#[serde(flatten)]
	_jsonrpcheader: JsonRPCHeader,
	#[serde(deserialize_with = "deserialise_from_string")]
	pub result: String,
}

fn deserialise_from_string<'de, D>(d: D) -> Result<String, D::Error>
where
	D: Deserializer<'de>,
{
	let mut val = String::deserialize(d)?;
	val.truncate(5);
	Ok(val)
}

/// Light to app client channel message struct
pub struct ClientMsg {
	pub header_hash: H256,
	pub block_num: u32,
	pub dimensions: ExtendedMatrixDimensions,
	pub lookup: AppDataIndex,
	pub commitment: Vec<u8>,
}

impl From<DaHeader> for ClientMsg {
	fn from(header: DaHeader) -> Self {
		let hash: H256 = Encode::using_encoded(&header, |e| blake2_256(e)).into();
		let HeaderExtension::V1(xt) = header.extension;
		let KateCommitment { rows, cols, .. } = xt.commitment;

		ClientMsg {
			header_hash: hash,
			block_num: header.number,
			dimensions: ExtendedMatrixDimensions {
				rows: (rows * 2) as usize,
				cols: cols as usize,
			},
			lookup: AppDataIndex {
				size: xt.app_lookup.size,
				index: xt
					.app_lookup
					.index
					.into_iter()
					.map(|e| (e.app_id.0, e.start))
					.collect(),
			},
			commitment: xt.commitment.commitment,
		}
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

/// Block partition assigned to light client
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Partition {
	pub number: u8,
	pub fraction: u8,
}

mod block_matrix_partition_format {
	use serde::{self, Deserialize, Deserializer, Serializer};

	use super::Partition;

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

fn default_dht_parallelization_limit() -> usize {
	800
}

fn default_query_proof_rpc_parallel_tasks() -> usize {
	8
}

fn default_false() -> bool {
	false
}

/// Representation of a configuration used by this project.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RuntimeConfig {
	/// Light client HTTP server host name (default: 127.0.0.1)
	pub http_server_host: String,
	/// Light client HTTP server port (default: 7000).
	#[serde(default)]
	#[serde(with = "port_range_format")]
	pub http_server_port: (u16, u16),
	/// Seed for IPFS keypair. If not set, or seed is 0, random seed is generated
	pub ipfs_seed: Option<u64>,
	/// IPFS service port range (port, range) (default: 37000).
	#[serde(default)]
	#[serde(with = "port_range_format")]
	pub ipfs_port: (u16, u16),
	/// File system path where IPFS service stores data (default: avail_ipfs_node_1)
	pub ipfs_path: String,
	/// RPC endpoint of a full node for proof queries, etc. (default: http://127.0.0.1:9933).
	pub full_node_rpc: Vec<String>,
	/// WebSocket endpoint of full node for subscribing to latest header, etc (default: ws://127.0.0.1:9944).
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
	#[serde(default = "default_false")]
	pub log_format_json: bool,
	/// Prometheus service port, used for emitting metrics to prometheus server. (default: 9520)
	pub prometheus_port: Option<u16>,
	/// Disables fetching of cells from RPC, set to true if client expects cells to be available in DHT (default: false)
	#[serde(default = "default_false")]
	pub disable_rpc: bool,
	/// Disables proof verification in general, if set to true, otherwise proof verification is performed. (default: false).
	#[serde(default = "default_false")]
	pub disable_proof_verification: bool,
	/// Maximum number of parallel tasks spawned for GET and PUT operations on DHT (default: 800).
	#[serde(default = "default_dht_parallelization_limit")]
	pub dht_parallelization_limit: usize,
	/// Number of parallel queries for cell fetching via RPC from node (default: 8).
	#[serde(default = "default_query_proof_rpc_parallel_tasks")]
	pub query_proof_rpc_parallel_tasks: usize,
	/// Number of seconds to postpone block processing after block finalized message arrives (default: 0)
	pub block_processing_delay: Option<u32>,
	/// Fraction and number of the block matrix part to fetch (e.g. 2/20 means second 1/20 part of a matrix)
	#[serde(default)]
	#[serde(with = "block_matrix_partition_format")]
	pub block_matrix_partition: Option<Partition>,
	/// How many blocks behind latest block to sync. If parameter is empty, or set to 0, synching is disabled (default: 0).
	pub sync_blocks_depth: Option<u32>,
	/// Maximum number of cells per request for proof queries (default: 30).
	pub max_cells_per_rpc: Option<usize>,
	/// Time-to-live for DHT entries in seconds (default: 3600).
	pub ttl: Option<u64>,
}

/// Light client configuration (see [RuntimeConfig] for details)
pub struct LightClientConfig {
	pub full_node_ws: Vec<String>,
	pub confidence: f64,
	pub disable_rpc: bool,
	pub dht_parallelization_limit: usize,
	pub query_proof_rpc_parallel_tasks: usize,
	pub block_processing_delay: Option<u32>,
	pub block_matrix_partition: Option<Partition>,
	pub disable_proof_verification: bool,
	pub max_cells_per_rpc: usize,
	pub ttl: u64,
}

impl From<&RuntimeConfig> for LightClientConfig {
	fn from(val: &RuntimeConfig) -> Self {
		LightClientConfig {
			full_node_ws: val.full_node_ws.clone(),
			confidence: val.confidence,
			disable_rpc: val.disable_rpc,
			dht_parallelization_limit: val.dht_parallelization_limit,
			query_proof_rpc_parallel_tasks: val.query_proof_rpc_parallel_tasks,
			block_processing_delay: val.block_processing_delay,
			block_matrix_partition: val.block_matrix_partition.clone(),
			disable_proof_verification: val.disable_proof_verification,
			max_cells_per_rpc: val.max_cells_per_rpc.unwrap_or(30),
			ttl: val.ttl.unwrap_or(3600),
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
			ttl: val.ttl.unwrap_or(3600),
		}
	}
}

/// App client configuration (see [RuntimeConfig] for details)
pub struct AppClientConfig {
	pub full_node_ws: Vec<String>,
	pub disable_rpc: bool,
	pub dht_parallelization_limit: usize,
	pub ttl: u64,
}

impl From<&RuntimeConfig> for AppClientConfig {
	fn from(val: &RuntimeConfig) -> Self {
		AppClientConfig {
			full_node_ws: val.full_node_ws.clone(),
			disable_rpc: val.disable_rpc,
			dht_parallelization_limit: val.dht_parallelization_limit,
			ttl: val.ttl.unwrap_or(3600),
		}
	}
}

impl Default for RuntimeConfig {
	fn default() -> Self {
		RuntimeConfig {
			http_server_host: "127.0.0.1".to_owned(),
			http_server_port: (7000, 0),
			ipfs_port: (37000, 0),
			ipfs_seed: None,
			ipfs_path: format!("avail_ipfs_node_{}", 1),
			full_node_rpc: vec!["http://127.0.0.1:9933".to_owned()],
			full_node_ws: vec!["ws://127.0.0.1:9944".to_owned()],
			app_id: None,
			confidence: 92.0,
			bootstraps: Vec::new(),
			avail_path: format!("avail_light_client_{}", 1),
			log_level: "INFO".to_owned(),
			log_format_json: false,
			prometheus_port: Some(9520),
			disable_rpc: false,
			disable_proof_verification: false,
			dht_parallelization_limit: 800,
			query_proof_rpc_parallel_tasks: 8,
			block_processing_delay: None,
			block_matrix_partition: None,
			sync_blocks_depth: None,
			max_cells_per_rpc: Some(30),
			ttl: Some(3600),
		}
	}
}

/// This structure is used for encapsulating all things required for
/// querying IPFS client for cell content
/// A specific block number is required
/// In that block row and column numbers are required
/// Finally one channel is also passed which will be used
/// by this message receiver to respond back as an attempt to
/// resolve query
#[derive(Clone)]
pub struct CellContentQueryPayload {
	pub block: u64,
	pub row: u16,
	pub col: u16,
	pub res_chan: std::sync::mpsc::SyncSender<Option<Vec<u8>>>,
}
