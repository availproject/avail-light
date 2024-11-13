use libp2p::PeerId;
use warp::{Filter, Rejection, Reply};

pub mod p2p;
use p2p::{dial_external_peer, get_peer_info, get_peer_multiaddr};

use crate::{
	api::server::{handle_rejection, log_internal_server_error},
	network::p2p::Client,
};

fn p2p_local_info_route(
	p2p_client: Client,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v1" / "p2p" / "local" / "info")
		.and(warp::get())
		.and(warp::any().map(move || p2p_client.clone()))
		.then(get_peer_info)
		.map(log_internal_server_error)
}

fn p2p_peers_dial_route(
	p2p_client: Client,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v1" / "p2p" / "peers" / "dial")
		.and(warp::post())
		.and(warp::any().map(move || p2p_client.clone()))
		.and(warp::body::json())
		.then(dial_external_peer)
		.map(log_internal_server_error)
}

fn p2p_peer_multiaddr_route(
	p2p_client: Client,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v1" / "p2p" / "peers" / "multiaddress" / PeerId)
		.and(warp::get())
		.and(warp::any().map(move || p2p_client.clone()))
		.then(get_peer_multiaddr)
		.map(log_internal_server_error)
}

#[allow(clippy::too_many_arguments)]
pub fn routes(
	p2p_client: Client,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	p2p_local_info_route(p2p_client.clone())
		.or(p2p_peers_dial_route(p2p_client.clone()))
		.or(p2p_peer_multiaddr_route(p2p_client.clone()))
		.recover(handle_rejection)
}
