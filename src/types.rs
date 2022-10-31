//! Shared light client structs and enums.

use kate_recovery::com::{AppDataIndex, ExtendedMatrixDimensions};
use libp2p::Multiaddr;
use serde::{Deserialize, Deserializer, Serialize};
use sp_core::H256;

const CELL_SIZE: usize = 32;
const PROOF_SIZE: usize = 48;
const CELL_WITH_PROOF_SIZE: usize = CELL_SIZE + PROOF_SIZE;

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
	pub result: Header,
}

/// Block header
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Header {
	#[serde(deserialize_with = "deserialize_u64_from_hex")]
	pub number: u64,
	#[serde(rename = "extrinsicsRoot")]
	pub extrinsics_root: ExtrinsicsRoot,
	#[serde(rename = "parentHash")]
	parent_hash: String,
	#[serde(rename = "stateRoot")]
	state_root: String,
	digest: Digest,
	#[serde(rename = "appDataLookup")]
	pub app_data_lookup: AppDataIndex,
}

/// Deserializes an hexademial string representation (like "0x12") directly into a `u64`.
fn deserialize_u64_from_hex<'de, D>(d: D) -> Result<u64, D::Error>
where
	D: Deserializer<'de>,
{
	let hex = String::deserialize(d)?;
	u64::from_str_radix(hex.trim_start_matches("0x"), 16).map_err(serde::de::Error::custom)
}

/// Root of extrinsics in header
#[derive(Serialize, Deserialize, Debug, Clone)]
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
	pub header: Header,
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

/// Light to app client channel message struct
pub struct ClientMsg {
	pub number: u64,
	pub dimensions: ExtendedMatrixDimensions,
	pub lookup: AppDataIndex,
	pub commitment: Vec<u8>,
}

impl From<Header> for ClientMsg {
	fn from(header: Header) -> Self {
		let ExtrinsicsRoot { rows, cols, .. } = header.extrinsics_root;

		ClientMsg {
			number: header.number,
			dimensions: ExtendedMatrixDimensions {
				rows: (rows * 2) as usize,
				cols: cols as usize,
			},
			lookup: header.app_data_lookup,
			commitment: header.extrinsics_root.commitment,
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
	/// Seed for Libp2p keypair. If not set, or seed is 0, random seed is generated
	pub libp2p_seed: Option<u8>,
	/// Libp2p service port range (port, range) (default: 37000).
	#[serde(default)]
	#[serde(with = "port_range_format")]
	pub libp2p_port: (u16, u16),
	/// RPC endpoint of a full node for proof queries, etc. (default: http://127.0.0.1:9933).
	pub full_node_rpc: Vec<String>,
	/// File system path where psk key is stored
	pub libp2p_psk_path: String,
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
	pub sync_blocks_depth: Option<u64>,
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
			libp2p_port: (37000, 0),
			libp2p_seed: None,
			libp2p_psk_path: "./avail/psk".to_owned(),
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
