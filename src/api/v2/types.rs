use serde::{Deserialize, Serialize};
use warp::Reply;

#[derive(Serialize, Deserialize)]
pub struct Version {
	pub version: String,
	pub network_version: String,
}

impl Reply for Version {
	fn into_response(self) -> warp::reply::Response {
		warp::reply::json(&self).into_response()
	}
}
