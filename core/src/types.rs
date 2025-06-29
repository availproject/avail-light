//! Shared light client structs and enums.
use crate::network::rpc::OutputEvent;
use crate::utils::{blake2_256, extract_app_lookup, extract_kate};
use avail_core::DataLookup;
use avail_rust::{
	avail::runtime_types::bounded_collections::bounded_vec::BoundedVec, AvailHeader, H256,
};
#[cfg(not(target_arch = "wasm32"))]
use kate_recovery::matrix::{Partition, Position};
use sp_core::{bytes, ed25519};

use kate_recovery::{commitments, matrix::Dimensions};

#[cfg(not(target_arch = "wasm32"))]
use avail_rust::{
	subxt_signer::{
		bip39::{Language, Mnemonic},
		SecretString, SecretUri,
	},
	Keypair,
};
use base64::{engine::general_purpose, DecodeError, Engine};
use clap::ValueEnum;
use codec::{Decode, Encode, Input};
use color_eyre::{eyre::eyre, Report, Result};
use convert_case::{Case, Casing};
use derive_more::derive::Display;
use libp2p::kad::Mode as KadMode;
use libp2p::{Multiaddr, PeerId};
use serde::{de::Error, Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use sp_core::crypto::{self, Ss58Codec};
use std::fmt::{self, Display, Formatter};
use std::str::FromStr;
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
#[cfg(target_arch = "wasm32")]
use tokio_with_wasm::alias as tokio;
#[cfg(not(target_arch = "wasm32"))]
use tracing::{info, warn};
#[cfg(target_arch = "wasm32")]
use web_time::{Duration, Instant};

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

#[derive(Clone)]
pub struct Extension {
	pub lookup: DataLookup,
	pub commitments: Vec<[u8; 48]>,
	pub dimensions: Dimensions,
}

/// Light to app client channel message struct
#[derive(Clone)]
pub struct BlockVerified {
	pub header_hash: H256,
	pub block_num: u32,
	pub extension: Option<Extension>,
	pub confidence: Option<f64>,
	pub target_grid_dimensions: Option<Dimensions>,
}
pub struct ClientChannels {
	pub block_sender: broadcast::Sender<BlockVerified>,
	pub rpc_event_receiver: broadcast::Receiver<OutputEvent>,
}

impl TryFrom<(&AvailHeader, Option<f64>)> for BlockVerified {
	type Error = Report;
	fn try_from((header, confidence): (&AvailHeader, Option<f64>)) -> Result<Self, Self::Error> {
		let hash: H256 = Encode::using_encoded(&header, blake2_256).into();
		let mut block = BlockVerified {
			header_hash: hash,
			block_num: header.number,
			confidence,
			extension: None,
			target_grid_dimensions: None,
		};

		let Some((rows, cols, _, commitment)) = extract_kate(&header.extension) else {
			return Ok(block);
		};

		let Some(lookup) = extract_app_lookup(&header.extension)? else {
			return Ok(block);
		};

		let dimensions = Dimensions::new(rows, cols).ok_or_else(|| eyre!("Invalid dimensions"))?;
		block.target_grid_dimensions = Some(dimensions);

		#[cfg(feature = "multiproof")]
		{
			let multiproof_dimensions = crate::utils::generate_multiproof_grid_dims(
				Dimensions::new(16, 64)
					.expect("Failed to generate dimensions for non-extended matrix"),
				dimensions,
			)
			.ok_or_else(|| eyre!("Invalid multiproof dimensions"))?;
			block.target_grid_dimensions = Some(multiproof_dimensions);
		}

		if !lookup.is_empty() {
			block.extension = Some(Extension {
				lookup,
				commitments: commitments::from_slice(&commitment)?,
				dimensions,
			});
		}

		Ok(block)
	}
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, clap::ValueEnum)]
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

