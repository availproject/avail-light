use warp::{Filter, Rejection, Reply};
use crate::api::server::{handle_rejection, log_internal_server_error};
use crate::network::p2p::Client;

pub mod p2p;
use p2p::{dial_external_peer, get_peer_multiaddr, get_peer_info};

fn p2p_local_info_route(
	p2p_client: Client,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("p2p" / "local" / "info")
		.and(warp::get())
		.and(warp::any().map(move || p2p_client.clone()))
		.then(get_peer_info)
		.map(log_internal_server_error)
}

fn p2p_peers_dial_route(
	p2p_client: Client,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("p2p" / "peers" / "dial")
		.and(warp::post())
		.and(warp::any().map(move || p2p_client.clone()))
		.and(warp::body::json())
		.then(dial_external_peer)
		.map(log_internal_server_error)
}

fn p2p_peer_multiaddr_route(
	p2p_client: Client,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("p2p" / "peers" / "get-multiaddress")
		.and(warp::post())
		.and(warp::any().map(move || p2p_client.clone()))
		.and(warp::body::json())
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
