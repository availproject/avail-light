use libp2p::PeerId;
use warp::{Filter, Rejection, Reply};

pub mod p2p;
use p2p::{dial_external_peer, get_peer_info, get_peer_multiaddr};

use crate::{
	api::{
		diagnostics::p2p::get_dht_metric,
		server::{handle_rejection, log_internal_server_error_v1},
	},
	data::Database,
	network::p2p::Client,
};

// Define a type alias for the filter return type to ensure compatibility between routes and p2p_disabled_routes
type P2PFilter = warp::filters::BoxedFilter<(Box<dyn Reply>,)>;

fn p2p_local_info_route(
	p2p_client: Client,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v1" / "p2p" / "local" / "info")
		.and(warp::get())
		.and(warp::any().map(move || p2p_client.clone()))
		.then(get_peer_info)
		.map(log_internal_server_error_v1)
}

fn p2p_peers_dial_route(
	p2p_client: Client,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v1" / "p2p" / "peers" / "dial")
		.and(warp::post())
		.and(warp::any().map(move || p2p_client.clone()))
		.and(warp::body::json())
		.then(dial_external_peer)
		.map(log_internal_server_error_v1)
}

fn p2p_peer_multiaddr_route(
	p2p_client: Client,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v1" / "p2p" / "peers" / "multiaddress" / PeerId)
		.and(warp::get())
		.and(warp::any().map(move || p2p_client.clone()))
		.then(get_peer_multiaddr)
		.map(log_internal_server_error_v1)
}

fn p2p_dht_metrics_route(
	db: impl Database + Clone + Send + Sync + 'static,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v1" / "p2p" / "dht" / String)
		.and(warp::get())
		.and(warp::any().map(move || db.clone()))
		.then(get_dht_metric)
		.map(log_internal_server_error_v1)
}

#[allow(clippy::too_many_arguments)]
pub fn routes(p2p_client: Client, db: impl Database + Clone + Send + Sync + 'static) -> P2PFilter {
	p2p_local_info_route(p2p_client.clone())
		.or(p2p_peers_dial_route(p2p_client.clone()))
		.or(p2p_peer_multiaddr_route(p2p_client.clone()))
		.or(p2p_dht_metrics_route(db.clone()))
		.recover(handle_rejection)
		.boxed()
		.map(|reply| Box::new(reply) as Box<dyn Reply>)
		.boxed()
}

// Helper function to create a route for when P2P is disabled
pub fn p2p_disabled_routes() -> P2PFilter {
	warp::path("v1")
		.and(warp::path("p2p"))
		.and(warp::path::tail())
		.map(|_| {
			warp::reply::with_status(
				"P2P functionality is disabled in this client",
				warp::http::StatusCode::NOT_FOUND,
			)
		})
		.boxed()
		.map(|reply| Box::new(reply) as Box<dyn Reply>)
		.boxed()
}
