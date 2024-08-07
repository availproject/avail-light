//! Shared light client structs and enums.
use crate::network::p2p::configuration::LibP2PConfig;
use crate::network::rpc::configuration::RPCConfig;
use crate::network::rpc::Event;
use crate::telemetry::otlp::OtelConfig;
use crate::utils::{extract_app_lookup, extract_kate};
use avail_core::DataLookup;
use avail_subxt::{primitives::Header as DaHeader, utils::H256};
use codec::{Decode, Encode, Input};
use color_eyre::{eyre::eyre, Report, Result};
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
use std::ops::Range;
use std::str::FromStr;
use std::time::{Duration, Instant};
use subxt_signer::bip39::{Language, Mnemonic};
use subxt_signer::sr25519::Keypair;
use subxt_signer::{SecretString, SecretUri};
use tokio::sync::broadcast;
use tracing::{info, warn};

const CELL_SIZE: usize = 32;
const PROOF_SIZE: usize = 48;
pub const CELL_WITH_PROOF_SIZE: usize = CELL_SIZE + PROOF_SIZE;

pub const DEV_FLAG_GENHASH: &str = "DEV";

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

#[derive(Clone, Debug)]
pub struct Extension {
	pub dimensions: Dimensions,
	pub lookup: DataLookup,
	pub commitments: Vec<[u8; 48]>,
}

/// Light to app client channel message struct
#[derive(Clone, Debug)]
pub struct BlockVerified {
	pub header_hash: H256,
	pub block_num: u32,
	pub extension: Option<Extension>,
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
		let mut block = BlockVerified {
			header_hash: hash,
			block_num: header.number,
			extension: None,
			confidence,
		};

		let Some((rows, cols, _, commitment)) = extract_kate(&header.extension) else {
			return Ok(block);
		};

		let Some(lookup) = extract_app_lookup(&header.extension)? else {
			return Ok(block);
		};

		if !lookup.is_empty() {
			block.extension = Some(Extension {
				dimensions: Dimensions::new(rows, cols)
					.ok_or_else(|| eyre!("Invalid dimensions"))?,
				lookup,
				commitments: commitments::from_slice(&commitment)?,
			});
		}

		Ok(block)
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
			Some(app_id) => Mode::AppClient(app_id),
		}
	}
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(try_from = "String")]
pub enum Origin {
	Internal,
	FatClient,
	External,
	Other(String),
}

impl TryFrom<String> for Origin {
	type Error = color_eyre::Report;

	fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
		match value.to_lowercase().as_str() {
			"internal" => Ok(Origin::Internal),
			"fatclient" => Ok(Origin::FatClient),
			"external" => Ok(Origin::External),
			_ => Ok(Origin::Other(value.to_lowercase())),
		}
	}
}

impl Display for Origin {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		f.write_str(match self {
			Origin::Internal => "internal",
			Origin::FatClient => "fatclient",
			Origin::External => "external",
			Origin::Other(val) => val,
		})
	}
}

pub mod option_duration_seconds_format {
	use super::duration_seconds_format;
	use serde::{self, Deserialize, Deserializer, Serializer};
	use std::time::Duration;

	pub fn serialize<S>(duration: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		match duration {
			Some(duration) => duration_seconds_format::serialize(duration, serializer),
			None => serializer.serialize_none(),
		}
	}

	pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
	where
		D: Deserializer<'de>,
	{
		let value = Option::<u64>::deserialize(deserializer)?;
		Ok(value.map(Duration::from_secs))
	}
}

pub mod duration_seconds_format {
	use serde::{self, Deserialize, Deserializer, Serializer};
	use std::time::Duration;

	pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		serializer.serialize_u64(duration.as_secs())
	}

	pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
	where
		D: Deserializer<'de>,
	{
		let value = u64::deserialize(deserializer)?;
		Ok(Duration::from_secs(value))
	}
}