#[derive(Serialize, Deserialize, Debug, Display, Clone, Copy, PartialEq, ValueEnum)]
pub enum NetworkMode {
	#[serde(rename = "both")]
	#[value(name = "both")]
	#[display("both")]
	Both,
	#[serde(rename = "p2p-only")]
	#[value(name = "p2p-only")]
	#[display("p2p-only")]
	P2POnly,
	#[serde(rename = "rpc-only")]
	#[value(name = "rpc-only")]
	#[display("rpc-only")]
	RPCOnly,
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

pub mod tracing_level_format {
	use serde::{self, Deserialize, Deserializer, Serializer};
	use std::str::FromStr;
	use tracing::Level;

	pub fn serialize<S>(level: &Level, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		serializer.serialize_str(&level.to_string())
	}

	pub fn deserialize<'de, D>(deserializer: D) -> Result<Level, D::Error>
	where
		D: Deserializer<'de>,
	{
		let value = String::deserialize(deserializer)?;
		Level::from_str(&value).map_err(serde::de::Error::custom)
	}
}

pub mod option_duration_seconds_format {
	use super::duration_seconds_format;
	use super::Duration;
	use serde::{self, Deserialize, Deserializer, Serializer};

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
	use super::Duration;
	use serde::{self, Deserialize, Deserializer, Serializer};

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
	use super::Duration;
	use serde::{self, Deserialize, Deserializer, Serializer};

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

	pub fn serialize<S>(partition: &Partition, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		let Partition { fraction, number } = partition;
		let s = format!("{number}/{fraction}");
		serializer.serialize_str(&s)
	}

