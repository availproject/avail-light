use serde::{Deserialize, Serialize};
use std::{
	collections::{HashMap, HashSet},
	sync::Arc,
};
use tokio::sync::RwLock;
use warp::Reply;

#[derive(Serialize)]
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

pub struct Client {
	pub subscription: Subscription,
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
