use anyhow::Context;
use kate_recovery::matrix::Partition;
use serde::{Deserialize, Serialize};
use std::{
	collections::{HashMap, HashSet},
	sync::Arc,
};
use tokio::sync::{mpsc::UnboundedSender, RwLock};
use warp::{ws, Reply};

use crate::{
	rpc::Node,
	types::{self, block_matrix_partition_format, RuntimeConfig, State},
};

#[derive(Debug)]
pub struct InternalServerError {}

impl warp::reject::Reject for InternalServerError {}

#[derive(Serialize, Clone)]
pub struct Version {
	pub version: String,
	pub network_version: String,
}

impl Reply for Version {
	fn into_response(self) -> warp::reply::Response {
		warp::reply::json(&self).into_response()
	}
}

#[derive(Serialize)]
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

#[derive(Serialize)]
pub struct HistoricalSync {
	pub synced: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub available: Option<BlockRange>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub app_data: Option<BlockRange>,
}

#[derive(Serialize)]
pub struct Blocks {
	pub latest: u32,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub available: Option<BlockRange>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub app_data: Option<BlockRange>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub historical_sync: Option<HistoricalSync>,
}

#[derive(Serialize)]
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

#[derive(Serialize)]
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
#[serde(rename_all = "kebab-case")]
pub enum RequestType {
	Version,
	Status,
}

#[derive(Deserialize)]
pub struct Request {
	#[serde(rename = "type")]
	pub request_type: RequestType,
	pub request_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResponseTopic {
	Version,
	Status,
}

#[derive(Serialize)]
pub struct Response<T> {
	pub topic: ResponseTopic,
	pub request_id: String,
	pub message: T,
}

impl TryFrom<ws::Message> for Request {
	type Error = anyhow::Error;

	fn try_from(value: ws::Message) -> Result<Self, Self::Error> {
		serde_json::from_slice(value.as_bytes()).context("Failed to parse json request")
	}
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
	BadRequest,
	InternalServerError,
}

#[derive(Serialize)]
pub struct Error {
	pub error_code: ErrorCode,
	pub message: String,
}

impl Error {
	pub fn internal_server_error() -> Self {
		Error {
			error_code: ErrorCode::InternalServerError,
			message: "Internal Server Error".to_string(),
		}
	}

	pub fn bad_request(message: String) -> Self {
		Error {
			error_code: ErrorCode::BadRequest,
			message,
		}
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
