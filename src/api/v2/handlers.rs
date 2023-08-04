use super::{
	types::{Client, Clients, Subscription, SubscriptionId, Version},
	ws,
};
use std::convert::Infallible;
use uuid::Uuid;
use warp::{ws::Ws, Rejection, Reply};

pub async fn subscriptions(
	subscription: Subscription,
	clients: Clients,
) -> Result<SubscriptionId, Infallible> {
	let subscription_id = Uuid::new_v4().to_string();
	let mut clients = clients.write().await;
	clients.insert(subscription_id.clone(), Client::new(subscription));
	Ok(SubscriptionId { subscription_id })
}

pub async fn ws(
	subscription_id: String,
	ws: Ws,
	clients: Clients,
	version: Version,
) -> Result<impl Reply, Rejection> {
	if !clients.read().await.contains_key(&subscription_id) {
		return Err(warp::reject::not_found());
	}
	// Multiple connections to the same client are currently allowed
	Ok(ws.on_upgrade(move |web_socket| ws::connect(subscription_id, web_socket, clients, version)))
}
