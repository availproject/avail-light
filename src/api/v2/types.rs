use avail_subxt::api::runtime_types::{
	avail_core::{data_lookup::compact::CompactDataLookup, header::extension::HeaderExtension},
	bounded_collections::bounded_vec::BoundedVec,
};
use base64::{engine::general_purpose, DecodeError, Engine};
use codec::Encode;
use color_eyre::{
	eyre::{eyre, WrapErr},
	Report, Result,
};
use derive_more::From;
use hyper::{http, StatusCode};
use kate_recovery::{com::AppData, commitments, config, matrix::Partition};
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use sp_core::{blake2_256, H256};
use std::{
	collections::{HashMap, HashSet},
	sync::Arc,
};
use tokio::sync::{mpsc::UnboundedSender, RwLock};
use uuid::Uuid;
use warp::{
	ws::{self, Message},
	Reply,
};

use crate::{
	network::rpc::Event as RpcEvent,
	types::{
		self, block_matrix_partition_format, BlockVerified, OptionBlockRange, RuntimeConfig, State,
	},
	utils::decode_app_data,
};

#[derive(Debug)]
pub struct InternalServerError {}

impl warp::reject::Reject for InternalServerError {}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Version {
	pub version: String,
	pub network_version: String,
}

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

impl From<&types::BlockRange> for BlockRange {
	fn from(value: &types::BlockRange) -> Self {
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
	#[serde(
		skip_serializing_if = "Option::is_none",
		with = "block_matrix_partition_format"
	)]
	pub partition: Option<Partition>,
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

impl Reply for SubmitResponse {
	fn into_response(self) -> warp::reply::Response {
		warp::reply::json(&self).into_response()
	}
}

