use super::{
	transactions,
	types::{
		Client, Clients, Error, Status, Subscription, SubscriptionId, Transaction, TransactionHash,
		Version,
	},
	ws,
};
use crate::{
	api::v2::types::InternalServerError,
	rpc::Node,
	types::{RuntimeConfig, State},
};
use base64::{engine::general_purpose, Engine};
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

pub async fn submit(
	submitter: Arc<impl transactions::Submit>,
	transaction: Transaction,
) -> Result<TransactionHash, Error> {
	fn base64_decode(data: String) -> Result<Vec<u8>, Error> {
		general_purpose::STANDARD
			.decode(data)
			.map_err(|error| Error::bad_request(format!("Bad Request: {error}")))
	}

	if matches!(&transaction, Transaction::Data(_)) && !submitter.has_signer() {
		return Err(Error::not_found());
	};

	let transaction = match transaction {
		Transaction::Data(data) => transactions::Transaction::Data(base64_decode(data)?),
		Transaction::Extrinsic(data) => transactions::Transaction::Extrinsic(base64_decode(data)?),
	};

	submitter
		.submit(transaction)
		.await
		.map(|hash| format!("{hash:#x}"))
		.map(|hash| TransactionHash { hash })
		.map_err(|_| Error::internal_server_error())
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
			return Err(Error::internal_server_error());
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
