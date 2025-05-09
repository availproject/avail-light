use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tracing::info;
use warp::{reply::Reply, Filter};

use crate::{config::Addr, p2p};

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

async fn get_peer_info(p2p_client: p2p::Client) -> Result<PeerInfoResponse, String> {
	let local_info = p2p_client
		.get_local_peer_info()
		.await
		.map_err(|error| error.to_string())?;

	Ok(PeerInfoResponse {
		peer_id: local_info.peer_id,
		listeners: Listeners {
			local: local_info.local_listeners,
			external: local_info.external_listeners,
		},
	})
}

pub async fn run(addr: Addr, p2p_client: p2p::Client) {
	let health_route = warp::head()
		.or(warp::get())
		.and(warp::path("health"))
		.map(|_| warp::reply::with_status("", warp::http::StatusCode::OK));

	let p2p_local_info_route = warp::path!("p2p" / "local" / "info")
		.and(warp::get())
		.and(warp::any().map(move || p2p_client.clone()))
		.then(get_peer_info);

	info!("HTTP server running on http://{addr}. Health endpoint available at '/health'.");

	let socket_addr: SocketAddr = addr.try_into().unwrap();

	warp::serve(health_route.or(p2p_local_info_route))
		.run(socket_addr)
		.await;
}
