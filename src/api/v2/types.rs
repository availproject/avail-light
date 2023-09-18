use anyhow::Context;
use avail_subxt::api::runtime_types::sp_core::bounded::bounded_vec::BoundedVec;
use base64::{engine::general_purpose, DecodeError, Engine};
use derive_more::From;
use hyper::{http, StatusCode};
use kate_recovery::matrix::Partition;
use serde::{Deserialize, Serialize};
use sp_core::H256;
use std::{
	collections::{HashMap, HashSet},
	sync::Arc,
};
use tokio::sync::{mpsc::UnboundedSender, RwLock};
use uuid::Uuid;
use warp::{ws, Reply};

use crate::{
	rpc::Node,
	types::{self, block_matrix_partition_format, RuntimeConfig, State},
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

#[derive(Clone, Debug, Deserialize)]
#[serde(try_from = "String")]
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
	pub fn new(config: &RuntimeConfig, node: &Node, state: &State) -> Self {
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

#[derive(Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum Topics {
	HeaderVerified,
	ConfidenceAchieved,
	DataVerified,
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum DataFields {
	Data,
	Extrinsic,
}

#[derive(Serialize, Deserialize, PartialEq)]
pub struct Subscription {
	pub topics: HashSet<Topics>,
	pub data_fields: HashSet<DataFields>,
}

pub type Sender = UnboundedSender<Result<ws::Message, warp::Error>>;

pub struct Client {
	pub subscription: Subscription,
	pub sender: Option<Sender>,
}

impl Client {
	pub fn new(subscription: Subscription) -> Self {
		Client {
			subscription,
			sender: None,
		}
	}
}

pub type Clients = Arc<RwLock<HashMap<String, Client>>>;

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
	type Error = anyhow::Error;

	fn try_from(value: ws::Message) -> Result<Self, Self::Error> {
		serde_json::from_slice(value.as_bytes()).context("Cannot parse json")
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
	pub cause: Option<anyhow::Error>,
	pub error_code: ErrorCode,
	pub message: String,
}

impl Error {
	fn new(
		request_id: Option<Uuid>,
		cause: Option<anyhow::Error>,
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

	pub fn internal_server_error(cause: anyhow::Error) -> Self {
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

pub fn handle_result(result: Result<impl Reply, impl Reply>) -> impl Reply {
	match result {
		Ok(ok) => ok.into_response(),
		Err(err) => err.into_response(),
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
