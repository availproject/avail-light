use super::{
	types::{Client, Clients, Status, Subscription, SubscriptionId, Version},
	ws,
};
use crate::{
	api::v2::types::InternalServerError,
	rpc::Node,
	types::{RuntimeConfig, State},
};
use hyper::StatusCode;
use std::{
	convert::Infallible,
	sync::{Arc, Mutex},
};
use tracing::info;
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
	config: RuntimeConfig,
	node: Node,
	state: Arc<Mutex<State>>,
) -> Result<impl Reply, Rejection> {
	if !clients.read().await.contains_key(&subscription_id) {
		return Err(warp::reject::not_found());
	}
	// Multiple connections to the same client are currently allowed
	Ok(ws.on_upgrade(move |web_socket| {
		ws::connect(
			subscription_id,
			web_socket,
			clients,
			version,
			config,
			node,
			state.clone(),
		)
	}))
}

pub async fn status(
	config: RuntimeConfig,
	node: Node,
	state: Arc<Mutex<State>>,
) -> Result<impl Reply, impl Reply> {
	let state = match state.lock() {
		Ok(state) => state,
		Err(error) => {
			info!("Cannot acquire lock for last_block: {error}");
			return Err(StatusCode::INTERNAL_SERVER_ERROR);
		},
	};

	Ok(Status::new(&config, &node, &state))
}

pub async fn handle_rejection(error: Rejection) -> Result<impl Reply, Rejection> {
	if error.find::<InternalServerError>().is_some() {
		return Ok(StatusCode::INTERNAL_SERVER_ERROR.into_response());
	}
	Err(error)
}