pub mod duration_millis_format {
	use serde::{self, Deserialize, Deserializer, Serializer};
	use std::time::Duration;

	pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		serializer.serialize_u64(duration.as_millis() as u64)
	}

	pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
	where
		D: Deserializer<'de>,
	{
		let value = u64::deserialize(deserializer)?;
		Ok(Duration::from_millis(value))
	}
}

pub mod block_matrix_partition_format {
	use kate_recovery::matrix::Partition;
	use serde::{self, Deserialize, Deserializer, Serializer};

	pub fn parse(value: &str) -> Result<Partition, String> {
		let value = value
			.split('/')
			.map(str::parse::<u8>)
			.collect::<Result<Vec<u8>, _>>()
			.map_err(|error| error.to_string())?;

		match value[..] {
			[0, _] | [_, 0] => Err("Partition number or fraction cannot be 0".to_string()),
			[num, frac] if num > frac => Err(format!("Invalid partition: {num}/{frac}")),
			[number, fraction] => Ok(Partition { number, fraction }),
			_ => Err(format!("Invalid partition parameter: {value:?})")),
		}
	}

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
		let value = &String::deserialize(deserializer)?;
		if value.is_empty() || value.to_ascii_lowercase().contains("none") {
			return Ok(None);
		}

		parse(value).map(Some).map_err(serde::de::Error::custom)
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

/// Representation of a configuration used by this project.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct RuntimeConfig {
	/// Light client HTTP server host name (default: 127.0.0.1).
	pub http_server_host: String,
	/// Light client HTTP server port (default: 7007).
	pub http_server_port: u16,
	#[serde(flatten)]
	pub libp2p: LibP2PConfig,
	#[serde(flatten)]
	pub rpc: RPCConfig,
	/// Genesis hash of the network to be connected to. Set to a string beginning with "DEV" to connect to any network.
	pub genesis_hash: String,
	/// If set, application client is started with given app_id (default: None).
	pub app_id: Option<u32>,
	/// Confidence threshold, used to calculate how many cells need to be sampled to achieve desired confidence (default: 92.0).
	pub confidence: f64,
	/// File system path where RocksDB used by light client, stores its data.
	pub avail_path: String,
	/// Log level, default is `INFO`. See `<https://docs.rs/log/0.4.14/log/enum.LevelFilter.html>` for possible log level values. (default: `INFO`).
	pub log_level: String,
	pub origin: Origin,
	/// If set to true, logs are displayed in JSON format, which is used for structured logging. Otherwise, plain text format is used (default: false).
	pub log_format_json: bool,
	#[serde(flatten)]
	pub otel: OtelConfig,
	pub ot_flush_block_interval: u32,
	pub total_memory_gb_threshold: f64,
	pub num_cpus_threshold: usize,
	/// Disables fetching of cells from RPC, set to true if client expects cells to be available in DHT (default: false).
	pub disable_rpc: bool,
	/// Number of parallel queries for cell fetching via RPC from node (default: 8).
	pub query_proof_rpc_parallel_tasks: usize,
	/// Number of seconds to postpone block processing after block finalized message arrives (default: 20).
	#[serde(with = "option_duration_seconds_format")]
	pub block_processing_delay: Option<Duration>,
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
	#[cfg(feature = "crawl")]
	#[serde(flatten)]
	pub crawl: crate::crawl_client::CrawlConfig,
	/// Client alias for use in logs and metrics
	pub client_alias: Option<String>,
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
		let block_processing_delay = val.block_processing_delay;
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
	pub query_proof_rpc_parallel_tasks: usize,
	pub block_processing_delay: Delay,
	pub block_matrix_partition: Option<Partition>,
	pub max_cells_per_rpc: usize,
}