impl Status {
	pub fn new(config: &RuntimeConfig, state: &State) -> Self {
		let historical_sync = state.synced.map(|synced| HistoricalSync {
			synced,
			available: state.sync_confidence_achieved.as_ref().map(From::from),
			app_data: state.sync_data_verified.as_ref().map(From::from),
		});

		let blocks = Blocks {
			latest: state.latest,
			available: state.confidence_achieved.as_ref().map(From::from),
			app_data: state.data_verified.as_ref().map(From::from),
			historical_sync,
		};

		let node = state.connected_node.clone();

		Status {
			modes: config.into(),
			app_id: config.app_id,
			genesis_hash: format!("{:?}", node.genesis_hash),
			network: node.network(),
			blocks,
			partition: config.block_matrix_partition,
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

impl From<&RuntimeConfig> for Vec<Mode> {
	fn from(value: &RuntimeConfig) -> Self {
		let mut result: Vec<Mode> = vec![];
		result.push(Mode::Light);
		if value.app_id.is_some() {
			result.push(Mode::App);
		}
		if value.block_matrix_partition.is_some() {
			result.push(Mode::Partition)
		}
		result
	}
}

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
	Finished,
}

pub fn block_status(
	sync_start_block: &Option<u32>,
	state: &State,
	block_number: u32,
) -> Option<BlockStatus> {
	if block_number > state.latest {
		return None;
	}

	let first_block = state.header_verified.first().unwrap_or(state.latest);
	let first_sync_block = sync_start_block.unwrap_or(first_block);

	if block_number < first_sync_block {
		return Some(BlockStatus::Unavailable);
	}

	if block_number < first_block {
		if state.sync_data_verified.contains(block_number) {
			return Some(BlockStatus::Finished);
		}
		if state.sync_confidence_achieved.contains(block_number) {
			return Some(BlockStatus::VerifyingData);
		}
		if state.sync_header_verified.contains(block_number) {
			return Some(BlockStatus::VerifyingConfidence);
		}
		let is_sync_latest = state.sync_latest.map(|latest| block_number == latest);
		if is_sync_latest.unwrap_or(false) {
			return Some(BlockStatus::VerifyingHeader);
		}
	} else {
		if state.data_verified.contains(block_number) {
			return Some(BlockStatus::Finished);
		}
		if state.confidence_achieved.contains(block_number) {
			return Some(BlockStatus::VerifyingData);
		}
		if state.header_verified.contains(block_number) {
			return Some(BlockStatus::VerifyingConfidence);
		}
		if state.latest == block_number {
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

impl Reply for Block {
	fn into_response(self) -> warp::reply::Response {
		warp::reply::json(&self).into_response()
	}
}

impl TryFrom<avail_subxt::primitives::Header> for HeaderMessage {
	type Error = Report;

	fn try_from(header: avail_subxt::primitives::Header) -> Result<Self, Self::Error> {
		let header: Header = header.try_into()?;
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
}

impl Reply for Header {
	fn into_response(self) -> warp::reply::Response {
		warp::reply::json(&self).into_response()
	}
}

#[derive(Debug, Clone)]
struct Commitment([u8; config::COMMITMENT_SIZE]);

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
		const LEN: usize = (config::COMMITMENT_SIZE * HEX_ENCODED_BYTE_LEN) + PREFIX_0X_LEN;

		let s = String::deserialize(deserializer)?;

		if !s.starts_with("0x") || s.len() != LEN {
			let message = "Expected a hex string of correct length with 0x prefix";
			return Err(de::Error::custom(message));
		}

		let decoded = hex::decode(&s[2..]).map_err(de::Error::custom)?;
		let decoded_len = decoded.len();
		let bytes: [u8; config::COMMITMENT_SIZE] = decoded
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

impl TryFrom<avail_subxt::primitives::Header> for Header {
	type Error = Report;

	fn try_from(header: avail_subxt::primitives::Header) -> Result<Self> {
		Ok(Header {
			hash: Encode::using_encoded(&header, blake2_256).into(),
			parent_hash: header.parent_hash,
			number: header.number,
			state_root: header.state_root,
			extrinsics_root: header.extrinsics_root,
			extension: header.extension.try_into()?,
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

impl TryFrom<RpcEvent> for PublishMessage {
	type Error = Report;

	fn try_from(value: RpcEvent) -> Result<Self, Self::Error> {
		match value {
			RpcEvent::HeaderUpdate { header, .. } => header
				.try_into()
				.map(Box::new)
				.map(PublishMessage::HeaderVerified),
		}
	}
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ConfidenceMessage {
	block_number: u32,
	#[serde(skip_serializing_if = "Option::is_none")]
	confidence: Option<f64>,
}

impl TryFrom<BlockVerified> for PublishMessage {
	type Error = Report;

	fn try_from(value: BlockVerified) -> Result<Self, Self::Error> {
		Ok(PublishMessage::ConfidenceAchieved(ConfidenceMessage {
			block_number: value.block_num,
			confidence: value.confidence,
		}))
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

impl TryFrom<(u32, AppData)> for PublishMessage {
	type Error = Report;

	fn try_from((block_number, app_data): (u32, AppData)) -> Result<Self, Self::Error> {
		let data_transactions = app_data
			.into_iter()
			.map(TryFrom::try_from)
			.collect::<Result<Vec<_>>>()?;
		Ok(PublishMessage::DataVerified(DataMessage {
			block_number,
			data_transactions,
		}))
	}
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(tag = "topic", content = "message", rename_all = "kebab-case")]
pub enum PublishMessage {
	HeaderVerified(Box<HeaderMessage>),
	ConfidenceAchieved(ConfidenceMessage),
	DataVerified(DataMessage),
}

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

impl TryFrom<PublishMessage> for Message {
	type Error = Report;
	fn try_from(value: PublishMessage) -> Result<Self, Self::Error> {
		serde_json::to_string(&value)
			.map(ws::Message::text)
			.wrap_err("Cannot serialize publish message")
	}
}

pub type Sender = UnboundedSender<Result<ws::Message, warp::Error>>;

pub struct WsClient {
	pub subscription: Subscription,
	pub sender: Option<Sender>,
}

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

#[derive(Clone)]
pub struct WsClients(pub Arc<RwLock<HashMap<String, WsClient>>>);

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

impl Default for WsClients {
	fn default() -> Self {
		Self(Arc::new(RwLock::new(HashMap::new())))
	}
}

#[derive(Serialize, Deserialize)]
pub struct SubscriptionId {
	pub subscription_id: String,
}

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

	fn status(&self) -> StatusCode {
		match self.error_code {
			ErrorCode::NotFound => StatusCode::NOT_FOUND,
			ErrorCode::BadRequest => StatusCode::BAD_REQUEST,
			ErrorCode::InternalServerError => StatusCode::INTERNAL_SERVER_ERROR,
		}
	}
}

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

	use avail_subxt::api::runtime_types::avail_core::data_lookup::compact::CompactDataLookup;
	use sp_core::H256;
	use tokio::sync::mpsc;

	use crate::{
		api::v2::types::{BlockStatus, Header, HeaderMessage, PublishMessage},
		types::{OptionBlockRange, State},
	};

	use super::{
		block_status, Base64, ConfidenceMessage, DataField, DataMessage, DataTransaction,
		Subscription, Topic, WsClients,
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

		tokio::task::spawn(async move {
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

	#[test]
	fn block_status_none() {
		let mut state = State::default();
		assert_eq!(block_status(&None, &state, 1), None);
		state.latest = 10;
		assert_ne!(block_status(&None, &state, 1), None);
		assert_eq!(block_status(&None, &state, 11), None);
	}

	#[test]
	fn block_status_unavailable() {
		let state = State {
			latest: 10,
			..Default::default()
		};
		let unavailable = Some(BlockStatus::Unavailable);
		assert_eq!(block_status(&Some(1), &state, 0), unavailable);
		assert_eq!(block_status(&Some(10), &state, 0), unavailable);
		assert_eq!(block_status(&Some(10), &state, 9), unavailable);
		assert_ne!(block_status(&Some(9), &state, 9), unavailable);
	}

	#[test]
	fn block_status_pending() {
		let state = State {
			latest: 5,
			..Default::default()
		};
		let pending = Some(BlockStatus::Pending);
		assert_eq!(block_status(&Some(0), &state, 0), pending);
		assert_eq!(block_status(&Some(0), &state, 1), pending);
		assert_eq!(block_status(&Some(0), &state, 4), pending);
		assert_ne!(block_status(&Some(0), &state, 5), pending);
	}

	#[test]
	fn block_status_verifying_header() {
		let mut state = State::default();
		let verifying_header = Some(BlockStatus::VerifyingHeader);
		assert_eq!(block_status(&Some(0), &state, 0), verifying_header);
		state.latest = 1;
		assert_eq!(block_status(&Some(0), &state, 1), verifying_header);
		state.latest = 10;
		assert_eq!(block_status(&Some(0), &state, 10), verifying_header);
		state.latest = 11;
		assert_ne!(block_status(&Some(10), &state, 10), verifying_header);

		let mut state = State {
			latest: 5,
			sync_latest: Some(1),
			..Default::default()
		};
		assert_eq!(block_status(&Some(1), &state, 1), verifying_header);
		state.sync_latest = Some(2);
		assert_eq!(block_status(&Some(1), &state, 2), verifying_header);
		assert_ne!(block_status(&Some(1), &state, 3), verifying_header);
	}

	#[test]
	fn block_status_verifying_confidence() {
		let mut state = State::default();
		let verifying_confidence = Some(BlockStatus::VerifyingConfidence);
		state.latest = 10;
		state.header_verified.set(1);
		assert_eq!(block_status(&None, &state, 1), verifying_confidence);
		state.confidence_achieved.set(1);
		state.header_verified.set(5);
		state.confidence_achieved.set(4);
		assert_eq!(block_status(&None, &state, 5), verifying_confidence);
		assert_ne!(block_status(&None, &state, 4), verifying_confidence);
		assert_ne!(block_status(&None, &state, 6), verifying_confidence);

		let mut state = State {
			latest: 10,
			..Default::default()
		};
		state.sync_header_verified.set(1);
		assert_eq!(block_status(&Some(1), &state, 1), verifying_confidence);
		state.sync_confidence_achieved.set(1);
		state.sync_header_verified.set(5);
		state.sync_confidence_achieved.set(4);
		assert_eq!(block_status(&Some(1), &state, 5), verifying_confidence);
		assert_ne!(block_status(&Some(1), &state, 4), verifying_confidence);
		assert_ne!(block_status(&Some(1), &state, 6), verifying_confidence);
	}

	#[test]
	fn block_status_verifying_data() {
		let mut state = State::default();
		let verifying_data = Some(BlockStatus::VerifyingData);
		state.latest = 10;
		state.header_verified.set(1);
		state.confidence_achieved.set(1);
		assert_eq!(block_status(&None, &state, 1), verifying_data);
		state.data_verified.set(1);
		state.header_verified.set(5);
		state.confidence_achieved.set(5);
		state.data_verified.set(4);
		assert_eq!(block_status(&None, &state, 5), verifying_data);
		assert_ne!(block_status(&None, &state, 4), verifying_data);
		assert_ne!(block_status(&None, &state, 6), verifying_data);

		let mut state = State {
			latest: 10,
			..Default::default()
		};
		state.sync_header_verified.set(1);
		state.sync_confidence_achieved.set(1);
		assert_eq!(block_status(&Some(1), &state, 1), verifying_data);
		state.sync_data_verified.set(1);
		state.sync_header_verified.set(5);
		state.sync_confidence_achieved.set(5);
		state.sync_data_verified.set(4);
		assert_eq!(block_status(&Some(1), &state, 5), verifying_data);
		assert_ne!(block_status(&Some(1), &state, 4), verifying_data);
		assert_ne!(block_status(&Some(1), &state, 6), verifying_data);
	}

	#[test]
	fn block_status_finished() {
		let mut state = State::default();
		let finished = Some(BlockStatus::Finished);
		state.latest = 10;
		state.header_verified.set(1);
		state.data_verified.set(1);
		assert_eq!(block_status(&None, &state, 1), finished);
		state.header_verified.set(5);
		state.data_verified.set(5);
		assert_eq!(block_status(&None, &state, 4), finished);
		assert_eq!(block_status(&None, &state, 5), finished);
		assert_ne!(block_status(&None, &state, 6), finished);

		let mut state = State {
			latest: 10,
			..Default::default()
		};
		state.sync_header_verified.set(1);
		state.sync_data_verified.set(1);
		assert_eq!(block_status(&Some(1), &state, 1), finished);
		state.sync_header_verified.set(5);
		state.sync_data_verified.set(5);
		assert_eq!(block_status(&Some(1), &state, 4), finished);
		assert_eq!(block_status(&Some(1), &state, 5), finished);
		assert_ne!(block_status(&Some(1), &state, 6), finished);
	}
}
