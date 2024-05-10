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
	pub multiaddresses: Vec<String>,
}

impl Reply for MultiaddrResponse {
	fn into_response(self) -> warp::reply::Response {
		warp::reply::json(&self).into_response()
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteMultiaddrResponse {
	pub peer_id: String,
	pub multiaddress: String,
	pub established_in: String,
	pub num_established: u32,
}

impl Reply for RemoteMultiaddrResponse {
	fn into_response(self) -> warp::reply::Response {
		warp::reply::json(&self).into_response()
	}
}

impl P2PClient {
	pub async fn get_local_multiaddress(&self) -> Result<MultiaddrResponse> {
		let multiaddresses = self.client.get_local_multiaddresses().await?;
		Ok(MultiaddrResponse { multiaddresses })
	}

	pub async fn get_peer_multiaddress(&self, peer_id: String) -> Result<RemoteMultiaddrResponse> {
		let peer_id = PeerId::from_str(&peer_id)?;
		let res = self.client.dial_peer(peer_id, None).await?;

		Ok(RemoteMultiaddrResponse {
			peer_id: res.peer_id.to_string(),
			multiaddress: res.endpoint.get_remote_address().to_string(),
			established_in: res.established_in.as_secs().to_string(),
			num_established: res.num_established,
		})
	}
}
