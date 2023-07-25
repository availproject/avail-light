use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use std::{
	collections::{HashMap, HashSet},
	sync::Arc,
};
use tokio::sync::{mpsc::UnboundedSender, RwLock};
use warp::{ws, Reply};

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
	Raw,
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
		serde_json::from_slice(value.as_bytes()).map_err(|error| anyhow!("{error}"))
	}
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
	BadRequest,
}

#[derive(Serialize)]
pub struct Error {
	pub error_code: ErrorCode,
	pub message: String,
}

impl From<Error> for String {
	fn from(error: Error) -> Self {
		serde_json::to_string(&error).expect("Error is serializable")
	}
}
