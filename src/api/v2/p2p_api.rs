use color_eyre::Result;
use serde::{Deserialize, Serialize};
use warp::reply::Reply;

use crate::network::p2p;

#[derive(Clone)]
pub struct P2PClient {
	pub client: p2p::Client,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MultiaddrResponse {
	pub multiaddresses: Vec<String>,
}

impl Reply for MultiaddrResponse {
	fn into_response(self) -> warp::reply::Response {
		warp::reply::json(&self).into_response()
	}
}

impl P2PClient {
	pub async fn get_local_multiaddress(&self) -> Result<MultiaddrResponse> {
		let multiaddresses = self.client.get_local_multiaddresses().await?;
		Ok(MultiaddrResponse { multiaddresses })
	}
}