impl From<&RuntimeConfig> for FatClientConfig {
	fn from(val: &RuntimeConfig) -> Self {
		FatClientConfig {
			full_nodes_ws: val.rpc.full_node_ws.clone(),
			confidence: val.confidence,
			disable_rpc: val.disable_rpc,
			query_proof_rpc_parallel_tasks: val.query_proof_rpc_parallel_tasks,
			block_processing_delay: Delay(val.block_processing_delay),
			block_matrix_partition: val.block_matrix_partition,
			max_cells_per_rpc: val.max_cells_per_rpc.unwrap_or(30),
		}
	}
}

/// Sync client configuration (see [RuntimeConfig] for details)
#[derive(Clone)]
pub struct SyncClientConfig {
	pub confidence: f64,
	pub disable_rpc: bool,
	pub is_last_step: bool,
}

impl From<&RuntimeConfig> for SyncClientConfig {
	fn from(val: &RuntimeConfig) -> Self {
		SyncClientConfig {
			confidence: val.confidence,
			disable_rpc: val.disable_rpc,
			is_last_step: val.app_id.is_none(),
		}
	}
}

/// App client configuration (see [RuntimeConfig] for details)
pub struct AppClientConfig {
	pub disable_rpc: bool,
	pub threshold: usize,
}

impl From<&RuntimeConfig> for AppClientConfig {
	fn from(val: &RuntimeConfig) -> Self {
		AppClientConfig {
			disable_rpc: val.disable_rpc,
			threshold: val.threshold,
		}
	}
}

#[derive(Clone, Copy)]
pub struct MaintenanceConfig {
	pub block_confidence_treshold: f64,
	pub replication_factor: u16,
	pub query_timeout: Duration,
	pub pruning_interval: u32,
	pub telemetry_flush_interval: u32,
	pub automatic_server_mode: bool,
	pub total_memory_gb_threshold: f64,
	pub num_cpus_threshold: usize,
}

impl From<&RuntimeConfig> for MaintenanceConfig {
	fn from(val: &RuntimeConfig) -> Self {
		MaintenanceConfig {
			block_confidence_treshold: val.confidence,
			replication_factor: val.libp2p.kademlia.record_replication_factor.get() as u16,
			query_timeout: val.libp2p.kademlia.query_timeout,
			pruning_interval: val.libp2p.kademlia.store_pruning_interval,
			telemetry_flush_interval: val.ot_flush_block_interval,
			automatic_server_mode: val.libp2p.kademlia.automatic_server_mode,
			total_memory_gb_threshold: val.total_memory_gb_threshold,
			num_cpus_threshold: val.num_cpus_threshold,
		}
	}
}

impl Default for RuntimeConfig {
	fn default() -> Self {
		RuntimeConfig {
			http_server_host: "127.0.0.1".to_owned(),
			http_server_port: 7007,
			libp2p: Default::default(),
			rpc: Default::default(),
			genesis_hash: "DEV".to_owned(),
			app_id: None,
			confidence: 99.9,
			avail_path: "avail_path".to_owned(),
			log_level: "INFO".to_owned(),
			log_format_json: false,
			otel: Default::default(),
			ot_flush_block_interval: 15,
			total_memory_gb_threshold: 16.0,
			num_cpus_threshold: 4,
			disable_rpc: false,
			query_proof_rpc_parallel_tasks: 8,
			block_processing_delay: Some(Duration::from_secs(20)),
			block_matrix_partition: None,
			sync_start_block: None,
			sync_finality_enable: false,
			max_cells_per_rpc: Some(30),
			threshold: 5000,
			#[cfg(feature = "crawl")]
			crawl: crate::crawl_client::CrawlConfig::default(),
			origin: Origin::External,
			client_alias: None,
		}
	}
}

impl RuntimeConfig {
	/// A range bounded inclusively below and exclusively above
	pub fn sync_range(&self, end: u32) -> Range<u32> {
		let start = self.sync_start_block.unwrap_or(end);
		Range { start, end }
	}
}