	pub fn deserialize<'de, D>(deserializer: D) -> Result<Partition, D::Error>
	where
		D: Deserializer<'de>,
	{
		let value = &String::deserialize(deserializer)?;
		parse(value).map_err(serde::de::Error::custom)
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
pub enum PeerAddress {
	Compact(CompactMultiaddress),
	PeerIdAndMultiaddr((PeerId, Multiaddr)),
}

impl From<&PeerAddress> for (PeerId, Multiaddr) {
	fn from(value: &PeerAddress) -> Self {
		match value {
			PeerAddress::Compact(CompactMultiaddress(value)) => value.clone(),
			PeerAddress::PeerIdAndMultiaddr(value) => value.clone(),
		}
	}
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum SecretKey {
	#[cfg(not(target_arch = "wasm32"))]
	Seed {
		seed: String,
	},
	Key {
		key: String,
	},
}

pub struct Delay(pub Option<Duration>);

impl Delay {
	pub fn sleep_duration(&self, from: Instant) -> Option<Duration> {
		(self.0?)
			.checked_sub(from.elapsed())
			.filter(|duration| !duration.is_zero())
	}
}

/// Sync client configuration (see [RuntimeConfig] for details)
#[derive(Clone)]
pub struct SyncClientConfig {
	pub confidence: f64,
	pub app_id: Option<u32>,
	pub network_mode: NetworkMode,
}

impl Default for SyncClientConfig {
	fn default() -> Self {
		Self {
			confidence: 99.9,
			app_id: None,
			network_mode: NetworkMode::Both,
		}
	}
}

/// App client configuration (see [RuntimeConfig] for details)
pub struct AppClientConfig {
	pub threshold: usize,
	pub network_mode: NetworkMode,
}

impl Default for AppClientConfig {
	fn default() -> Self {
		Self {
			threshold: 5000,
			network_mode: NetworkMode::Both,
		}
	}
}

#[derive(Clone, Copy)]
pub struct MaintenanceConfig {
	pub block_confidence_treshold: f64,
	pub replication_factor: u16,
	pub query_timeout: Duration,
	pub pruning_interval: u32,
	pub automatic_server_mode: bool,
	pub total_memory_gb_threshold: f64,
	pub num_cpus_threshold: usize,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
pub struct IdentityConfig {
	/// Avail account secret key. (secret is generated if it is not configured)
	pub avail_key_pair: Keypair,
	/// Avail ss58 address
	pub avail_address: String,
	/// Avail public key
	pub avail_public_key: String,
}

#[cfg(not(target_arch = "wasm32"))]
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

#[cfg(not(target_arch = "wasm32"))]
impl IdentityConfig {
	pub fn from_suri(suri: String, password: Option<String>) -> Result<Self> {
		let mut suri = SecretUri::from_str(&suri)?;

		if let Some(password) = password {
			suri.password = Some(SecretString::from(password));
		}

		let avail_key_pair = Keypair::from_uri(&suri)?;
		let avail_address = avail_key_pair.public_key().to_account_id();
		let avail_address = crypto::AccountId32::from(avail_address.0).to_ss58check();
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
	pub votes_ancestries: Vec<AvailHeader>,
}

impl<'de> Deserialize<'de> for GrandpaJustification {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		let encoded = bytes::deserialize(deserializer)?;
		Self::decode(&mut &encoded[..])
			.map_err(|codec_err| D::Error::custom(format!("Invalid decoding: {codec_err:?}")))
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

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(try_from = "String", into = "String")]
pub struct Base64(pub Vec<u8>);

impl From<Base64> for BoundedVec<u8> {
	fn from(val: Base64) -> Self {
		BoundedVec(val.0)
	}
}

impl From<Base64> for Vec<u8> {
	fn from(val: Base64) -> Self {
		val.0
	}
}

impl From<&Base64> for Vec<u8> {
	fn from(val: &Base64) -> Self {
		val.0.clone()
	}
}

impl TryFrom<String> for Base64 {
	type Error = DecodeError;

	fn try_from(value: String) -> Result<Self, Self::Error> {
		general_purpose::STANDARD.decode(value).map(Base64)
	}
}

impl From<Base64> for String {
	fn from(value: Base64) -> Self {
		general_purpose::STANDARD.encode(value.0)
	}
}

#[derive(Clone, Debug, derive_more::Display, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct ProjectName(pub String);

impl ProjectName {
	pub fn new<S: Into<String>>(name: S) -> Self {
		ProjectName(name.into().to_case(Case::Snake))
	}

	pub fn gauge_name(&self, name: &'static str) -> String {
		format!("{self}.{name}")
	}
}

impl Default for ProjectName {
	fn default() -> Self {
		ProjectName::new("avail")
	}
}

impl TryFrom<String> for ProjectName {
	type Error = &'static str;

	fn try_from(value: String) -> Result<Self, Self::Error> {
		let formatted_name = value.to_case(Case::Snake);
		if formatted_name.contains(char::is_alphanumeric)
			&& formatted_name
				.chars()
				.all(|c| c.is_alphanumeric() || c == '_')
		{
			Ok(ProjectName(formatted_name))
		} else {
			Err(INVALID_PROJECT_NAME)
		}
	}
}

const INVALID_PROJECT_NAME: &str = r#"
Project name must only contain alphanumeric characters.
"#;

#[cfg(not(target_arch = "wasm32"))]
pub fn iter_partition_cells(partition: Partition, dimensions: Dimensions) -> Vec<Position> {
	if cfg!(feature = "multiproof") {
		dimensions
			.iter_mcell_partition_positions(&partition)
			.collect()
	} else {
		dimensions
			.iter_extended_partition_positions(&partition)
			.collect()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json;

	#[test]
	fn test_project_name_patterns() {
		// Project name with alphanumeric characters and spaces
		let json_data = "\"Project 001\"";
		let deserialized: ProjectName = serde_json::from_str(json_data).unwrap();
		assert_eq!(deserialized.0, "project_001");

		// Project name with caps characters and spaces
		let json_data = "\"PROJECT 002\"";
		let deserialized: ProjectName = serde_json::from_str(json_data).unwrap();
		assert_eq!(deserialized.0, "project_002");

		// Project name with special non-alphanumeric characters (disallowed)
		let json_data = "\"project_002#%\"";
		let deserialized_res: Result<ProjectName, _> = serde_json::from_str(json_data);
		match deserialized_res {
			Err(e) => assert_eq!(e.to_string(), INVALID_PROJECT_NAME.to_string()),
			Ok(_) => panic!("Deserialization should have failed"),
		}
	}
}
