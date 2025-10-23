use avail_core::kate::COMMITMENT_SIZE;
use avail_rust::{
	avail::runtime_types::avail_core::{
		data_lookup::compact::CompactDataLookup, header::extension::HeaderExtension,
	},
	subxt::config::substrate::{Digest as ApiDigest, DigestItem as ApiDigestItem},
	AvailHeader, H256,
};
use codec::Encode;
#[cfg(not(target_arch = "wasm32"))]
use color_eyre::eyre::eyre;
use color_eyre::{eyre::WrapErr, Report, Result};
use derive_more::From;
#[cfg(not(target_arch = "wasm32"))]
use hyper::{http, StatusCode};
use kate_recovery::{com::AppData, commitments};
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashSet;
#[cfg(not(target_arch = "wasm32"))]
use std::{collections::HashMap, sync::Arc};
#[cfg(not(target_arch = "wasm32"))]
use tokio::sync::{mpsc::UnboundedSender, RwLock};
use uuid::Uuid;
#[cfg(not(target_arch = "wasm32"))]
use warp::{
	ws::{self, Message},
	Reply,
};

use crate::{
	api::configuration::SharedConfig,
	data::{
		AchievedConfidenceKey, AchievedSyncConfidenceKey, Database, IsSyncedKey, LatestHeaderKey,
		LatestSyncKey, RpcNodeKey, VerifiedDataKey, VerifiedHeaderKey, VerifiedSyncDataKey,
		VerifiedSyncHeaderKey,
	},
	network::rpc::OutputEvent as RpcEvent,
	types::{self, Base64, BlockProcessed, BlockVerified},
	utils::{blake2_256, decode_app_data, OptionalExtension},
};

#[derive(Debug)]
pub struct InternalServerError {}

#[cfg(not(target_arch = "wasm32"))]
impl warp::reject::Reject for InternalServerError {}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Version {
	pub version: String,
	pub network_version: String,
}

#[cfg(not(target_arch = "wasm32"))]
impl Reply for Version {
	fn into_response(self) -> warp::reply::Response {
		warp::reply::json(&self).into_response()
	}
}

#[derive(Serialize, Deserialize)]
pub struct BlockRange {
	pub first: u32,
	pub last: u32,
}

impl From<types::BlockRange> for BlockRange {
	fn from(value: types::BlockRange) -> Self {
		BlockRange {
			first: value.first,
			last: value.last,
		}
	}
}

#[derive(Serialize, Deserialize)]
pub struct HistoricalSync {
	pub synced: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub available: Option<BlockRange>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub app_data: Option<BlockRange>,
}

#[derive(Serialize, Deserialize)]
pub struct Blocks {
	pub latest: u32,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub available: Option<BlockRange>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub app_data: Option<BlockRange>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub historical_sync: Option<HistoricalSync>,
}

