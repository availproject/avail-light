use crate::{
	api::v2::types::Error,
	network::p2p::{self, LocalInfo},
};
use libp2p::{swarm::DialError, Multiaddr, PeerId};
use serde::{Deserialize, Serialize};
use warp::reply::Reply;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Listeners {
	pub local: Vec<String>,
	pub external: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerInfoResponse {
	peer_id: String,
	listeners: Listeners,
}

impl Reply for PeerInfoResponse {
	fn into_response(self) -> warp::reply::Response {
		warp::reply::json(&self).into_response()
	}
}

impl From<LocalInfo> for PeerInfoResponse {
	fn from(value: LocalInfo) -> Self {
		PeerInfoResponse {
			peer_id: value.peer_id,
			listeners: Listeners {
				local: value.local_listeners,
				external: value.external_listeners,
			},
		}
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExternalPeerDialResponse {
	pub peer_id: String,
	pub multiaddress: String,
	pub established_in: String,
	pub num_established: u32,
}

impl Reply for ExternalPeerDialResponse {
	fn into_response(self) -> warp::reply::Response {
		warp::reply::json(&self).into_response()
	}
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExternalPeerMultiaddress {
	pub multiaddress: Multiaddr,
	pub peer_id: PeerId,
}

pub async fn get_peer_info(p2p_client: p2p::Client) -> Result<PeerInfoResponse, Error> {
	let local_info = p2p_client
		.get_local_info()
		.await
		.map_err(Error::internal_server_error)?;

	Ok(local_info.into())
}

pub async fn dial_external_peer(
	p2p_client: p2p::Client,
	peer_address: ExternalPeerMultiaddress,
) -> Result<ExternalPeerDialResponse, Error> {
	p2p_client
		.dial_peer(peer_address.peer_id, Some(peer_address.multiaddress))
		.await
		.map(|connection_info| ExternalPeerDialResponse {
			peer_id: connection_info.peer_id.to_string(),
			multiaddress: connection_info.endpoint.get_remote_address().to_string(),
			established_in: connection_info.established_in.as_secs().to_string(),
			num_established: connection_info.num_established,
		})
		.map_err(|err| {
			let root_err = err.root_cause();
			if let Some(dial_error) = root_err.downcast_ref::<DialError>() {
				match dial_error {
					DialError::LocalPeerId { .. } => {
						Error::bad_request_unknown("Can't dial yourself!")
					},
					DialError::NoAddresses => Error::bad_request_unknown("Address not provided."),
					DialError::DialPeerConditionFalse(_) => Error::internal_server_error(err),
					DialError::Aborted => Error::bad_request_unknown("Peer dial aborted."),
					DialError::WrongPeerId { obtained, endpoint } => {
						let peer_id = obtained.to_owned();
						let observed = endpoint.get_remote_address();
						let message = "The peerID obtained on the connection is not matching the one provided";
						Error::bad_request_unknown(&format!(
							"{message}. User provided peerID: {peer_id}. Observed: {observed}",
						))
					},
					DialError::Denied { cause } => {
						Error::bad_request_unknown(&format!("Connection denied. Reason: {cause}",))
					},
					DialError::Transport(_) => {
						let message = "An error occurred while negotiating the transport protocol(s) on a connection";
						Error::bad_request_unknown(&format!("{message}. Cause: {dial_error}"))
					},
				}
			} else {
				Error::internal_server_error(err)
			}
		})
}
