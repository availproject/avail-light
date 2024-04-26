use std::str::FromStr;

use color_eyre::Result;
use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use warp::reply::Reply;

use crate::network::p2p;

#[derive(Clone)]
pub struct P2PClient {
	pub client: p2p::Client,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MultiaddrResponse {
	pub multiaddr: String,
}

impl Reply for MultiaddrResponse {
	fn into_response(self) -> warp::reply::Response {
		warp::reply::json(&self).into_response()
	}
}

impl P2PClient {
	pub async fn get_multiaddress(&self, peer_id: String) -> Result<MultiaddrResponse> {
		let peer_id = PeerId::from_str(peer_id.as_str())?;
		let _ = self.client.dial_peer(peer_id, None).await;
		Ok(MultiaddrResponse {
			multiaddr: "test1234".to_string(),
		})
	}
}