#[derive(Serialize, Deserialize)]
pub struct Status {
	pub modes: Vec<Mode>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub app_id: Option<u32>,
	pub genesis_hash: String,
	pub network: String,
	pub blocks: Blocks,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transaction {
	Data(Base64),
	Extrinsic(Base64),
}

impl Transaction {
	pub fn is_empty(&self) -> bool {
		match self {
			Transaction::Data(data) => data.0.is_empty(),
			Transaction::Extrinsic(data) => data.0.is_empty(),
		}
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubmitResponse {
	pub block_number: u32,
	pub block_hash: H256,
	pub hash: H256,
	pub index: u32,
}

#[cfg(not(target_arch = "wasm32"))]
impl Reply for SubmitResponse {
	fn into_response(self) -> warp::reply::Response {
		warp::reply::json(&self).into_response()
	}
}

impl Status {
	pub fn new(config: &SharedConfig, db: impl Database) -> Self {
		let historical_sync = db.get(IsSyncedKey).map(|synced| HistoricalSync {
			synced,
			available: db.get(AchievedSyncConfidenceKey).map(From::from),
			app_data: db.get(VerifiedSyncDataKey).map(From::from),
		});

		let blocks = Blocks {
			latest: db.get(LatestHeaderKey).unwrap_or_default(),
			available: db.get(AchievedConfidenceKey).map(From::from),
			app_data: db.get(VerifiedDataKey).map(From::from),
			historical_sync,
		};

		let node = db.get(RpcNodeKey).unwrap_or_default();

		Status {
			modes: config.into(),
			app_id: config.app_id,
			genesis_hash: format!("{:?}", node.genesis_hash),
			network: node.network(),
			blocks,
		}
	}
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
	Light,
	App,
	Partition,
}

impl From<&SharedConfig> for Vec<Mode> {
	fn from(value: &SharedConfig) -> Self {
		match value.app_id {
			None => vec![Mode::Light],
			Some(_) => vec![Mode::Light, Mode::App],
		}
	}
}

#[cfg(not(target_arch = "wasm32"))]
impl Reply for Status {
	fn into_response(self) -> warp::reply::Response {
		warp::reply::json(&self).into_response()
	}
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum Topic {
	HeaderVerified,
	ConfidenceAchieved,
	DataVerified,
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum DataField {
	Data,
	Extrinsic,
}

#[derive(Serialize, Deserialize, PartialEq, Default)]
pub struct Subscription {
	pub topics: HashSet<Topic>,
	pub data_fields: HashSet<DataField>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HeaderMessage {
	block_number: u32,
	header: Header,
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Hash, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum BlockStatus {
	Unavailable,
	Pending,
	VerifyingHeader,
	VerifyingConfidence,
	VerifyingData,
	Incomplete,
	Finished,
}

pub fn block_status(
	sync_start_block: &Option<u32>,
	db: impl Database,
	block_number: u32,
	extension: impl OptionalExtension,
) -> Option<BlockStatus> {
	let latest = db.get(LatestHeaderKey).unwrap_or_default();
	if block_number > latest {
		return None;
	};

	let first_block = db
		.get(VerifiedHeaderKey)
		.map_or(latest, |range| range.first);

	let first_sync_block = sync_start_block.unwrap_or(first_block);

	if block_number < first_sync_block {
		return Some(BlockStatus::Unavailable);
	};

	if extension.option().is_none() {
		return Some(BlockStatus::Incomplete);
	};

	if block_number < first_block {
		let has_verified_sync_data = db
			.get(VerifiedSyncDataKey)
			.is_some_and(|range| range.contains(block_number));

		if has_verified_sync_data {
			return Some(BlockStatus::Finished);
		}

		let has_achieved_sync_confidence = db
			.get(AchievedSyncConfidenceKey)
			.is_some_and(|range| range.contains(block_number));

		if has_achieved_sync_confidence {
			return Some(BlockStatus::VerifyingData);
		}

		let has_verified_sync_header = db
			.get(VerifiedSyncHeaderKey)
			.is_some_and(|range| range.contains(block_number));

		if has_verified_sync_header {
			return Some(BlockStatus::VerifyingConfidence);
		}

		let latest_sync = db.get(LatestSyncKey);
		let is_sync_latest = latest_sync.map(|latest| block_number == latest);
		if is_sync_latest.unwrap_or(false) {
			return Some(BlockStatus::VerifyingHeader);
		}
	} else {
		let has_verified_data = db
			.get(VerifiedDataKey)
			.is_some_and(|range| range.contains(block_number));

		if has_verified_data {
			return Some(BlockStatus::Finished);
		}

		let has_achieved_confidence = db
			.get(AchievedConfidenceKey)
			.is_some_and(|range| range.contains(block_number));

		if has_achieved_confidence {
			return Some(BlockStatus::VerifyingData);
		}

		let has_verified_header = db
			.get(VerifiedHeaderKey)
			.is_some_and(|range| range.contains(block_number));

		if has_verified_header {
			return Some(BlockStatus::VerifyingConfidence);
		}
		if latest == block_number {
			return Some(BlockStatus::VerifyingHeader);
		}
	}

	Some(BlockStatus::Pending)
}

#[derive(Serialize, Deserialize, PartialEq)]
pub struct Block {
	pub status: BlockStatus,
	pub confidence: Option<f64>,
}

impl Block {
	pub fn new(status: BlockStatus, confidence: Option<f64>) -> Self {
		Self { status, confidence }
	}
}

#[cfg(not(target_arch = "wasm32"))]
impl Reply for Block {
	fn into_response(self) -> warp::reply::Response {
		warp::reply::json(&self).into_response()
	}
}

impl TryFrom<(AvailHeader, u64)> for HeaderMessage {
	type Error = Report;

	fn try_from((header, received_at): (AvailHeader, u64)) -> Result<Self, Self::Error> {
		let header: Header = (header, received_at).try_into()?;
		Ok(Self {
			block_number: header.number,
			header,
		})
	}
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Header {
	hash: H256,
	parent_hash: H256,
	pub number: u32,
	state_root: H256,
	extrinsics_root: H256,
	extension: Extension,
	digest: Digest,
	received_at: u64,
}

#[cfg(not(target_arch = "wasm32"))]
impl Reply for Header {
	fn into_response(self) -> warp::reply::Response {
		warp::reply::json(&self).into_response()
	}
}

#[derive(Debug, Clone)]
struct Commitment([u8; COMMITMENT_SIZE]);

impl Serialize for Commitment {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		let hex_string = format!("0x{}", hex::encode(self.0));
		serializer.serialize_str(&hex_string)
	}
}

impl<'de> Deserialize<'de> for Commitment {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		const PREFIX_0X_LEN: usize = 2;
		const HEX_ENCODED_BYTE_LEN: usize = 2;
		const LEN: usize = (COMMITMENT_SIZE * HEX_ENCODED_BYTE_LEN) + PREFIX_0X_LEN;

		let s = String::deserialize(deserializer)?;

		if !s.starts_with("0x") || s.len() != LEN {
			let message = "Expected a hex string of correct length with 0x prefix";
			return Err(de::Error::custom(message));
		}

		let decoded = hex::decode(&s[2..]).map_err(de::Error::custom)?;
		let decoded_len = decoded.len();
		let bytes: [u8; COMMITMENT_SIZE] = decoded
			.try_into()
			.map_err(|_| de::Error::invalid_length(decoded_len, &"Expected vector of 48 bytes"))?;

		Ok(Commitment(bytes))
	}
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Extension {
	rows: u16,
	cols: u16,
	data_root: H256,
	commitments: Vec<Commitment>,
	app_lookup: CompactDataLookup,
}

impl TryFrom<(AvailHeader, u64)> for Header {
	type Error = Report;

	fn try_from((header, received_at): (AvailHeader, u64)) -> Result<Self> {
		Ok(Header {
			hash: Encode::using_encoded(&header, blake2_256).into(),
			parent_hash: header.parent_hash,
			number: header.number,
			state_root: header.state_root,
			extrinsics_root: header.extrinsics_root,
			extension: header.extension.try_into()?,
			digest: header.digest.try_into()?,
			received_at,
		})
	}
}

impl TryFrom<HeaderExtension> for Extension {
	type Error = Report;

	fn try_from(value: HeaderExtension) -> Result<Self, Self::Error> {
		match value {
			HeaderExtension::V3(v3) => {
				let commitments = commitments::from_slice(&v3.commitment.commitment)?
					.into_iter()
					.map(Commitment)
					.collect::<Vec<_>>();

				Ok(Extension {
					rows: v3.commitment.rows,
					cols: v3.commitment.cols,
					data_root: v3.commitment.data_root,
					commitments,
					app_lookup: v3.app_lookup,
				})
			},
		}
	}
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Digest {
	pub logs: Vec<DigestItem>,
}

pub type ConsensusEngineId = [u8; 4];

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
pub enum DigestItem {
	PreRuntime(ConsensusEngineId, Vec<u8>),
	Consensus(ConsensusEngineId, Vec<u8>),
	Seal(ConsensusEngineId, Vec<u8>),
	Other(Vec<u8>),
	RuntimeEnvironmentUpdated,
}

impl TryFrom<ApiDigest> for Digest {
	type Error = Report;

	fn try_from(digest: ApiDigest) -> Result<Self, Self::Error> {
		Ok(Digest {
			logs: digest
				.logs
				.into_iter()
				.map(|log| match log {
					ApiDigestItem::PreRuntime(id, items) => DigestItem::PreRuntime(id, items),
					ApiDigestItem::Consensus(id, items) => DigestItem::Consensus(id, items),
					ApiDigestItem::Seal(id, items) => DigestItem::Seal(id, items),
					ApiDigestItem::Other(items) => DigestItem::Other(items),
					ApiDigestItem::RuntimeEnvironmentUpdated => {
						DigestItem::RuntimeEnvironmentUpdated
					},
				})
				.collect(),
		})
	}
}

impl TryFrom<RpcEvent> for Option<PublishMessage> {
	type Error = Report;

	fn try_from(value: RpcEvent) -> Result<Self, Self::Error> {
		match value {
			RpcEvent::HeaderUpdate {
				header,
				received_at: _,
				received_at_timestamp,
			} => (header, received_at_timestamp)
				.try_into()
				.map(Box::new)
				.map(PublishMessage::HeaderVerified)
				.map(Some),
			RpcEvent::InitialConnection(_) | RpcEvent::SwitchedConnection(_) => Ok(None), // skip all the rest of events
		}
	}
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ConfidenceMessage {
	block_number: u32,
	#[serde(skip_serializing_if = "Option::is_none")]
	confidence: Option<f64>,
}

impl TryFrom<BlockVerified> for Option<PublishMessage> {
	type Error = Report;

	fn try_from(value: BlockVerified) -> Result<Self, Self::Error> {
		Ok(Some(PublishMessage::ConfidenceAchieved(
			ConfidenceMessage {
				block_number: value.block_num,
				confidence: value.confidence,
			},
		)))
	}
}

impl TryFrom<BlockProcessed> for Option<PublishMessage> {
	type Error = Report;

	fn try_from(value: BlockProcessed) -> Result<Self, Self::Error> {
		match value {
			BlockProcessed::Verified(block) => Ok(Some(PublishMessage::ConfidenceAchieved(
				ConfidenceMessage {
					block_number: block.block_num,
					confidence: block.confidence,
				},
			))),
			BlockProcessed::Skipped { .. } => Ok(None),
		}
	}
}

#[derive(Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct FieldsQueryParameter(pub HashSet<DataField>);

impl TryFrom<String> for FieldsQueryParameter {
	type Error = Report;

	fn try_from(value: String) -> Result<Self, Self::Error> {
		value
			.split(',')
			.map(|part| format!(r#""{part}""#))
			.map(|part| serde_json::from_str(&part).wrap_err("Cannot deserialize field"))
			.collect::<Result<HashSet<_>>>()
			.map(FieldsQueryParameter)
	}
}

#[derive(Serialize, Deserialize)]
pub struct DataQuery {
	pub fields: Option<FieldsQueryParameter>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DataResponse {
	pub block_number: u32,
	pub data_transactions: Vec<DataTransaction>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Reply for DataResponse {
	fn into_response(self) -> warp::reply::Response {
		warp::reply::json(&self).into_response()
	}
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DataMessage {
	block_number: u32,
	data_transactions: Vec<DataTransaction>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DataTransaction {
	#[serde(skip_serializing_if = "Option::is_none")]
	data: Option<Base64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	extrinsic: Option<Base64>,
}

impl TryFrom<Vec<u8>> for DataTransaction {
	type Error = Report;

	fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
		Ok(DataTransaction {
			data: decode_app_data(&value)?.map(Base64),
			extrinsic: Some(Base64(value)),
		})
	}
}

pub fn filter_fields(data_transactions: &mut [DataTransaction], fields: &HashSet<DataField>) {
	if !fields.contains(&DataField::Extrinsic) {
		for transaction in data_transactions.iter_mut() {
			transaction.extrinsic = None
		}
	}
	if !fields.contains(&DataField::Data) && fields.contains(&DataField::Extrinsic) {
		for transaction in data_transactions.iter_mut() {
			transaction.data = None
		}
	}
}

#[derive(Clone)]
pub struct ApiData(pub u32, pub AppData);

impl TryFrom<ApiData> for Option<PublishMessage> {
	type Error = Report;

	fn try_from(ApiData(block_number, app_data): ApiData) -> Result<Self, Self::Error> {
		let data_transactions = app_data
			.into_iter()
			.map(TryFrom::try_from)
			.collect::<Result<Vec<_>>>()?;

		Ok(Some(PublishMessage::DataVerified(DataMessage {
			block_number,
			data_transactions,
		})))
	}
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(tag = "topic", content = "message", rename_all = "kebab-case")]
pub enum PublishMessage {
	HeaderVerified(Box<HeaderMessage>),
	ConfidenceAchieved(ConfidenceMessage),
	DataVerified(DataMessage),
}

#[cfg(not(target_arch = "wasm32"))]
impl PublishMessage {
	fn apply_filter(&mut self, fields: &HashSet<DataField>) {
		match self {
			PublishMessage::HeaderVerified(_) => (),
			PublishMessage::ConfidenceAchieved(_) => (),
			PublishMessage::DataVerified(data) => {
				filter_fields(&mut data.data_transactions, fields)
			},
		}
	}
}

#[cfg(not(target_arch = "wasm32"))]
impl TryFrom<PublishMessage> for Message {
	type Error = Report;
	fn try_from(value: PublishMessage) -> Result<Self, Self::Error> {
		serde_json::to_string(&value)
			.map(ws::Message::text)
			.wrap_err("Cannot serialize publish message")
	}
}

#[cfg(not(target_arch = "wasm32"))]
pub type Sender = UnboundedSender<Result<ws::Message, warp::Error>>;

#[cfg(not(target_arch = "wasm32"))]
pub struct WsClient {
	pub subscription: Subscription,
	pub sender: Option<Sender>,
}

#[cfg(not(target_arch = "wasm32"))]
impl WsClient {
	pub fn new(subscription: Subscription) -> Self {
		WsClient {
			subscription,
			sender: None,
		}
	}

	fn is_subscribed(&self, topic: &Topic) -> bool {
		self.subscription.topics.contains(topic)
	}

	fn sender_with_data_fields(&self) -> Option<(&Sender, &HashSet<DataField>)> {
		self.sender
			.as_ref()
			.map(|sender| (sender, &self.subscription.data_fields))
	}
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
pub struct WsClients(pub Arc<RwLock<HashMap<String, WsClient>>>);

#[cfg(not(target_arch = "wasm32"))]
impl WsClients {
	pub async fn set_sender(&self, subscription_id: &str, sender: Sender) -> Result<()> {
		let mut clients = self.0.write().await;
		let Some(client) = clients.get_mut(subscription_id) else {
			return Err(eyre!("Client is not subscribed"));
		};
		client.sender = Some(sender);
		Ok(())
	}

	pub async fn has_subscription(&self, subscription_id: &str) -> bool {
		self.0.read().await.contains_key(subscription_id)
	}

	pub async fn subscribe(&self, subscription_id: &str, subscription: Subscription) {
		let mut clients = self.0.write().await;
		clients.insert(subscription_id.to_string(), WsClient::new(subscription));
	}

	pub async fn publish(&self, topic: &Topic, message: PublishMessage) -> Result<Vec<Result<()>>> {
		let clients = self.0.read().await;
		Ok(clients
			.iter()
			.filter(|(_, client)| client.is_subscribed(topic))
			.flat_map(|(_, client)| client.sender_with_data_fields())
			.map(|(sender, data_fields)| {
				let mut message = message.clone();
				message.apply_filter(data_fields);
				message
					.try_into()
					.wrap_err("Cannot convert to ws message")
					.and_then(|message: warp::ws::Message| {
						sender.send(Ok(message)).wrap_err("Send failed")
					})
			})
			.collect::<Vec<_>>())
	}
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for WsClients {
	fn default() -> Self {
		Self(Arc::new(RwLock::new(HashMap::new())))
	}
}

#[derive(Serialize, Deserialize)]
pub struct SubscriptionId {
	pub subscription_id: String,
}

#[cfg(not(target_arch = "wasm32"))]
impl Reply for SubscriptionId {
	fn into_response(self) -> warp::reply::Response {
		warp::reply::json(&self).into_response()
	}
}

#[derive(Deserialize)]
#[serde(tag = "type", content = "message", rename_all = "kebab-case")]
pub enum Payload {
	Version,
	Status,
	Submit(Transaction),
}

#[derive(Deserialize)]
pub struct Request {
	#[serde(flatten)]
	pub payload: Payload,
	pub request_id: Uuid,
}

#[derive(Serialize, Deserialize)]
pub struct Response<T> {
	pub request_id: Uuid,
	pub message: T,
}

impl<T> Response<T> {
	pub fn new(request_id: Uuid, message: T) -> Self {
		Response {
			request_id,
			message,
		}
	}
}

#[cfg(not(target_arch = "wasm32"))]
impl TryFrom<ws::Message> for Request {
	type Error = Report;

	fn try_from(value: ws::Message) -> Result<Self, Self::Error> {
		serde_json::from_slice(value.as_bytes()).wrap_err("Cannot parse json")
	}
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
	NotFound,
	BadRequest,
	InternalServerError,
}

#[derive(Serialize, Deserialize)]
pub struct Error {
	#[serde(skip_serializing_if = "Option::is_none")]
	pub request_id: Option<Uuid>,
	#[serde(skip)]
	pub cause: Option<Report>,
	pub error_code: ErrorCode,
	pub message: String,
}

impl Error {
	fn new(
		request_id: Option<Uuid>,
		cause: Option<Report>,
		error_code: ErrorCode,
		message: &str,
	) -> Self {
		Error {
			request_id,
			cause,
			error_code,
			message: message.to_string(),
		}
	}

	pub fn not_found() -> Self {
		Self::new(None, None, ErrorCode::NotFound, "Not Found")
	}

	pub fn internal_server_error(cause: Report) -> Self {
		Self::new(
			None,
			Some(cause),
			ErrorCode::InternalServerError,
			"Internal Server Error",
		)
	}

	pub fn bad_request_unknown(message: &str) -> Self {
		Self::new(None, None, ErrorCode::BadRequest, message)
	}

	pub fn bad_request(request_id: Uuid, message: &str) -> Self {
		Self::new(Some(request_id), None, ErrorCode::BadRequest, message)
	}

	#[cfg(not(target_arch = "wasm32"))]
	fn status(&self) -> StatusCode {
		match self.error_code {
			ErrorCode::NotFound => StatusCode::NOT_FOUND,
			ErrorCode::BadRequest => StatusCode::BAD_REQUEST,
			ErrorCode::InternalServerError => StatusCode::INTERNAL_SERVER_ERROR,
		}
	}
}

#[cfg(not(target_arch = "wasm32"))]
impl Reply for Error {
	fn into_response(self) -> warp::reply::Response {
		http::Response::builder()
			.status(self.status())
			.body(self.message.clone())
			.expect("Can create error response")
			.into_response()
	}
}

impl From<Error> for String {
	fn from(error: Error) -> Self {
		serde_json::to_string(&error).expect("Error is serializable")
	}
}

#[derive(Serialize, Deserialize, From)]
#[serde(tag = "topic", rename_all = "kebab-case")]
pub enum WsResponse {
	Version(Response<Version>),
	Status(Response<Status>),
	DataTransactionSubmitted(Response<SubmitResponse>),
}

#[derive(Serialize, Deserialize, From)]
#[serde(tag = "topic", rename_all = "kebab-case")]
pub enum WsError {
	Error(Error),
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use avail_rust::{
		avail::runtime_types::avail_core::data_lookup::compact::CompactDataLookup, H256,
	};
	use tokio::sync::mpsc;

	use crate::{
		api::types::{BlockStatus, Header, HeaderMessage, PublishMessage},
		data::{
			self, AchievedConfidenceKey, AchievedSyncConfidenceKey, Database, LatestHeaderKey,
			LatestSyncKey, VerifiedDataKey, VerifiedHeaderKey, VerifiedSyncDataKey,
			VerifiedSyncHeaderKey,
		},
		types::BlockRange,
		utils::{spawn_in_span, OptionalExtension},
	};

	use super::{
		block_status, Base64, ConfidenceMessage, DataField, DataMessage, DataTransaction, Digest,
		DigestItem, Subscription, Topic, WsClients,
	};

	fn subscription(topics: Vec<Topic>, fields: Vec<DataField>) -> Subscription {
		Subscription {
			topics: topics.into_iter().collect(),
			data_fields: fields.into_iter().collect(),
		}
	}

	fn header_verified() -> PublishMessage {
		PublishMessage::HeaderVerified(Box::new(HeaderMessage {
			block_number: 1,
			header: Header {
				hash: H256::default(),
				parent_hash: H256::default(),
				number: 1,
				state_root: H256::default(),
				extrinsics_root: H256::default(),
				extension: super::Extension {
					rows: 1,
					cols: 1,
					data_root: H256::default(),
					commitments: vec![],
					app_lookup: CompactDataLookup {
						size: 0,
						index: vec![],
					},
				},
				digest: Digest {
					logs: vec![DigestItem::RuntimeEnvironmentUpdated],
				},
				received_at: 0,
			},
		}))
	}

	fn confidence_achieved() -> PublishMessage {
		PublishMessage::ConfidenceAchieved(ConfidenceMessage {
			block_number: 1,
			confidence: Some(1.0),
		})
	}

	fn transaction_data() -> Option<Base64> {
		Some(Base64(vec![0, 1, 2, 3, 4]))
	}

	fn data_verified() -> PublishMessage {
		PublishMessage::DataVerified(DataMessage {
			block_number: 1,
			data_transactions: vec![DataTransaction {
				data: transaction_data(),
				extrinsic: transaction_data(),
			}],
		})
	}

	#[tokio::test]
	async fn clients_publish() {
		let clients = WsClients::default();
		let subscription_1 = subscription(
			vec![Topic::HeaderVerified, Topic::DataVerified],
			vec![DataField::Extrinsic],
		);
		let subscription_2 = subscription(
			vec![Topic::ConfidenceAchieved, Topic::DataVerified],
			vec![DataField::Data],
		);
		let (sender_1, mut receiver_1) = mpsc::unbounded_channel();
		let (sender_2, mut receiver_2) = mpsc::unbounded_channel();
		clients.subscribe("1", subscription_1).await;
		clients.subscribe("2", subscription_2).await;
		clients.set_sender("1", sender_1).await.unwrap();
		clients.set_sender("2", sender_2).await.unwrap();

		spawn_in_span(async move {
			for (topic, message) in [
				(Topic::HeaderVerified, header_verified()),
				(Topic::ConfidenceAchieved, confidence_achieved()),
				(Topic::DataVerified, data_verified()),
			] {
				let _ = clients.publish(&topic, message).await;
			}
		});

		tokio::select! {
			Some(message) = receiver_1.recv() => {
				let message: PublishMessage = serde_json::from_slice(message.unwrap().as_bytes()).unwrap();
				assert!(matches!(message, PublishMessage::HeaderVerified(_)));
			},
			_ = tokio::time::sleep(Duration::from_millis(100)) => panic!("Message isn't received"),
		};
		tokio::select! {
			Some(message) = receiver_2.recv() => {
				let message: PublishMessage = serde_json::from_slice(message.unwrap().as_bytes()).unwrap();
				assert!(matches!(message, PublishMessage::ConfidenceAchieved(_)));
			},
			_ = tokio::time::sleep(Duration::from_millis(100)) => panic!("Message isn't received"),
		};
		tokio::select! {
			Some(message) = receiver_1.recv() => {
				let message: PublishMessage = serde_json::from_slice(message.unwrap().as_bytes()).unwrap();
				let PublishMessage::DataVerified(data) = message else {
					panic!("Invalid message type");
				};
				assert!(data.data_transactions.iter().all(|tx| tx.data.is_none()));
				assert!(data.data_transactions.iter().all(|tx| tx.extrinsic == transaction_data()));
			},
			_ = tokio::time::sleep(Duration::from_millis(100)) => panic!("Message isn't received"),
		};
		tokio::select! {
			Some(message) = receiver_2.recv() => {
				let message: PublishMessage = serde_json::from_slice(message.unwrap().as_bytes()).unwrap();
				let PublishMessage::DataVerified(data) = message else {
					panic!("Invalid message type");
				};
				assert!(data.data_transactions.iter().all(|tx| tx.extrinsic.is_none()));
				assert!(data.data_transactions.iter().all(|tx| tx.data == transaction_data()));
			},
			_ = tokio::time::sleep(Duration::from_millis(100)) => panic!("Message isn't received"),
		};
	}

	struct ExtensionNone;

	impl OptionalExtension for ExtensionNone {
		fn option(&self) -> Option<&Self> {
			None
		}
	}

	struct ExtensionSome;

	impl OptionalExtension for ExtensionSome {
		fn option(&self) -> Option<&Self> {
			Some(self)
		}
	}

	#[test]
	fn block_status_none() {
		let db = data::MemoryDB::default();
		assert_eq!(block_status(&None, db.clone(), 1, ExtensionNone), None);
		db.put(LatestHeaderKey, 10);
		assert_ne!(block_status(&None, db.clone(), 1, ExtensionNone), None);
		assert_eq!(block_status(&None, db.clone(), 11, ExtensionNone), None);
	}

	#[test]
	fn block_status_unavailable() {
		let db = data::MemoryDB::default();
		let unavailable = Some(BlockStatus::Unavailable);
		db.put(LatestHeaderKey, 10);
		assert_eq!(
			block_status(&Some(1), db.clone(), 0, ExtensionNone),
			unavailable
		);
		assert_eq!(
			block_status(&Some(10), db.clone(), 0, ExtensionNone),
			unavailable
		);
		assert_eq!(
			block_status(&Some(10), db.clone(), 9, ExtensionNone),
			unavailable
		);
		assert_ne!(
			block_status(&Some(9), db.clone(), 9, ExtensionNone),
			unavailable
		);
	}

	#[test]
	fn block_status_pending() {
		let db = data::MemoryDB::default();
		db.put(LatestHeaderKey, 5);
		let pending = Some(BlockStatus::Pending);
		assert_eq!(
			block_status(&Some(0), db.clone(), 0, ExtensionSome),
			pending
		);
		assert_eq!(
			block_status(&Some(0), db.clone(), 1, ExtensionSome),
			pending
		);
		assert_eq!(
			block_status(&Some(0), db.clone(), 4, ExtensionSome),
			pending
		);
		assert_ne!(
			block_status(&Some(0), db.clone(), 5, ExtensionSome),
			pending
		);
	}

	#[test]
	fn block_status_verifying_header() {
		let db = data::MemoryDB::default();
		let verifying_header = Some(BlockStatus::VerifyingHeader);
		assert_eq!(
			block_status(&Some(0), db.clone(), 0, ExtensionSome),
			verifying_header
		);
		db.put(LatestHeaderKey, 1);
		assert_eq!(
			block_status(&Some(0), db.clone(), 1, ExtensionSome),
			verifying_header
		);
		db.put(LatestHeaderKey, 10);
		assert_eq!(
			block_status(&Some(0), db.clone(), 10, ExtensionSome),
			verifying_header
		);
		db.put(LatestHeaderKey, 11);
		assert_ne!(
			block_status(&Some(10), db.clone(), 10, ExtensionSome),
			verifying_header
		);

		let db = data::MemoryDB::default();
		db.put(LatestHeaderKey, 5);
		db.put(LatestSyncKey, 1);
		assert_eq!(
			block_status(&Some(1), db.clone(), 1, ExtensionSome),
			verifying_header
		);
		db.put(LatestSyncKey, 2);
		assert_eq!(
			block_status(&Some(1), db.clone(), 2, ExtensionSome),
			verifying_header
		);
		assert_ne!(
			block_status(&Some(1), db.clone(), 3, ExtensionSome),
			verifying_header
		);
	}

	#[test]
	fn block_status_verifying_confidence() {
		let mut db = data::MemoryDB::default();
		let verifying_confidence = Some(BlockStatus::VerifyingConfidence);
		db.put(LatestHeaderKey, 10);
		let mut verified_header = BlockRange::init(1);
		db.put(VerifiedHeaderKey, verified_header.clone());
		assert_eq!(
			block_status(&None, db.clone(), 1, ExtensionSome),
			verifying_confidence
		);
		let mut achieved_confidence = BlockRange::init(1);
		achieved_confidence.last = 4;
		db.put(AchievedConfidenceKey, achieved_confidence);
		verified_header.last = 5;
		db.put(VerifiedHeaderKey, verified_header);
		assert_eq!(
			block_status(&None, db.clone(), 5, ExtensionSome),
			verifying_confidence
		);
		assert_ne!(
			block_status(&None, db.clone(), 4, ExtensionSome),
			verifying_confidence
		);
		assert_ne!(
			block_status(&None, db.clone(), 6, ExtensionSome),
			verifying_confidence
		);

		db = data::MemoryDB::default();
		db.put(LatestHeaderKey, 10);

		let mut verified_sync_header = BlockRange::init(1);
		db.put(VerifiedSyncHeaderKey, verified_sync_header.clone());
		assert_eq!(
			block_status(&Some(1), db.clone(), 1, ExtensionSome),
			verifying_confidence
		);
		let mut achieved_sync_confidence = BlockRange::init(1);
		achieved_sync_confidence.last = 4;
		db.put(AchievedSyncConfidenceKey, achieved_sync_confidence);
		verified_sync_header.last = 5;
		db.put(VerifiedSyncHeaderKey, verified_sync_header);
		assert_eq!(
			block_status(&Some(1), db.clone(), 5, ExtensionSome),
			verifying_confidence
		);
		assert_ne!(
			block_status(&Some(1), db.clone(), 4, ExtensionSome),
			verifying_confidence
		);
		assert_ne!(
			block_status(&Some(1), db.clone(), 6, ExtensionSome),
			verifying_confidence
		);
	}

	#[test]
	fn block_status_verifying_data() {
		let mut db = data::MemoryDB::default();
		let verifying_data = Some(BlockStatus::VerifyingData);
		db.put(LatestHeaderKey, 10);
		let mut verified_header = BlockRange::init(1);
		let mut achieved_confidence = BlockRange::init(1);
		db.put(AchievedConfidenceKey, achieved_confidence.clone());
		db.put(VerifiedHeaderKey, verified_header.clone());
		assert_eq!(
			block_status(&None, db.clone(), 1, ExtensionSome),
			verifying_data
		);

		let mut verified_data = BlockRange::init(1);
		verified_data.last = 4;
		db.put(VerifiedDataKey, verified_data);
		verified_header.last = 5;
		db.put(VerifiedHeaderKey, verified_header);
		achieved_confidence.last = 5;
		db.put(AchievedConfidenceKey, achieved_confidence);
		assert_eq!(
			block_status(&None, db.clone(), 5, ExtensionSome),
			verifying_data
		);
		assert_ne!(
			block_status(&None, db.clone(), 4, ExtensionSome),
			verifying_data
		);
		assert_ne!(
			block_status(&None, db.clone(), 6, ExtensionSome),
			verifying_data
		);

		db = data::MemoryDB::default();
		db.put(LatestHeaderKey, 10);

		let mut verified_sync_header = BlockRange::init(1);
		db.put(VerifiedSyncHeaderKey, verified_sync_header.clone());
		let mut achieved_sync_confidence = BlockRange::init(1);
		db.put(AchievedSyncConfidenceKey, achieved_sync_confidence.clone());
		assert_eq!(
			block_status(&Some(1), db.clone(), 1, ExtensionSome),
			verifying_data
		);
		let mut verified_sync_data = BlockRange::init(1);
		verified_sync_data.last = 4;
		db.put(VerifiedSyncDataKey, verified_sync_data.clone());
		verified_sync_header.last = 5;
		db.put(VerifiedSyncHeaderKey, verified_sync_header);
		achieved_sync_confidence.last = 5;
		db.put(AchievedSyncConfidenceKey, achieved_sync_confidence);
		assert_eq!(
			block_status(&Some(1), db.clone(), 5, ExtensionSome),
			verifying_data
		);
		assert_ne!(
			block_status(&Some(1), db.clone(), 4, ExtensionSome),
			verifying_data
		);
		assert_ne!(
			block_status(&Some(1), db.clone(), 6, ExtensionSome),
			verifying_data
		);
	}

	#[test]
	fn block_status_finished() {
		let mut db = data::MemoryDB::default();
		let finished = Some(BlockStatus::Finished);
		db.put(LatestHeaderKey, 10);
		let mut verified_header = BlockRange::init(1);
		db.put(VerifiedHeaderKey, verified_header.clone());
		let mut verified_data = BlockRange::init(1);
		db.put(VerifiedDataKey, verified_data.clone());
		assert_eq!(block_status(&None, db.clone(), 1, ExtensionSome), finished);
		verified_header.last = 5;
		db.put(VerifiedHeaderKey, verified_header);
		verified_data.last = 5;
		db.put(VerifiedDataKey, verified_data);
		assert_eq!(block_status(&None, db.clone(), 4, ExtensionSome), finished);
		assert_eq!(block_status(&None, db.clone(), 5, ExtensionSome), finished);
		assert_ne!(block_status(&None, db.clone(), 6, ExtensionSome), finished);

		db = data::MemoryDB::default();
		db.put(LatestHeaderKey, 10);

		let mut verified_sync_header = BlockRange::init(1);
		db.put(VerifiedSyncHeaderKey, verified_sync_header.clone());
		let mut verified_sync_data = BlockRange::init(1);
		db.put(VerifiedSyncDataKey, verified_sync_data.clone());
		assert_eq!(
			block_status(&Some(1), db.clone(), 1, ExtensionSome),
			finished
		);
		verified_sync_header.last = 5;
		db.put(VerifiedSyncHeaderKey, verified_sync_header);
		verified_sync_data.last = 5;
		db.put(VerifiedSyncDataKey, verified_sync_data);
		assert_eq!(
			block_status(&Some(1), db.clone(), 4, ExtensionSome),
			finished
		);
		assert_eq!(
			block_status(&Some(1), db.clone(), 5, ExtensionSome),
			finished
		);
		assert_ne!(
			block_status(&Some(1), db.clone(), 6, ExtensionSome),
			finished
		);
	}
}
