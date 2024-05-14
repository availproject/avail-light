use color_eyre::Result;
use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use warp::reply::Reply;

use crate::network::p2p;

use super::types::Error;

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
pub struct PeerIDResponse {
	pub peer_id: String,
}

impl Reply for PeerIDResponse {
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

pub async fn get_local_listeners(p2p_client: p2p::Client) -> Result<MultiaddrResponse, Error> {
	p2p_client
		.get_local_listeners()
		.await
		.map(|multiaddresses| MultiaddrResponse { multiaddresses })
		.map_err(Error::internal_server_error)
}

pub async fn get_external_addresses(p2p_client: p2p::Client) -> Result<MultiaddrResponse, Error> {
	p2p_client
		.get_external_addresses()
		.await
		.map(|multiaddresses| MultiaddrResponse { multiaddresses })
		.map_err(Error::internal_server_error)
}

pub async fn get_local_peer_id(p2p_client: p2p::Client) -> Result<PeerIDResponse, Error> {
	p2p_client
		.get_local_peer_id()
		.await
		.map(|peer_id| PeerIDResponse { peer_id })
		.map_err(Error::internal_server_error)
}

pub async fn get_peer_multiaddress(
	peer_id: PeerId,
	p2p_client: p2p::Client,
) -> Result<RemoteMultiaddrResponse, Error> {
	p2p_client
		.dial_peer(peer_id, None)
		.await
		.map(|connection_info| RemoteMultiaddrResponse {
			peer_id: connection_info.peer_id.to_string(),
			multiaddress: connection_info.endpoint.get_remote_address().to_string(),
			established_in: connection_info.established_in.as_secs().to_string(),
			num_established: connection_info.num_established,
		})
		.map_err(Error::internal_server_error)
}
