use anyhow::Context;
use ipfs_embed::{Block as IpfsBlock, Cid, DefaultParams, Multiaddr, PeerId};
use kate_recovery::com::{AppDataIndex, ExtendedMatrixDimensions};
use serde::{Deserialize, Deserializer, Serialize};

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

#[derive(Clone)]
pub struct BlockCidPair {
	pub cid: Cid,            // cid of some certain block number's data matrix
	pub self_computed: bool, // is this CID computed by this process ?
}

// Same as above struct, just that it can be easily JSON serialised
// and deserialised, so it's easy to push it into on-disk data store
// where block <-> cid mapping is maintained
#[derive(Clone, Serialize, Deserialize)]
pub struct BlockCidPersistablePair {
	pub cid: String,
	pub self_computed: bool,
}

#[derive(Deserialize, Debug)]
pub struct BlockHashResponse {
	#[serde(flatten)]
	_jsonrpcheader: JsonRPCHeader,
	pub result: String,
}

#[derive(Deserialize, Debug)]
pub struct GetChainResponse {
	#[serde(flatten)]
	_jsonrpcheader: JsonRPCHeader,
	pub result: String,
}

#[derive(Deserialize, Debug)]
pub struct BlockResponse {
	#[serde(flatten)]
	_jsonrpcheader: JsonRPCHeader,
	pub result: RPCResult,
}

#[derive(Deserialize, Debug)]
pub struct BlockHeaderResponse {
	#[serde(flatten)]
	_jsonrpcheader: JsonRPCHeader,
	pub result: Header,
}

#[derive(Deserialize, Debug)]
pub struct RPCResult {
	pub block: Block,
	#[serde(skip_deserializing)]
	pub justification: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Block {
	pub extrinsics: Vec<Extrinsic>,
	pub header: Header,
}

#[derive(Debug, Clone)]
pub struct Extrinsic {
	pub data: Vec<u8>,
}

impl<'a> Deserialize<'a> for Extrinsic {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'a>,
	{
		Ok(Self {
			data: sp_core::bytes::deserialize(deserializer)?,
		})
	}
}

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

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExtrinsicsRoot {
	pub cols: u16,
	pub rows: u16,
	pub hash: String,
	pub commitment: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Digest {
	logs: Vec<String>,
}

#[derive(Deserialize, Debug)]
pub struct JsonRPCHeader {
	#[serde(rename = "jsonrpc")]
	_jsonrpc: String,
	#[serde(rename = "id")]
	_id: u32,
}

#[derive(Deserialize, Debug)]
pub struct BlockProofResponse {
	#[serde(flatten)]
	_jsonrpcheader: JsonRPCHeader,
	pub result: Vec<u8>,
}

impl BlockProofResponse {
	pub fn by_cell(&self, cells_len: usize) -> impl Iterator<Item = &[u8; 80]> {
		assert_eq!(CELL_WITH_PROOF_SIZE * cells_len, self.result.len());
		self.result
			.chunks_exact(CELL_WITH_PROOF_SIZE)
			.map(|chunk| chunk.try_into().expect("chunks of 80 bytes size"))
	}
}

#[derive(Deserialize, Debug)]
pub struct QueryResult {
	#[serde(rename = "result")]
	pub header: Header,
	#[serde(rename = "subscription")]
	_subscription: String,
}

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
		let parts = s.split('/').collect::<Vec<_>>();
		if parts.len() != 2 {
			return Err(serde::de::Error::custom(format!("Invalid value {s}")));
		}
		let number = parts[0].parse::<u8>().map_err(serde::de::Error::custom)?;
		let fraction = parts[1].parse::<u8>().map_err(serde::de::Error::custom)?;
		Ok(Some(Partition { number, fraction }))
	}
}

/// Representation of a configuration used by this project.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RuntimeConfig {
	pub http_server_host: String,
	pub http_server_port: u16,
	pub ipfs_seed: u64,
	pub ipfs_port: u16,
	pub ipfs_path: String,
	pub full_node_rpc: Vec<String>,
	pub full_node_ws: Vec<String>,
	pub app_id: Option<u32>,
	pub confidence: f64,
	pub bootstraps: Vec<(String, Multiaddr)>,
	pub avail_path: String,
	pub log_level: String,
	pub log_format_json: Option<bool>,
	/// Disables fetching of cells from RPC, set to true if client expects cells to be available in DHT
	pub disable_rpc: Option<bool>,
	pub max_parallel_fetch_tasks: usize,
	/// Number of seconds to postpone block processing after block finalized message arrives
	pub block_processing_delay: Option<u32>,
	/// Fraction and number of the block matrix part to fetch (e.g. 2/20 means second 1/20 part of a matrix)
	#[serde(default)]
	#[serde(with = "block_matrix_partition_format")]
	pub block_matrix_partition: Option<Partition>,
}

pub struct LightClientConfig {
	pub full_node_ws: Vec<String>,
	pub confidence: f64,
	pub disable_rpc: bool,
	pub max_parallel_fetch_tasks: usize,
	pub block_processing_delay: Option<u32>,
	pub block_matrix_partition: Option<Partition>,
}

impl From<&RuntimeConfig> for LightClientConfig {
	fn from(val: &RuntimeConfig) -> Self {
		LightClientConfig {
			full_node_ws: val.full_node_ws.clone(),
			confidence: val.confidence,
			disable_rpc: val.disable_rpc == Some(true),
			max_parallel_fetch_tasks: val.max_parallel_fetch_tasks,
			block_processing_delay: val.block_processing_delay,
			block_matrix_partition: val.block_matrix_partition.clone(),
		}
	}
}

pub struct SyncClientConfig {
	pub confidence: f64,
	pub disable_rpc: bool,
	pub max_parallel_fetch_tasks: usize,
}

impl From<&RuntimeConfig> for SyncClientConfig {
	fn from(val: &RuntimeConfig) -> Self {
		SyncClientConfig {
			confidence: val.confidence,
			disable_rpc: val.disable_rpc == Some(true),
			max_parallel_fetch_tasks: val.max_parallel_fetch_tasks,
		}
	}
}

pub struct AppClientConfig {
	pub full_node_ws: Vec<String>,
	pub disable_rpc: bool,
	pub max_parallel_fetch_tasks: usize,
}

impl From<&RuntimeConfig> for AppClientConfig {
	fn from(val: &RuntimeConfig) -> Self {
		AppClientConfig {
			full_node_ws: val.full_node_ws.clone(),
			disable_rpc: val.disable_rpc == Some(true),
			max_parallel_fetch_tasks: val.max_parallel_fetch_tasks,
		}
	}
}

impl Default for RuntimeConfig {
	fn default() -> Self {
		RuntimeConfig {
			http_server_host: "127.0.0.1".to_owned(),
			http_server_port: 7000,
			ipfs_seed: 1,
			ipfs_port: 37000,
			ipfs_path: format!("avail_ipfs_node_{}", 1),
			full_node_rpc: vec!["http://127.0.0.1:9933".to_owned()],
			full_node_ws: vec!["ws://127.0.0.1:9944".to_owned()],

			app_id: None,
			confidence: 92.0,
			bootstraps: Vec::new(),
			avail_path: format!("avail_light_client_{}", 1),
			log_level: "INFO".to_owned(),
			log_format_json: Some(false),
			disable_rpc: Some(false),
			max_parallel_fetch_tasks: 8,
			block_processing_delay: None,
			block_matrix_partition: None,
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