pub struct IdentityConfig {
	/// Avail account secret key. (secret is generated if it is not configured)
	pub avail_key_pair: Keypair,
	/// Avail ss58 address
	pub avail_address: String,
	/// Avail public key
	pub avail_public_key: String,
}

pub fn load_or_init_suri(path: &str) -> Result<String> {
	#[derive(Default, Serialize, Deserialize)]
	struct Config {
		pub avail_secret_uri: Option<String>,
		// TODO: Deprecated since 1.9.0, remove it once it is safe
		pub avail_secret_seed_phrase: Option<String>,
	}

	impl Config {
		fn avail_secret_uri(&self) -> Option<String> {
			self.avail_secret_uri
				.as_ref()
				.or(self.avail_secret_seed_phrase.as_ref())
				.cloned()
		}
	}

	let mut config: Config = confy::load_path(path)?;
	info!("Identity loaded from {path}");

	if config.avail_secret_seed_phrase.is_some() {
		warn!("Using deprecated configuration parameter `avail_secret_seed_phrase`, use `avail_secret_uri` instead.");
	}

	if let Some(suri) = config.avail_secret_uri() {
		return Ok(suri);
	};

	let mnemonic = Mnemonic::generate_in(Language::English, 24)?;
	config.avail_secret_uri = Some(mnemonic.to_string());
	confy::store_path(path, &config)?;
	Ok(mnemonic.to_string())
}

impl IdentityConfig {
	pub fn from_suri(suri: String, password: Option<&String>) -> Result<Self> {
		let mut suri = SecretUri::from_str(&suri)?;

		if let Some(password) = password {
			suri.password = Some(SecretString::from_str(password)?);
		}

		let avail_key_pair = Keypair::from_uri(&suri)?;
		let avail_address = avail_key_pair.public_key().to_account_id();
		let avail_address = sp_core::crypto::AccountId32::from(avail_address.0).to_ss58check();
		let avail_public_key = hex::encode(avail_key_pair.public_key());

		Ok(IdentityConfig {
			avail_key_pair,
			avail_address,
			avail_public_key,
		})
	}
}

#[derive(Clone, Debug, Serialize, Deserialize, Decode, Encode)]
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

#[derive(Debug, Encode)]
pub enum SignerMessage {
	_DummyMessage(u32),
	PrecommitMessage(Precommit),
}

#[derive(Clone, Debug, Decode, Encode, Serialize, Deserialize)]
pub struct Precommit {
	pub target_hash: H256,
	/// The target block's number
	pub target_number: u32,
}

#[derive(Clone, Debug, Decode, Encode, Serialize, Deserialize)]
pub struct SignedPrecommit {
	pub precommit: Precommit,
	/// The signature on the message.
	pub signature: ed25519::Signature,
	/// The Id of the signer.
	pub id: ed25519::Public,
}
#[derive(Clone, Debug, Decode, Encode, Serialize, Deserialize)]
pub struct Commit {
	pub target_hash: H256,
	/// The target block's number.
	pub target_number: u32,
	/// Precommits for target block or any block after it that justify this commit.
	pub precommits: Vec<SignedPrecommit>,
}

#[derive(Clone, Debug, Decode, Encode, Serialize)]
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

#[derive(Clone, Serialize, Deserialize)]
pub struct Uuid(uuid::Uuid);

impl Uuid {
	pub fn new_v4() -> Uuid {
		Self(uuid::Uuid::new_v4())
	}
}

impl Display for Uuid {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(&self.0.to_string())
	}
}

impl Decode for Uuid {
	fn decode<I: Input>(input: &mut I) -> Result<Self, codec::Error> {
		let mut bytes = [0u8; 16];
		input.read(&mut bytes)?;
		uuid::Uuid::from_slice(&bytes)
			.map_err(|_| codec::Error::from("failed to decode uuid"))
			.map(Uuid)
	}
}

impl Encode for Uuid {
	fn encode(&self) -> Vec<u8> {
		self.0.as_bytes().to_vec()
	}
}
