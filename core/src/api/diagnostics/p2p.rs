use crate::{
	api::types::Error,
	data::{Database, DhtFetchedPercentageKey, DhtPutSuccessKey},
	network::p2p::{self, MultiAddressInfo},
};
use libp2p::{
	swarm::{dial_opts::PeerCondition, DialError},
	Multiaddr, PeerId,
};
use serde::{Deserialize, Serialize};
use warp::reply::Reply;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Listeners {
	pub local: Vec<String>,
	pub external: Vec<String>,
	pub public: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerInfoResponse {
	peer_id: String,
	listeners: Listeners,
	operation_mode: String,
	routing_table_peers_count: usize,
	routing_table_external_peers_count: usize,
}

impl Reply for PeerInfoResponse {
	fn into_response(self) -> warp::reply::Response {
		warp::reply::json(&self).into_response()
	}
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ExternalPeerDialError {
	pub error: String,
	pub description: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExternalPeerDialSuccess {
	pub peer_id: String,
	pub multiaddress: String,
	pub established_in: String,
	pub num_established: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExternalPeerDialResponse {
	pub dial_success: Option<ExternalPeerDialSuccess>,
	pub dial_error: Option<ExternalPeerDialError>,
}

impl Reply for ExternalPeerDialResponse {
	fn into_response(self) -> warp::reply::Response {
		warp::reply::json(&self).into_response()
	}
}

#[derive(Clone, Debug, Deserialize)]
pub struct PeerInfoQuery {
	pub peer_id: PeerId,
}

#[derive(Debug)]
pub struct MultiAddressResponse(MultiAddressInfo);

impl Reply for MultiAddressResponse {
	fn into_response(self) -> warp::reply::Response {
		warp::reply::json(&self.0).into_response()
	}
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExternalPeerMultiaddress {
	pub multiaddress: Multiaddr,
	pub peer_id: PeerId,
}

pub async fn get_peer_info(p2p_client: p2p::Client) -> Result<PeerInfoResponse, Error> {
	let local_info = p2p_client
		.get_local_peer_info()
		.await
		.map_err(Error::internal_server_error)?;

	let (routing_table_peers_count, routing_table_external_peers_count) = p2p_client
		.count_dht_entries()
		.await
		.map_err(Error::internal_server_error)?;

	Ok(PeerInfoResponse {
		peer_id: local_info.peer_id,
		operation_mode: local_info.operation_mode,
		listeners: Listeners {
			local: local_info.local_listeners,
			external: local_info.external_listeners,
			public: vec![],
		},
		routing_table_peers_count,
		routing_table_external_peers_count,
	})
}

pub async fn get_peer_multiaddr(
	peer_id: PeerId,
	p2p_client: p2p::Client,
) -> Result<MultiAddressResponse, Error> {
	let external_peer_info = p2p_client
		.get_external_peer_info(peer_id)
		.await
		.map_err(Error::internal_server_error)?;

	Ok(MultiAddressResponse(external_peer_info))
}

pub async fn dial_external_peer(
	p2p_client: p2p::Client,
	peer_address: ExternalPeerMultiaddress,
) -> Result<ExternalPeerDialResponse, Error> {
	p2p_client
		.dial_peer(
			peer_address.peer_id,
			vec![peer_address.multiaddress],
			PeerCondition::NotDialing,
		)
		.await
		.map(|connection_info| ExternalPeerDialResponse {
			dial_success: Some(ExternalPeerDialSuccess {
				peer_id: connection_info.peer_id.to_string(),
				multiaddress: connection_info.endpoint.get_remote_address().to_string(),
				established_in: connection_info.established_in.as_secs().to_string(),
				num_established: connection_info.num_established,
			}),
			dial_error: None,
		})
		.or_else(|err| {
			let Some(dial_error) = err.root_cause().downcast_ref::<DialError>() else {
				return Err(Error::internal_server_error(err));
			};
			match dial_error {
				DialError::LocalPeerId { .. } => {
					Err(Error::bad_request_unknown("Can't dial yourself!"))
				},
				DialError::NoAddresses => Err(Error::bad_request_unknown("Address not provided.")),
				DialError::DialPeerConditionFalse(_) => Err(Error::internal_server_error(err)),
				DialError::Aborted => Err(Error::internal_server_error(err)),
				DialError::WrongPeerId { obtained, .. } => {
					let peer_id = peer_address.peer_id;
					let message =
						"The peerID obtained on the connection is not matching the one provided";

					Ok(ExternalPeerDialResponse {
						dial_success: None,
						dial_error: Some(ExternalPeerDialError {
							error: "wrong-peer-id".to_string(),
							description: format!(
								"{message}. User provided peerID: {peer_id}. Observed peerID: {obtained}."
							),
						}),
					})
				},
				DialError::Denied { .. } => Err(Error::internal_server_error(err)),
				DialError::Transport(_) => {
					let message = "An error occurred while negotiating the transport protocol(s) on a connection";
					Ok(ExternalPeerDialResponse {
						dial_success: None,
						dial_error: Some(ExternalPeerDialError {
							error: "transport".to_string(),
							description: format!("{message}. Cause: {dial_error}"),
						}),
					})
				},
			}
		})
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum DhtMetricResponse {
	DhtFetched { dht_fetched_percentage: f64 },
	DhtPut { dht_put_success: f64 },
}

pub async fn get_dht_metric(
	metric: String,
	db: impl Database + Clone,
) -> Result<impl Reply, Error> {
	let response = match metric.as_str() {
		"fetch" => db
			.get(DhtFetchedPercentageKey)
			.map(|v| DhtMetricResponse::DhtFetched {
				dht_fetched_percentage: v,
			}),

		"put" => db
			.get(DhtPutSuccessKey)
			.map(|v| DhtMetricResponse::DhtPut { dht_put_success: v }),

		_ => None,
	};

	match response {
		Some(resp) => Ok(warp::reply::json(&resp)),
		None => Err(Error::not_found()),
	}
}
