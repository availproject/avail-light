use libp2p::PeerId;
use serde_json;
use warp::Filter;

pub mod p2p;
use p2p::{dial_external_peer, get_peer_info, get_peer_multiaddr};

use crate::{
	api::routes::{with_boxed_reply, BoxedAnyFilter},
	network::p2p::Client,
};

/// Create routes for P2P diagnostics endpoints
pub fn routes(p2p_client: Client) -> BoxedAnyFilter {
	let client_clone1 = p2p_client.clone();
	let client_clone2 = p2p_client.clone();
	let client_clone3 = p2p_client;

	// Route for local peer info
	let local_info = warp::path!("v1" / "p2p" / "local" / "info")
		.and(warp::get())
		.and(warp::any().map(move || client_clone1.clone()))
		.then(|client| async move {
			match get_peer_info(client).await {
				Ok(info) => warp::reply::json(&info),
				Err(_) => warp::reply::json(&serde_json::json!({
					"error": "Failed to get peer info"
				})),
			}
		});

	// Route for dialing external peers
	let peers_dial = warp::path!("v1" / "p2p" / "peers" / "dial")
		.and(warp::post())
		.and(warp::any().map(move || client_clone2.clone()))
		.and(warp::body::json())
		.then(|client, body| async move {
			match dial_external_peer(client, body).await {
				Ok(_) => warp::reply::json(&serde_json::json!({
					"success": true,
					"message": "Successfully dialed peer"
				})),
				Err(_) => warp::reply::json(&serde_json::json!({
					"success": false,
					"message": "Failed to dial peer"
				})),
			}
		});

	// Route for getting peer multiaddress
	let peer_multiaddr = warp::path!("v1" / "p2p" / "peers" / "multiaddress" / PeerId)
		.and(warp::get())
		.and(warp::any().map(move || client_clone3.clone()))
		.then(|peer_id, client| async move {
			match get_peer_multiaddr(peer_id, client).await {
				Ok(_) => warp::reply::json(&serde_json::json!({
					"peer_id": peer_id.to_string(),
					"message": "Found peer multiaddresses"
				})),
				Err(_) => warp::reply::json(&serde_json::json!({
					"peer_id": peer_id.to_string(),
					"error": "Failed to get peer multiaddresses"
				})),
			}
		});

	// Combine all routes and box the result
	with_boxed_reply(local_info.or(peers_dial).or(peer_multiaddr))
}
